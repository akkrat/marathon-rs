//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDeviceDisposeTest.kt
mod simulator_support;
mod support;
use marathon_apple::cmd::cancelled;
use marathon_config::vendor::{GrantLifecycle, IosPermission, LifecycleAction};
use marathon_core::{
    error::VendorError,
    executor::shared::{SharedDeferred, Signal},
    vendor::{Device, VendorContext},
};
use simulator_support::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
struct PermissionGate {
    entered: Arc<SharedDeferred<()>>,
    release: Arc<SharedDeferred<()>>,
    finished: Arc<AtomicBool>,
    lifecycle: Arc<AtomicUsize>,
    raced: Arc<AtomicBool>,
}
impl PermissionGate {
    fn attach(h: &Harness) -> Self {
        let entered = SharedDeferred::new();
        let release = SharedDeferred::new();
        let finished = Arc::new(AtomicBool::new(false));
        let lifecycle = Arc::new(AtomicUsize::new(0));
        let raced = Arc::new(AtomicBool::new(false));
        let (start, gate, done, count, race, boot) = (
            entered.clone(),
            release.clone(),
            finished.clone(),
            lifecycle.clone(),
            raced.clone(),
            h.boot.clone(),
        );
        *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
            let grant = cmd.get(2).is_some_and(|s| s == "privacy");
            let terminate = cmd.get(2).is_some_and(|s| s.starts_with("ps aux"));
            let (start, gate, done, count, race) =
                (start.clone(), gate.clone(), done.clone(), count.clone(), race.clone());
            let reply = boot.answer(&cmd);
            Box::pin(async move {
                if grant {
                    start.complete(Ok(()));
                    gate.wait().await?;
                    done.store(true, Ordering::SeqCst);
                }
                if terminate {
                    count.fetch_add(1, Ordering::SeqCst);
                    if !done.load(Ordering::SeqCst) {
                        race.store(true, Ordering::SeqCst);
                    }
                }
                Ok(reply)
            })
        });
        Self {
            entered,
            release,
            finished,
            lifecycle,
            raced,
        }
    }
}
fn harness(timeout: i64) -> TestResult<Harness> {
    let mut h = Harness::new(|v| {
        v.permissions.lifecycle = GrantLifecycle::BeforeEachBatch;
        v.permissions.grant = vec![IosPermission::Calendar];
        v.lifecycle_configuration.on_dispose = vec![LifecycleAction::Terminate];
    })?;
    h.services.timeouts.cancelled_batch_millis = timeout;
    Ok(h)
}
#[tokio::test(start_paused = true)]
async fn disposal_runs_the_on_dispose_lifecycle_only_after_the_running_batch_has_finished() -> TestResult {
    let h = harness(5000)?;
    let (d, _) = prepared(&h).await?;
    let gate = PermissionGate::attach(&h);
    let batch = spawn_batch(&h, d.clone(), h.services.context.clone());
    gate.entered.wait().await.unwrap();
    let disposal = d.dispose();
    tokio::pin!(disposal);
    assert!(futures_util::poll!(disposal.as_mut()).is_pending());
    assert_eq!(gate.lifecycle.load(Ordering::SeqCst), 0);
    assert!(!d.disposed());
    gate.release.complete(Ok(()));
    disposal.await.unwrap();
    assert_eq!(gate.lifecycle.load(Ordering::SeqCst), 1);
    assert!(!gate.raced.load(Ordering::SeqCst));
    assert!(matches!(batch.await?.0, Err(VendorError::DeviceLost(_))));
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_cancelled_batch_that_has_not_finished_in_time_takes_the_simulator_out_of_the_run_and_disposal_still_waits_for_it()
-> TestResult {
    let h = harness(1000)?;
    h.services
        .logging
        .registry
        .lock()
        .unwrap()
        .set_level("marathon_ios::apple_simulator_device", marathon_logging::Level::WARN);
    let (d, _) = prepared(&h).await?;
    let gate = PermissionGate::attach(&h);
    let signal = Signal::new();
    let batch = spawn_batch(
        &h,
        d.clone(),
        VendorContext {
            cancellation: signal.clone(),
            ..h.services.context.clone()
        },
    );
    gate.entered.wait().await.unwrap();
    signal.cancel(cancelled("caller"));
    assert!(matches!(batch.await?.0, Err(VendorError::Cancelled(_))));
    assert!(
        h.logs
            .lock()
            .unwrap()
            .iter()
            .any(|(_, level, message, _)| *level == marathon_logging::Level::WARN
                && message.contains("has not finished within 1 s, taking the device out of the run"))
    );
    assert!(d.retired());
    assert!(!gate.finished.load(Ordering::SeqCst));
    let disposal = d.dispose();
    tokio::pin!(disposal);
    assert!(futures_util::poll!(disposal.as_mut()).is_pending());
    assert_eq!(gate.lifecycle.load(Ordering::SeqCst), 0);
    gate.release.complete(Ok(()));
    disposal.await.unwrap();
    assert_eq!(gate.lifecycle.load(Ordering::SeqCst), 1);
    assert!(!gate.raced.load(Ordering::SeqCst));
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
/// D93, in place of the Kotlin `disposal stops waiting for a batch that has not finished in time`, which ran the
/// on-dispose lifecycle while the batch was still blocked (parity row `n/a`, `x92: D93`). Disposal still stops waiting at
/// its bound, but a batch that has not ended by then lets no lifecycle run: one WARN names the simulator, its commands are
/// revoked and it is quarantined, so its lease is not released. The batch, let go later, sends nothing more.
#[tokio::test(start_paused = true)]
async fn disposal_that_cannot_confirm_the_batch_ended_skips_the_on_dispose_lifecycle_d93() -> TestResult {
    let h = harness(300)?;
    h.services
        .logging
        .registry
        .lock()
        .unwrap()
        .set_level("marathon_ios::apple_simulator_device", marathon_logging::Level::WARN);
    let (d, _) = prepared(&h).await?;
    let gate = PermissionGate::attach(&h);
    let batch = spawn_batch(&h, d.clone(), h.services.context.clone());
    gate.entered.wait().await.unwrap();
    let started = tokio::time::Instant::now();
    d.dispose().await.unwrap();
    assert_eq!(started.elapsed(), std::time::Duration::from_millis(300));
    assert!(!gate.finished.load(Ordering::SeqCst));
    assert_eq!(
        gate.lifecycle.load(Ordering::SeqCst),
        0,
        "the lifecycle ran under a live batch"
    );
    assert!(d.retired());
    assert!(d.disposed());
    assert!(d.quarantined());
    let warnings: Vec<_> = h
        .logs
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, level, _, _)| *level == marathon_logging::Level::WARN)
        .map(|(_, _, message, _)| message.clone())
        .collect();
    assert_eq!(
        warnings,
        [format!(
            "Device {}'s work was not confirmed to have ended within 0.3 s (its batch has not ended); not running its \
             on-dispose lifecycle, sending it no further command and keeping its connection out of use",
            d.serial()
        )]
    );
    let before = h.worker.commands.lock().unwrap().len();
    gate.release.complete(Ok(()));
    assert!(matches!(batch.await?.0, Err(VendorError::DeviceLost(_))));
    assert_eq!(
        h.worker.commands.lock().unwrap().len(),
        before,
        "a revoked simulator sent a command"
    );
    assert!(!d.lost());
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_concurrent_dispose_returns_only_after_the_disposal_in_progress_has_finished() -> TestResult {
    let h = harness(5000)?;
    let (d, _) = prepared(&h).await?;
    let gate = PermissionGate::attach(&h);
    let batch = spawn_batch(&h, d.clone(), h.services.context.clone());
    gate.entered.wait().await.unwrap();
    let first = d.dispose();
    let second = d.dispose();
    tokio::pin!(first, second);
    assert!(futures_util::poll!(first.as_mut()).is_pending());
    assert!(futures_util::poll!(second.as_mut()).is_pending());
    assert_eq!(gate.lifecycle.load(Ordering::SeqCst), 0);
    gate.release.complete(Ok(()));
    let (a, b) = tokio::join!(first, second);
    a.unwrap();
    b.unwrap();
    assert!(batch.await?.0.is_err());
    d.dispose().await.unwrap();
    assert_eq!(gate.lifecycle.load(Ordering::SeqCst), 1);
    assert!(!gate.raced.load(Ordering::SeqCst));
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}

/// U179: holds the preparation in its `pkill` of a video recording, ignoring cancellation as a hung probe would.
struct PreparationGate {
    entered: Arc<SharedDeferred<()>>,
    release: Arc<SharedDeferred<()>>,
}
impl PreparationGate {
    fn attach(h: &Harness) -> Self {
        let entered = SharedDeferred::new();
        let release = SharedDeferred::new();
        let (start, gate, boot) = (entered.clone(), release.clone(), h.boot.clone());
        // As the SSH and local executors do: a stopped preparation sends nothing more.
        h.worker.refuses_cancelled.store(true, Ordering::SeqCst);
        *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
            let hold = cmd.first().is_some_and(|s| s == "pkill");
            let (start, gate) = (start.clone(), gate.clone());
            let reply = boot.answer(&cmd);
            Box::pin(async move {
                if hold {
                    start.complete(Ok(()));
                    gate.wait().await?;
                }
                Ok(reply)
            })
        });
        Self { entered, release }
    }
}
fn spawn_prepare(
    h: &Harness,
    d: &Arc<marathon_ios::apple_simulator_device::AppleSimulatorDevice>,
    context: VendorContext,
) -> tokio::task::JoinHandle<marathon_core::error::VendorResult<()>> {
    let (d, configuration) = (d.clone(), h.services.configuration.clone());
    tokio::spawn(async move { d.prepare(configuration, context).await })
}
/// A bound of 70 s, the default, so only the end of the preparation ends the wait.
fn long_bound() -> TestResult<Harness> {
    let mut h = Harness::new(|_| {})?;
    h.services.timeouts.cancelled_batch_millis = 70_000;
    Ok(h)
}
fn sent(h: &Harness) -> usize {
    h.worker.commands.lock().unwrap().len()
}
#[tokio::test(start_paused = true)]
async fn disposal_stops_a_running_preparation_and_returns_only_after_it_has_ended_u179() -> TestResult {
    let h = long_bound()?;
    let d = h.device().await?;
    let gate = PreparationGate::attach(&h);
    let preparation = spawn_prepare(&h, &d, h.services.context.clone());
    gate.entered.wait().await.unwrap();
    let disposal = d.dispose();
    tokio::pin!(disposal);
    assert!(futures_util::poll!(disposal.as_mut()).is_pending());
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    assert!(futures_util::poll!(disposal.as_mut()).is_pending());
    assert!(!d.disposed());
    let before = sent(&h);
    gate.release.complete(Ok(()));
    disposal.await.unwrap();
    assert!(d.disposed());
    assert!(matches!(preparation.await?, Err(VendorError::Cancelled(_))));
    // The preparation was stopped: nothing it would have sent after its held command reached the worker.
    assert_eq!(sent(&h), before);
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn disposal_stops_waiting_for_a_preparation_that_has_not_ended_in_time_u179() -> TestResult {
    let mut h = Harness::new(|_| {})?;
    h.services.timeouts.cancelled_batch_millis = 1000;
    h.services
        .logging
        .registry
        .lock()
        .unwrap()
        .set_level("marathon_ios::apple_simulator_device", marathon_logging::Level::WARN);
    let d = h.device().await?;
    let gate = PreparationGate::attach(&h);
    let preparation = spawn_prepare(&h, &d, h.services.context.clone());
    gate.entered.wait().await.unwrap();
    let started = tokio::time::Instant::now();
    d.dispose().await.unwrap();
    assert_eq!(started.elapsed(), std::time::Duration::from_secs(1));
    assert!(d.disposed());
    // D93: one WARN for the disposal (before: `Cancelled preparation of … disposing of the simulator anyway`).
    let warning = format!(
        "Device {}'s work was not confirmed to have ended within 1 s (a preparation has not ended); not running its \
         on-dispose lifecycle, sending it no further command and keeping its connection out of use",
        d.serial()
    );
    assert_eq!(
        h.logs
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, level, message, _)| *level == marathon_logging::Level::WARN && *message == warning)
            .count(),
        1
    );
    let before = sent(&h);
    gate.release.complete(Ok(()));
    assert!(matches!(preparation.await?, Err(VendorError::Cancelled(_))));
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    // Stopped by disposal, the preparation sends no further command to the simulator it was disposed of.
    assert_eq!(sent(&h), before);
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn concurrent_disposals_both_wait_for_the_stopped_preparation_u179() -> TestResult {
    let h = long_bound()?;
    let d = h.device().await?;
    let gate = PreparationGate::attach(&h);
    let preparation = spawn_prepare(&h, &d, h.services.context.clone());
    gate.entered.wait().await.unwrap();
    let first = d.dispose();
    let second = d.dispose();
    tokio::pin!(first, second);
    assert!(futures_util::poll!(first.as_mut()).is_pending());
    assert!(futures_util::poll!(second.as_mut()).is_pending());
    gate.release.complete(Ok(()));
    let (a, b) = tokio::join!(first, second);
    a.unwrap();
    b.unwrap();
    assert!(matches!(preparation.await?, Err(VendorError::Cancelled(_))));
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_cancel_during_disposal_ends_the_callers_at_once_and_disposal_still_stops_the_preparation_u179() -> TestResult
{
    let h = long_bound()?;
    let d = h.device().await?;
    let gate = PreparationGate::attach(&h);
    let signal = Signal::new();
    let preparation = spawn_prepare(
        &h,
        &d,
        VendorContext {
            cancellation: signal.clone(),
            ..h.services.context.clone()
        },
    );
    gate.entered.wait().await.unwrap();
    {
        // D52: provider termination is abandoned at its limit, dropping its wait for this disposal.
        let abandoned = d.dispose();
        tokio::pin!(abandoned);
        assert!(futures_util::poll!(abandoned.as_mut()).is_pending());
    }
    signal.cancel(cancelled("run cancelled"));
    assert!(matches!(preparation.await?, Err(VendorError::Cancelled(_))));
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    assert!(!d.disposed());
    let before = sent(&h);
    gate.release.complete(Ok(()));
    for _ in 0..100 {
        if d.disposed() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(d.disposed());
    assert_eq!(sent(&h), before);
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_device_that_is_being_disposed_of_starts_no_preparation_u179() -> TestResult {
    let h = Harness::new(|_| {})?;
    let d = h.device().await?;
    d.dispose().await.unwrap();
    let before = sent(&h);
    assert!(matches!(h.prepare(&d).await, Err(VendorError::Cancelled(_))));
    assert_eq!(sent(&h), before);
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
/// D84, review round 3 (R3-2): a run stopping after an internal failure cancels a preparation's caller, which returns at
/// once while the preparation runs on. `join_cancelled_work`, which the stopping run waits for before the provider
/// terminates, stops that preparation and waits for it within the simulator's bound, as disposal does (U179).
#[tokio::test(start_paused = true)]
async fn the_stop_waits_for_a_preparation_its_cancelled_caller_left_running_d84() -> TestResult {
    let h = long_bound()?;
    let d = h.device().await?;
    let gate = PreparationGate::attach(&h);
    let signal = Signal::new();
    let preparation = spawn_prepare(
        &h,
        &d,
        VendorContext {
            cancellation: signal.clone(),
            ..h.services.context.clone()
        },
    );
    gate.entered.wait().await.unwrap();
    signal.cancel(cancelled("the run is stopping"));
    assert!(matches!(preparation.await?, Err(VendorError::Cancelled(_))));
    let joined = d.join_cancelled_work();
    tokio::pin!(joined);
    assert!(futures_util::poll!(joined.as_mut()).is_pending());
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    assert!(
        futures_util::poll!(joined.as_mut()).is_pending(),
        "the stop did not wait for the preparation"
    );
    let before = sent(&h);
    gate.release.complete(Ok(()));
    joined.await.unwrap();
    // The preparation was stopped: nothing it would have sent after its held command reached the worker.
    assert_eq!(sent(&h), before);
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
/// A bridge whose send runs in a task of its own, as the rsync bridge's retained task does: dropping the send leaves it
/// running, and once stopped it takes 3 s to end, as a process that is being stopped does.
#[derive(Default)]
struct SlowlyStoppingSends {
    entered: Arc<SharedDeferred<()>>,
    ended: Arc<AtomicBool>,
}
impl marathon_apple::cmd::FileBridge for SlowlyStoppingSends {
    fn send<'a>(&'a self, _: &'a std::path::Path, _: &'a str) -> marathon_core::runtime::VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    fn receive<'a>(&'a self, _: &'a str, _: &'a std::path::Path) -> marathon_core::runtime::VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    fn send_with<'a>(
        &'a self,
        _: &'a std::path::Path,
        _: &'a str,
        mut stop: marathon_apple::cmd::TransferStop,
    ) -> marathon_core::runtime::VendorFuture<'a, bool> {
        let (entered, ended) = (self.entered.clone(), self.ended.clone());
        let transfer = tokio::spawn(async move {
            entered.complete(Ok(()));
            let stopped = stop.stopped().await;
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            ended.store(true, Ordering::SeqCst);
            Err(stopped.into_error())
        });
        Box::pin(async move {
            transfer.await.map_err(|e| {
                VendorError::Other(marathon_apple::cmd::failure(
                    marathon_error::FailureKind::IllegalState,
                    e.to_string(),
                ))
            })?
        })
    }
}
/// U210: the run stops while a preparation is sending the test bundle. The send's own task goes on stopping after the
/// preparation's caller has returned; the preparation, and so the stop's wait for it, ends only once that task has
/// ended, not when the retry around the send lets go of it.
#[tokio::test(start_paused = true)]
async fn the_stop_waits_for_the_bundle_send_a_cancelled_preparation_was_making_u210() -> TestResult {
    let h = long_bound()?;
    let bridge = Arc::new(SlowlyStoppingSends::default());
    let d = h
        .device_on_bridge(
            h.worker.clone(),
            "a",
            Arc::new(
                marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext::new(
                    marathon_config::devices::Transport::Local,
                    h.clock.clone(),
                ),
            ),
            bridge.clone(),
        )
        .await?;
    let signal = Signal::new();
    let preparation = spawn_prepare(
        &h,
        &d,
        VendorContext {
            cancellation: signal.clone(),
            ..h.services.context.clone()
        },
    );
    bridge.entered.wait().await.unwrap();
    signal.cancel(cancelled("the run is stopping"));
    assert!(matches!(preparation.await?, Err(VendorError::Cancelled(_))));
    assert!(!bridge.ended.load(Ordering::SeqCst));
    d.join_cancelled_work().await.unwrap();
    assert!(
        bridge.ended.load(Ordering::SeqCst),
        "the preparation ended before the send it was making"
    );
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
/// A bridge whose sends never end and ignore their stop, as a local copy blocked on a stalled volume does.
#[derive(Default)]
struct StuckSends {
    entered: Arc<SharedDeferred<()>>,
}
impl marathon_apple::cmd::FileBridge for StuckSends {
    fn send<'a>(&'a self, _: &'a std::path::Path, _: &'a str) -> marathon_core::runtime::VendorFuture<'a, bool> {
        self.entered.complete(Ok(()));
        Box::pin(std::future::pending())
    }
    fn receive<'a>(&'a self, _: &'a str, _: &'a std::path::Path) -> marathon_core::runtime::VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
}
async fn on_bridge(
    h: &Harness,
    bridge: Arc<dyn marathon_apple::cmd::FileBridge>,
) -> TestResult<Arc<marathon_ios::apple_simulator_device::AppleSimulatorDevice>> {
    h.device_on_bridge(
        h.worker.clone(),
        "a",
        Arc::new(
            marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext::new(
                marathon_config::devices::Transport::Local,
                h.clock.clone(),
            ),
        ),
        bridge,
    )
    .await
}
/// U210 review: test discovery installs the bundle through the same installer, under its own timeout. Its send is not
/// waited for after that timeout cancels it, so a send that ignores the cancellation (a blocked local copy) does not keep
/// discovery past its timeout.
#[tokio::test(start_paused = true)]
async fn discovery_ends_at_its_timeout_when_its_send_ignores_the_cancellation_u210() -> TestResult {
    use marathon_core::runtime::VendorClock;
    let h = long_bound()?;
    let bridge = Arc::new(StuckSends::default());
    let d = on_bridge(&h, bridge.clone()).await?;
    let config = marathon_config::VendorConfiguration::Ios((*h.services.vendor).clone());
    let installer = marathon_apple::apple_application_installer::AppleApplicationInstaller::new(
        &config,
        h.services.runtime.clone(),
        h.services.logging.clone(),
    );
    let context = h.services.context.clone();
    let started = tokio::time::Instant::now();
    let discovery = h.clock.with_timeout_or_null(
        context.cancellation.clone(),
        180_000,
        Box::new(|scope| {
            let context = VendorContext {
                cancellation: scope,
                ..context.clone()
            };
            let (installer, d) = (&installer, d.clone());
            Box::pin(async move {
                installer
                    .prepare_installation(&*d, true, &context)
                    .await
                    .map_err(|e| e.into_vendor())
            })
        }),
    );
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(3_600), discovery)
        .await
        .expect("discovery outlived its timeout");
    assert_eq!(outcome, Ok(None));
    assert!(bridge.entered.completed().is_some());
    assert_eq!(started.elapsed(), std::time::Duration::from_secs(180));
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
/// U210 review: a preparation stopped during a send waits for it to end on the cancellation, but only within the
/// simulator's bound for a cancelled call (`cancelled_batch_millis`, 70 s here); a send that never ends is then left.
#[tokio::test(start_paused = true)]
async fn a_preparation_waits_for_its_stopped_send_only_within_the_simulators_bound_u210() -> TestResult {
    let h = long_bound()?;
    let bridge = Arc::new(StuckSends::default());
    let d = on_bridge(&h, bridge.clone()).await?;
    let signal = Signal::new();
    let preparation = spawn_prepare(
        &h,
        &d,
        VendorContext {
            cancellation: signal.clone(),
            ..h.services.context.clone()
        },
    );
    bridge.entered.wait().await.unwrap();
    let stopped = tokio::time::Instant::now();
    signal.cancel(cancelled("the run is stopping"));
    assert!(matches!(preparation.await?, Err(VendorError::Cancelled(_))));
    tokio::time::timeout(std::time::Duration::from_secs(3_600), d.join_retained())
        .await
        .expect("the preparation waited for its send beyond the bound")
        .unwrap();
    assert_eq!(stopped.elapsed(), std::time::Duration::from_secs(70));
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
/// D91: test discovery lets go of its send at its timeout (U210 review), and the send's own task goes on stopping, as the
/// rsync bridge's retained task does. The simulator's disposal waits for it, its caller long gone, before it goes on.
#[tokio::test(start_paused = true)]
async fn disposal_waits_for_a_send_whose_caller_stopped_waiting_d91() -> TestResult {
    use marathon_core::runtime::VendorClock;
    let h = long_bound()?;
    let bridge = Arc::new(SlowlyStoppingSends::default());
    let d = on_bridge(&h, bridge.clone()).await?;
    let config = marathon_config::VendorConfiguration::Ios((*h.services.vendor).clone());
    let installer = marathon_apple::apple_application_installer::AppleApplicationInstaller::new(
        &config,
        h.services.runtime.clone(),
        h.services.logging.clone(),
    );
    let context = h.services.context.clone();
    let discovery = h.clock.with_timeout_or_null(
        context.cancellation.clone(),
        180_000,
        Box::new(|scope| {
            let context = VendorContext {
                cancellation: scope,
                ..context.clone()
            };
            let (installer, d) = (&installer, d.clone());
            Box::pin(async move {
                installer
                    .prepare_installation(&*d, true, &context)
                    .await
                    .map_err(|e| e.into_vendor())
            })
        }),
    );
    assert_eq!(discovery.await, Ok(None));
    assert!(bridge.entered.completed().is_some());
    assert!(!bridge.ended.load(Ordering::SeqCst));
    let disposing = tokio::time::Instant::now();
    d.dispose().await.unwrap();
    assert!(
        bridge.ended.load(Ordering::SeqCst),
        "the simulator was disposed of before the send to it had ended"
    );
    // The send's own 3 s, not the bound for a cancelled call.
    assert_eq!(disposing.elapsed(), std::time::Duration::from_secs(3));
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
/// A bridge that starts a transfer, as rsync would, unless its stop has already come, which it looks at first as the rsync
/// bridge does; it counts what it starts.
#[derive(Default)]
struct StartsUnlessStopped {
    started: Arc<AtomicUsize>,
}
impl marathon_apple::cmd::FileBridge for StartsUnlessStopped {
    fn send<'a>(&'a self, _: &'a std::path::Path, _: &'a str) -> marathon_core::runtime::VendorFuture<'a, bool> {
        self.started.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(true) })
    }
    fn receive<'a>(&'a self, _: &'a str, _: &'a std::path::Path) -> marathon_core::runtime::VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    fn send_with<'a>(
        &'a self,
        src: &'a std::path::Path,
        dst: &'a str,
        mut stop: marathon_apple::cmd::TransferStop,
    ) -> marathon_core::runtime::VendorFuture<'a, bool> {
        match stop.now() {
            Some(stopped) => Box::pin(async move { Err(stopped.into_error()) }),
            None => self.send(src, dst),
        }
    }
}
/// D91: a send asked of a disposed simulator is stopped before it starts, by the disposal: no transfer starts. Nothing
/// else would have stopped it (review round 1: the witness now counts what starts).
#[tokio::test(start_paused = true)]
async fn a_send_to_a_disposed_simulator_is_stopped_before_it_starts_d91() -> TestResult {
    use marathon_apple::apple_device::AppleDevice;
    let h = long_bound()?;
    let bridge = Arc::new(StartsUnlessStopped::default());
    let d = on_bridge(&h, bridge.clone()).await?;
    d.dispose().await.unwrap();
    let dir = tempfile::tempdir()?;
    let context = h.services.context.clone();
    let sent = d.push_file(dir.path(), "/tmp/marathon/late", &context).await;
    match sent {
        Err(VendorError::Cancelled(reason)) => {
            assert!(
                reason
                    .failure()
                    .message
                    .as_deref()
                    .unwrap_or_default()
                    .ends_with("has been disposed"),
                "{reason:?}"
            )
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(bridge.started.load(Ordering::SeqCst), 0, "a transfer started");
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}

/// D93: holds each simulator's batch in its permission grant until that simulator's gate opens, ignoring cancellation as
/// a hung command would, and records when each on-dispose lifecycle (`Terminate`'s `ps aux`) runs, by udid.
struct PerSimulatorGates {
    gates: std::collections::HashMap<String, Arc<SharedDeferred<()>>>,
    entered: Arc<AtomicUsize>,
    lifecycle: Arc<std::sync::Mutex<Vec<(String, tokio::time::Instant)>>>,
}
impl PerSimulatorGates {
    fn attach(h: &Harness, udids: &[&str]) -> Self {
        let gates: std::collections::HashMap<_, _> =
            udids.iter().map(|u| ((*u).to_owned(), SharedDeferred::new())).collect();
        let entered = Arc::new(AtomicUsize::new(0));
        let lifecycle = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (held, count, seen, boot) = (gates.clone(), entered.clone(), lifecycle.clone(), h.boot.clone());
        *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
            let gate = (cmd.get(2).is_some_and(|s| s == "privacy"))
                .then(|| cmd.iter().find_map(|part| held.get(part)).cloned())
                .flatten();
            if let Some(line) = cmd.get(2).filter(|s| s.starts_with("ps aux"))
                && let Some(udid) = held.keys().find(|u| line.contains(&format!(" {u} ")))
            {
                seen.lock().unwrap().push((udid.clone(), tokio::time::Instant::now()));
            }
            let (count, reply) = (count.clone(), boot.answer(&cmd));
            Box::pin(async move {
                if let Some(gate) = gate {
                    count.fetch_add(1, Ordering::SeqCst);
                    gate.wait().await?;
                }
                Ok(reply)
            })
        });
        Self {
            gates,
            entered,
            lifecycle,
        }
    }
}

