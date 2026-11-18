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
    let (outcome, results) = guarded("the batch", run(&h, &d)).await;
    assert!(matches!(&outcome, Err(error) if is_probe(error)), "{outcome:?}");
    assert!(results.is_none());
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
