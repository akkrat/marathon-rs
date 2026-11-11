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