/// D93, the run's stop as the Scheduler drives it (seal every device, then cancel, then each device's cancelled call and
/// `join_cancelled_work` together, then the provider's disposal), with two simulators and the default 70 s window: `a`'s
/// batch ends at 69 s, `b`'s is held past 75 s. Every wait ends within the one window sealed at the stop: the devices'
/// joins end at 70 s and disposal takes no more time (before: disposal waited another 70 s for `b`, then ran its
/// lifecycle under the live batch). `a` alone runs its on-dispose lifecycle; `b` gets one WARN, no lifecycle, its
/// commands revoked and its lease marked for quarantine.
#[tokio::test(start_paused = true)]
async fn one_window_from_the_runs_stop_and_lifecycle_only_after_confirmed_termination_d93() -> TestResult {
    let h = harness(70_000)?;
    h.services
        .logging
        .registry
        .lock()
        .unwrap()
        .set_level("marathon_ios::apple_simulator_device", marathon_logging::Level::WARN);
    h.worker.add(["b"]);
    let mut devices = Vec::new();
    for udid in ["a", "b"] {
        let cycle = Arc::new(
            marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext::new(
                marathon_config::devices::Transport::Local,
                h.clock.clone(),
            ),
        );
        let d = h.device_on(h.worker.clone(), udid, cycle).await?;
        h.prepare(&d).await.map_err(|e| e.failure().to_string().to_owned())?;
        devices.push(d);
    }
    let (a, b) = (devices[0].clone(), devices[1].clone());
    let gates = PerSimulatorGates::attach(&h, &["a", "b"]);
    let mut calls = Vec::new();
    for d in [&a, &b] {
        let signal = Signal::new();
        let batch = spawn_batch(
            &h,
            d.clone(),
            VendorContext {
                cancellation: signal.clone(),
                ..h.services.context.clone()
            },
        );
        calls.push((d.clone(), signal, batch));
    }
    while gates.entered.load(Ordering::SeqCst) < 2 {
        tokio::task::yield_now().await;
    }
    // The Scheduler's begin_stop: seal both, then cancel both.
    let stopped = tokio::time::Instant::now();
    for (d, _, _) in &calls {
        Device::seal_for_stop(&**d);
    }
    for (_, signal, _) in &calls {
        signal.cancel(cancelled("the run is stopping"));
    }
    let (release_a, release_b) = (gates.gates["a"].clone(), gates.gates["b"].clone());
    tokio::spawn(async move {
        tokio::time::sleep_until(stopped + std::time::Duration::from_secs(69)).await;
        release_a.complete(Ok(()));
    });
    // The stop routine: each device's cancelled call, then its join_cancelled_work, all devices together.
    let joins = calls.into_iter().map(|(d, _, batch)| async move {
        let outcome = batch.await.map(|(outcome, _)| outcome);
        Device::join_cancelled_work(&*d).await.unwrap();
        outcome
    });
    let outcomes = futures_util::future::join_all(joins).await;
    assert!(
        outcomes.iter().all(|o| matches!(o, Ok(Err(VendorError::Cancelled(_))))),
        "{outcomes:?}"
    );
    assert_eq!(stopped.elapsed(), std::time::Duration::from_secs(70));
    // The provider's termination: both disposals, in what is left of the window (nothing).
    let before = h.worker.commands.lock().unwrap().len();
    let (da, db) = tokio::join!(a.dispose(), b.dispose());
    da.unwrap();
    db.unwrap();
    assert_eq!(stopped.elapsed(), std::time::Duration::from_secs(70), "a second window");
    let lifecycle = gates.lifecycle.lock().unwrap().clone();
    assert_eq!(
        lifecycle.iter().map(|(udid, _)| udid.as_str()).collect::<Vec<_>>(),
        ["a"],
        "the on-dispose lifecycle runs only for the simulator whose work ended"
    );
    assert!(!a.quarantined());
    assert!(b.quarantined());
    let warnings: Vec<_> = h
        .logs
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, level, _, _)| *level == marathon_logging::Level::WARN)
        .map(|(_, _, message, _)| message.clone())
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with(&format!(
            "Device {}'s work was not confirmed to have ended within 70 s",
            b.serial()
        )),
        "{warnings:?}"
    );
    // `b`'s batch, let go after the run would have exited, sends nothing more.
    let sent_after_dispose = h.worker.commands.lock().unwrap()[before..].to_vec();
    tokio::time::sleep_until(stopped + std::time::Duration::from_secs(80)).await;
    let mark = h.worker.commands.lock().unwrap().len();
    release_b.complete(Ok(()));
    b.join_retained().await.unwrap();
    let late: Vec<_> = h.worker.commands.lock().unwrap()[mark..].to_vec();
    assert!(late.is_empty(), "the quarantined simulator sent {late:?}");
    assert!(
        sent_after_dispose.iter().all(|c| !c.iter().any(|p| p.contains(" b "))),
        "{sent_after_dispose:?}"
    );
    h.finish(&[a, b]).await;
    Ok(())
}

