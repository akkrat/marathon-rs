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
#[tokio::test(start_paused = true)]
async fn disposal_stops_waiting_for_a_batch_that_has_not_finished_in_time() -> TestResult {
    let h = harness(300)?;
    let (d, _) = prepared(&h).await?;
    let gate = PermissionGate::attach(&h);
    let batch = spawn_batch(&h, d.clone(), h.services.context.clone());
    gate.entered.wait().await.unwrap();
    d.dispose().await.unwrap();
    assert!(!gate.finished.load(Ordering::SeqCst));
    assert_eq!(gate.lifecycle.load(Ordering::SeqCst), 1);
    assert!(d.retired());
    assert!(d.disposed());
    gate.release.complete(Ok(()));
    assert!(matches!(batch.await?.0, Err(VendorError::DeviceLost(_))));
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
    let warning = format!(
        "Cancelled preparation of {} has not finished within 1 s, disposing of the simulator anyway",
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
