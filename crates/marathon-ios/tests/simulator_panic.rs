//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt
//! (D84: a preparation, batch or disposal that panics still reaches whoever waits for it).
//!
//! Each runs in a retained task that publishes its outcome to a cell the caller waits on. Before D84 a panic ended the
//! task before it published anything, and the caller waited for ever (U206). The timeouts are hang guards only.
mod provider_support;
mod simulator_support;
mod support;
use marathon_config::vendor::{GrantLifecycle, IosPermission, LifecycleAction};
use marathon_core::{error::VendorError, vendor::Device};
use simulator_support::*;
use std::{sync::Arc, time::Duration};

const GUARD: Duration = Duration::from_secs(3600);
const PROBE: &str = "D84 probe: the worker command panicked";

/// Panics on the first command `matches` accepts; answers every other one as the harness does.
fn panic_on(h: &Harness, matches: impl Fn(&[String]) -> bool + Send + Sync + 'static) {
    let boot = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let hit = matches(&cmd);
        let reply = boot.answer(&cmd);
        Box::pin(async move {
            if hit {
                panic!("{PROBE}");
            }
            Ok(reply)
        })
    });
}

fn is_probe(error: &VendorError) -> bool {
    error.failure().message.as_deref() == Some(PROBE)
}

async fn guarded<T>(what: &str, future: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(GUARD, future).await {
        Ok(value) => value,
        Err(_) => panic!("{what} did not end"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_preparation_that_panics_ends_with_the_panic_and_does_not_hold_disposal_d84() -> TestResult {
    let h = Harness::new(|_| {})?;
    let d = h.device().await?;
    // The preparation's `pkill` of a video recording, as the U179 gate holds it.
    panic_on(&h, |cmd| cmd.first().is_some_and(|s| s == "pkill"));
    let prepared = guarded("the preparation", h.prepare(&d)).await;
    assert!(matches!(&prepared, Err(error) if is_probe(error)), "{prepared:?}");
    guarded("the disposal", d.dispose()).await.unwrap();
    assert!(d.disposed());
    guarded("the retained tasks", d.join_retained()).await.unwrap();
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_batch_that_panics_ends_with_the_panic_and_does_not_hold_disposal_d84() -> TestResult {
    let h = Harness::new(|v| {
        v.permissions.lifecycle = GrantLifecycle::BeforeEachBatch;
        v.permissions.grant = vec![IosPermission::Calendar];
    })?;
    let (d, _) = prepared(&h).await?;
    // The grant before the batch, which runs inside the batch's retained task.
    panic_on(&h, |cmd| cmd.get(2).is_some_and(|s| s == "privacy"));
    assert_eq!(d.line_listener_count(), 0);
    let (outcome, results) = guarded("the batch", run(&h, &d)).await;
    assert!(matches!(&outcome, Err(error) if is_probe(error)), "{outcome:?}");
    assert!(results.is_none());
    // Review round 1: the panic was after the batch's output-line listeners were installed. A retry must not inherit them.
    assert_eq!(
        d.line_listener_count(),
        0,
        "the panicked batch left its line listeners installed"
    );
    guarded("the disposal", d.dispose()).await.unwrap();
    guarded("the retained tasks", d.join_retained()).await.unwrap();
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_disposal_that_panics_ends_every_caller_with_the_panic_and_logs_it_d84() -> TestResult {
    let h = Harness::new(|v| {
        v.lifecycle_configuration.on_dispose = vec![LifecycleAction::Terminate];
    })?;
    let (d, _) = prepared(&h).await?;
    // The on-dispose Terminate's process listing.
    panic_on(&h, |cmd| cmd.get(2).is_some_and(|s| s.starts_with("ps aux")));
    let (first, second) = guarded("the disposals", async { tokio::join!(d.dispose(), d.dispose()) }).await;
    for outcome in [&first, &second] {
        assert!(matches!(outcome, Err(error) if is_probe(error)), "{outcome:?}");
    }
    assert!(
        h.logs
            .lock()
            .unwrap()
            .iter()
            .any(|(_, level, message, cause)| *level == marathon_logging::Level::ERROR
                && message == "Disposal failed"
                && cause
                    .as_ref()
                    .is_some_and(|cause| cause.message.as_deref() == Some(PROBE))),
        "{:?}",
        h.logs.lock().unwrap()
    );
    guarded("the retained tasks", d.join_retained()).await.unwrap();
    Ok(())
}

/// The provider's update processing, operations and termination run the same way: a caller waits on a cell the retained
/// task completes. Here processing panics on the worker's first command.
#[tokio::test(start_paused = true)]
async fn a_provider_update_that_panics_ends_with_the_panic_d84() -> TestResult {
    let p = provider_support::ProviderHarness::new(|_| {})?;
    panic_on(&p.h, |_| true);
    let updated = guarded(
        "the update",
        p.provider.process_update(
            provider_support::snapshot(vec![(
                marathon_config::devices::Transport::Local,
                provider_support::targets(&["a"]),
            )]),
            p.h.services.context.clone(),
        ),
    )
    .await;
    assert!(matches!(&updated, Err(error) if is_probe(error)), "{updated:?}");
    Ok(())
}

/// D84, review round 1: the provider's monitor provisions the first simulators itself (not through `process_update`).
/// A panic there ended the monitor, whose result termination discards, and the run waited for devices until its
/// device-initialization timeout. The device channel now fails with the panic.
#[tokio::test(start_paused = true)]
async fn a_monitor_that_panics_fails_the_device_channel_d84() -> TestResult {
    let p = provider_support::ProviderHarness::new(|_| {})?;
    std::fs::write(
        p.h.dir.path().join("Marathondevices"),
        "workers:\n- transport: {type: local}\n  devices: [{type: simulator, udid: a}]\n",
    )?;
    panic_on(&p.h, |_| true);
    use marathon_core::vendor::DeviceProvider;
    guarded(
        "the initialization",
        p.provider.initialize(p.h.services.context.clone()),
    )
    .await
    .map_err(provider_support::test_error)?;
    let received = guarded("the device channel", p.provider.subscribe().receive()).await;
    match &received {
        Err(error) => assert!(is_probe(error), "{error:?}"),
        Ok(_) => panic!("the device channel did not fail"),
    }
    guarded("the termination", p.provider.terminate())
        .await
        .map_err(provider_support::test_error)?;
    Ok(())
}

/// Panics when a test passes, from inside the runner's event processing.
struct PanicsOnPass;
impl marathon_apple::listener::AppleTestRunListener for PanicsOnPass {
    fn test_passed<'a>(
        &'a self,
        _: &'a marathon_core::model::Test,
        _: i64,
        _: i64,
    ) -> marathon_core::runtime::VendorFuture<'a, ()> {
        Box::pin(async { panic!("{PROBE}") })
    }
}
struct PanickingListeners;
impl marathon_ios::apple_simulator_device::SimulatorListenerFactory for PanickingListeners {
    fn create(
        &self,
        device: Arc<marathon_ios::apple_simulator_device::AppleSimulatorDevice>,
        pool: marathon_core::model::DevicePoolId,
        batch: marathon_core::model::BatchRef,
        context: marathon_core::vendor::VendorContext,
    ) -> marathon_core::error::VendorResult<marathon_ios::apple_simulator_device::SimulatorListeners> {
        let mut listeners = marathon_ios::apple_simulator_device::MinimalSimulatorListenerFactory
            .create(device, pool, batch, context)?;
        listeners.artifacts = Arc::new(PanicsOnPass);
        Ok(listeners)
    }
}

/// D84, review round 1: a listener that panics while the runner processes the batch's events used to skip the
/// runner's cleanup (`after_test_run`, the event channel's cancel and the xcodebuild session's stop), so the session's
/// producer could stay blocked on its channel and hold the owner's drain. The panic is now the run's error and the
/// cleanup runs: the batch ends with it, the session is closed, and the device's tasks drain.
#[tokio::test(start_paused = true)]
async fn a_listener_that_panics_still_lets_the_batch_clean_up_d84() -> TestResult {
    let mut h = Harness::new(|_| {})?;
    h.services.listeners = Arc::new(PanickingListeners);
    let (d, script) = prepared(&h).await?;
    script.output(passed(3));
    let (outcome, _) = guarded("the batch", run(&h, &d)).await;
    assert!(matches!(&outcome, Err(error) if is_probe(error)), "{outcome:?}");
    assert_eq!(d.line_listener_count(), 0);
    let closed = guarded(
        "the session's close",
        d.await_session_closed(jcompat::jtime::duration_of(1, 0).unwrap(), &h.services.context),
    )
    .await;
    assert!(matches!(closed, Ok(true)), "{closed:?}");
    guarded("the disposal", d.dispose()).await.unwrap();
    guarded("the retained tasks", d.join_retained()).await.unwrap();
    Ok(())
}