// ---- D93 review round 1 ---------------------------------------------------------------------------------------------

/// A bridge whose send, once stopped, keeps its transfer for 40 s and then lets go of it while panicking, so that the
/// simulator's receipt ends `Unconfirmed`, as D91's bridge ends one whose cleanup it could not confirm.
#[derive(Default)]
struct UnconfirmedAfterStop {
    entered: Arc<SharedDeferred<()>>,
}
impl marathon_apple::cmd::FileBridge for UnconfirmedAfterStop {
    fn send<'a>(&'a self, _: &'a std::path::Path, _: &'a str) -> marathon_core::runtime::VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    fn receive<'a>(&'a self, _: &'a str, _: &'a std::path::Path) -> marathon_core::runtime::VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    fn send_with<'a>(
        &'a self,
        _: &'a std::path::Path,
        _: &'a str,
        mut stop: marathon_apple::cmd::TransferStop,
    ) -> marathon_core::runtime::VendorFuture<'a, bool> {
        let entered = self.entered.clone();
        let transfer = tokio::spawn(async move {
            entered.complete(Ok(()));
            let _ = stop.stopped().await;
            tokio::time::sleep(std::time::Duration::from_secs(40)).await;
            let _held = stop;
            panic!("D93 probe: the transfer's cleanup could not be confirmed");
        });
        Box::pin(async move {
            let _ = transfer.await;
            Err(VendorError::Other(marathon_apple::cmd::failure(
                marathon_error::FailureKind::IllegalState,
                "the transfer ended unconfirmed",
            )))
        })
    }
}

/// D93 (review round 1, finding 1): a send stopped by the run's stop ends at 40 s with its cleanup unconfirmed. The
/// verdict latched then stands: the provider's disposal later runs no on-dispose lifecycle and quarantines the simulator
/// (before: disposal looked again, no longer saw the ended send, and ran the lifecycle).
#[tokio::test(start_paused = true)]
async fn a_send_unconfirmed_during_the_stop_keeps_the_simulator_from_its_lifecycle_d93() -> TestResult {
    let h = harness(70_000)?;
    let bridge = Arc::new(UnconfirmedAfterStop::default());
    let d = on_bridge(&h, bridge.clone()).await?;
    let gate = PermissionGate::attach(&h);
    let signal = Signal::new();
    let send = {
        let (d, context) = (
            d.clone(),
            VendorContext {
                cancellation: signal.clone(),
                ..h.services.context.clone()
            },
        );
        tokio::spawn(async move {
            use marathon_apple::apple_device::AppleDevice;
            let dir = tempfile::tempdir().unwrap();
            d.push_file(dir.path(), "/tmp/marathon/x", &context).await
        })
    };
    bridge.entered.wait().await.unwrap();
    Device::seal_for_stop(&*d);
    signal.cancel(cancelled("the run is stopping"));
    let _ = send.await;
    Device::join_cancelled_work(&*d).await.unwrap();
    d.dispose().await.unwrap();
    assert_eq!(
        gate.lifecycle.load(Ordering::SeqCst),
        0,
        "the lifecycle ran after an unconfirmed send"
    );
    assert!(d.quarantined());
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}

/// D93 (review round 1, finding 1): the verdict is sticky. The batch is still running when the run's window ends at 70 s,
/// and ends at 71 s; the provider's disposal at 72 s reads the verdict latched at 70 s and runs no lifecycle (before:
/// it looked again, found the batch ended, and ran the lifecycle).
#[tokio::test(start_paused = true)]
async fn work_that_ends_after_the_window_does_not_turn_the_verdict_confirmed_d93() -> TestResult {
    let h = harness(70_000)?;
    let (d, _) = prepared(&h).await?;
    let gate = PermissionGate::attach(&h);
    let signal = Signal::new();
    let batch = spawn_batch(
        &h,
        d.clone(),
        VendorContext {
            cancellation: signal.clone(),
            ..h.services.context.clone()
        },
    );
    gate.entered.wait().await.unwrap();
    let stopped = tokio::time::Instant::now();
    Device::seal_for_stop(&*d);
    signal.cancel(cancelled("the run is stopping"));
    let _ = batch.await?;
    Device::join_cancelled_work(&*d).await.unwrap();
    assert_eq!(stopped.elapsed(), std::time::Duration::from_secs(70));
    tokio::time::sleep_until(stopped + std::time::Duration::from_secs(71)).await;
    gate.release.complete(Ok(()));
    tokio::time::sleep_until(stopped + std::time::Duration::from_secs(72)).await;
    assert!(gate.finished.load(Ordering::SeqCst));
    d.dispose().await.unwrap();
    assert_eq!(
        gate.lifecycle.load(Ordering::SeqCst),
        0,
        "a verdict turned confirmed after the window"
    );
    assert!(d.quarantined());
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}

/// D93 (review round 1, finding 4): the disposal that cannot confirm the simulator's work quarantines its lease in the
/// connection factory itself, through the hook its provider gave, at once: before the provider's termination, so that no
/// simulator set up meanwhile is given that connection.
#[tokio::test(start_paused = true)]
async fn a_disposal_quarantines_the_lease_when_it_decides_to_d93() -> TestResult {
    let h = harness(300)?;
    let (d, _) = prepared(&h).await?;
    let quarantined = Arc::new(AtomicUsize::new(0));
    let count = quarantined.clone();
    d.on_lease_quarantine(Arc::new(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
    }));
    let gate = PermissionGate::attach(&h);
    let batch = spawn_batch(&h, d.clone(), h.services.context.clone());
    gate.entered.wait().await.unwrap();
    d.dispose().await.unwrap();
    assert_eq!(
        quarantined.load(Ordering::SeqCst),
        1,
        "the lease was not quarantined by the disposal"
    );
    d.dispose().await.unwrap();
    assert_eq!(quarantined.load(Ordering::SeqCst), 1);
    gate.release.complete(Ok(()));
    let _ = batch.await?;
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}

/// D93 (review round 1, finding 5): after the run's stop, the on-dispose lifecycle of a simulator whose work ended at 69 s
/// shares the window, with its 5 s reserve as a floor: a lifecycle command that takes 10 s is cut at 75 s and the
/// simulator quarantined, so its disposal ends within the published 75 s (before: 80 s).
#[tokio::test(start_paused = true)]
async fn the_on_dispose_lifecycle_after_the_runs_stop_is_held_to_the_window_d93() -> TestResult {
    let h = harness(70_000)?;
    let (d, _) = prepared(&h).await?;
    let entered = SharedDeferred::new();
    let release = SharedDeferred::new();
    let (start, gate, boot) = (entered.clone(), release.clone(), h.boot.clone());
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let grant = cmd.get(2).is_some_and(|s| s == "privacy");
        let lifecycle = cmd.get(2).is_some_and(|s| s.starts_with("ps aux"));
        let (start, gate, reply) = (start.clone(), gate.clone(), boot.answer(&cmd));
        Box::pin(async move {
            if grant {
                start.complete(Ok(()));
                gate.wait().await?;
            }
            if lifecycle {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
            Ok(reply)
        })
    });
    let signal = Signal::new();
    let batch = spawn_batch(
        &h,
        d.clone(),
        VendorContext {
            cancellation: signal.clone(),
            ..h.services.context.clone()
        },
    );
    entered.wait().await.unwrap();
    let stopped = tokio::time::Instant::now();
    Device::seal_for_stop(&*d);
    signal.cancel(cancelled("the run is stopping"));
    let releasing = release.clone();
    tokio::spawn(async move {
        tokio::time::sleep_until(stopped + std::time::Duration::from_secs(69)).await;
        releasing.complete(Ok(()));
    });
    let _ = batch.await?;
    Device::join_cancelled_work(&*d).await.unwrap();
    tokio::time::sleep_until(stopped + std::time::Duration::from_secs(70)).await;
    // As the SSH and local executors do (D30): a command whose call is cancelled ends.
    h.worker.cancellable_before.store(true, Ordering::SeqCst);
    d.dispose().await.unwrap();
    assert_eq!(stopped.elapsed(), std::time::Duration::from_secs(75));
    assert!(
        d.quarantined(),
        "a lifecycle cut by the window left the lease to be released"
    );
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}

/// D93 (review round 1, finding 2): the local bridge honours the seal. A send asked of a simulator the run's stop has
/// sealed does not copy anything (before: the copy ran, its stop ignored).
#[tokio::test(start_paused = true)]
async fn a_local_send_after_the_seal_copies_nothing_d93() -> TestResult {
    use marathon_apple::apple_device::AppleDevice;
    let h = long_bound()?;
    let d = on_bridge(
        &h,
        Arc::new(marathon_apple::cmd::local::jvm_file_bridge::JvmFileBridge::new(
            h.services.runtime.clone(),
            true,
        )),
    )
    .await?;
    let dir = tempfile::tempdir()?;
    let src = dir.path().join("src");
    std::fs::write(&src, b"bundle")?;
    let dst = dir.path().join("dst");
    Device::seal_for_stop(&*d);
    let sent = d.push_file(&src, dst.to_str().unwrap(), &h.services.context).await;
    assert!(matches!(sent, Err(VendorError::Cancelled(_))), "{sent:?}");
    assert!(!dst.exists(), "a sealed simulator's local send copied its file");
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}

/// D93 (review round 1, finding 2): a local copy in progress owns its receipt until it has really ended. Its source is a
/// FIFO nobody writes to. The copy creates its destination's folder after its last look at its stop and right before it
/// opens the source, where it blocks; once that folder exists the caller lets go of the send. The disposal's window ends
/// with the copy still running, and the simulator is quarantined (before: the receipt ended when the bridge dropped the
/// stop, before copying, and the disposal saw it confirmed). A writer then lets the copy end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_local_copy_in_progress_keeps_its_receipt_until_it_ends_d93() -> TestResult {
    use marathon_apple::apple_device::AppleDevice;
    let mut h = Harness::new(|_| {})?;
    h.services.timeouts.cancelled_batch_millis = 300;
    let d = on_bridge(
        &h,
        Arc::new(marathon_apple::cmd::local::jvm_file_bridge::JvmFileBridge::new(
            h.services.runtime.clone(),
            true,
        )),
    )
    .await?;
    let dir = tempfile::tempdir()?;
    let fifo = dir.path().join("fifo");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status()?;
    assert!(made.success());
    let parent = dir.path().join("sub");
    let dst = parent.join("dst");
    // Whatever happens below, a writer is opened at the end, so that the copy blocked on the FIFO ends.
    struct Unblock(std::path::PathBuf);
    impl Drop for Unblock {
        fn drop(&mut self) {
            let fifo = self.0.clone();
            let writer = std::thread::spawn(move || {
                use std::io::Write;
                if let Ok(mut w) = std::fs::OpenOptions::new().write(true).open(fifo) {
                    let _ = w.write_all(b"x");
                }
            });
            let _ = writer.join();
        }
    }
    let unblock = Unblock(fifo.clone());
    let send = {
        let (d, fifo, dst, context) = (d.clone(), fifo.clone(), dst.clone(), h.services.context.clone());
        tokio::spawn(async move { d.push_file(&fifo, dst.to_str().unwrap(), &context).await })
    };
    let started = std::time::Instant::now();
    while !parent.is_dir() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the copy never started"
        );
        tokio::task::yield_now().await;
    }
    send.abort();
    let _ = send.await;
    d.dispose().await.unwrap();
    let quarantined = d.quarantined();
    drop(unblock);
    assert!(quarantined, "a copy still running was taken as ended");
    tokio::time::timeout(std::time::Duration::from_secs(60), d.join_retained())
        .await
        .expect("the copy did not end")
        .unwrap();
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
