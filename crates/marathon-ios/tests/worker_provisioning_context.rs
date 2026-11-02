//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/WorkerProvisioningContextTest.kt
//! Only the context-only row belongs here; real setup/provider assertions stay with 5.2/5.5a.
mod support;
use marathon_apple::device::connection_factory::Executor;
use marathon_ios::device::worker_provisioning_context::*;
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use support::*;
use tokio::sync::oneshot;

#[tokio::test]
async fn an_inventory_requested_earlier_never_replaces_a_later_one() -> TestResult {
    let h = Harness::new();
    let context = WorkerProvisioningContext::new(worker("worker"), h.clock.clone());
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let release = gate.clone();
    let lists = requests.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |args| {
        let release = release.clone();
        let lists = lists.clone();
        Box::pin(async move {
            if args == marathon_test_support::apple::fake_simulator_worker::LIST {
                let earlier = lists.fetch_add(1, Ordering::SeqCst) == 0;
                if earlier {
                    release.acquire().await.map_err(|e| error(&e.to_string()))?.forget();
                }
                return Ok(Some(marathon_test_support::apple::fake_simulator_worker::ok([
                    inventory_json(if earlier { "old" } else { "new" }),
                ])));
            }
            Ok(None)
        })
    });
    let first = context.refresh(&h.bin, &h.context);
    tokio::pin!(first);
    while requests.load(Ordering::SeqCst) == 0 {
        assert!(futures_util::poll!(&mut first).is_pending());
        tokio::task::yield_now().await;
    }
    context.refresh(&h.bin, &h.context).await?;
    gate.add_permits(1);
    assert_eq!(first.await?.devices.devices[0].udid, "old");
    assert_eq!(
        context.inventory(&h.bin, &h.context).await?.devices.devices[0].udid,
        "new"
    );
    assert_eq!(context.simulator("new", &h.bin, false, &h.context).await?.udid, "new");
    Ok(())
}
#[tokio::test]
async fn ttl_is_inclusive_measured_from_start_and_wraps_as_a_java_long() -> TestResult {
    for now in [9_999_999_999, 10_000_000_000, 10_000_000_001] {
        let h = Harness::new();
        let clock = Arc::new(NanoClock(AtomicI64::new(0)));
        let c = WorkerProvisioningContext::new(worker("worker"), clock.clone());
        c.refresh_with(h.worker.clone(), || async { Ok(inventory("old")) })
            .await?;
        clock.0.store(now, Ordering::SeqCst);
        let result = c
            .inventory_with(h.worker.clone(), &h.context, || async { Ok(inventory("new")) })
            .await?;
        assert_eq!(
            result.devices.devices[0].udid,
            if now <= MAX_INVENTORY_AGE_NANOS { "old" } else { "new" }
        );
    }
    let h = Harness::new();
    let clock = Arc::new(NanoClock(AtomicI64::new(i64::MAX - 2)));
    let c = WorkerProvisioningContext::new(worker("worker"), clock.clone());
    c.refresh_with(h.worker.clone(), || async {
        clock.0.store(i64::MIN + 2, Ordering::SeqCst);
        Ok(inventory("wrapped"))
    })
    .await?;
    assert_eq!(
        c.inventory_with(h.worker.clone(), &h.context, || async {
            panic!("wrapping age is five nanoseconds")
        })
        .await?
        .devices
        .devices[0]
            .udid,
        "wrapped"
    );
    let clock = Arc::new(NanoClock(AtomicI64::new(0)));
    let c = WorkerProvisioningContext::new(worker("worker"), clock.clone());
    c.refresh_with(h.worker.clone(), || async {
        clock.0.store(10_000_000_001, Ordering::SeqCst);
        Ok(inventory("slow"))
    })
    .await?;
    assert_eq!(
        c.inventory_with(h.worker.clone(), &h.context, || async { Ok(inventory("fresh")) })
            .await?
            .devices
            .devices[0]
            .udid,
        "fresh"
    );
    Ok(())
}
#[tokio::test]
async fn later_failure_does_not_block_older_success_and_cached_readers_skip_refresh_loads() -> TestResult {
    let h = Harness::new();
    let c = WorkerProvisioningContext::new(worker("worker"), h.clock.clone());
    c.refresh_with(h.worker.clone(), || async { Ok(inventory("cached")) })
        .await?;
    let (tx, rx) = oneshot::channel();
    let first = c.refresh_with(h.worker.clone(), || async {
        rx.await.map_err(|e| error(&e.to_string()))?;
        Ok(inventory("old-request"))
    });
    tokio::pin!(first);
    assert!(futures_util::poll!(&mut first).is_pending());
    assert_eq!(
        c.inventory_with(h.worker.clone(), &h.context, || async {
            panic!("cached reader blocked")
        })
        .await?
        .devices
        .devices[0]
            .udid,
        "cached"
    );
    assert!(
        c.refresh_with(h.worker.clone(), || async { Err(error("later request failed").into()) })
            .await
            .is_err()
    );
    let _ = tx.send(());
    first.await?;
    assert_eq!(
        c.inventory_with(h.worker.clone(), &h.context, || async {
            panic!("failed request must not publish its generation")
        })
        .await?
        .devices
        .devices[0]
            .udid,
        "old-request"
    );
    Ok(())
}
#[tokio::test]
async fn disconnected_same_executor_reuses_but_replacements_reload_inventory_and_facts() -> TestResult {
    let h = Harness::new();
    let c = WorkerProvisioningContext::new(worker("worker"), h.clock.clone());
    c.refresh_with(h.worker.clone(), || async { Ok(inventory("old")) })
        .await?;
    c.worker_facts(h.worker.clone(), &h.context, || async { Ok(facts(true)) })
        .await?;
    h.worker.connected.store(false, Ordering::SeqCst);
    assert_eq!(
        c.inventory_with(h.worker.clone(), &h.context, || async {
            panic!("source identity shortcut")
        })
        .await?
        .devices
        .devices[0]
            .udid,
        "old"
    );
    c.worker_facts(h.worker.clone(), &h.context, || async {
        panic!("source identity shortcut")
    })
    .await?;
    let replacement = Harness::new();
    let executor: Executor = replacement.worker.clone();
    assert_eq!(
        c.inventory_with(executor.clone(), &h.context, || async { Ok(inventory("replacement")) })
            .await?
            .devices
            .devices[0]
            .udid,
        "replacement"
    );
    let mut changed = facts(true);
    changed.arch = "x86_64".into();
    assert_eq!(
        c.worker_facts(executor, &h.context, || async { Ok(changed) })
            .await?
            .arch
            .to_string_lossy(),
        "x86_64"
    );
    Ok(())
}
#[tokio::test]
async fn unshareable_and_failed_fact_loads_are_retried_without_poisoning_the_cycle() -> TestResult {
    let h = Harness::new();
    let c = WorkerProvisioningContext::new(worker("worker"), h.clock.clone());
    assert!(
        !c.worker_facts(h.worker.clone(), &h.context, || async { Ok(facts(false)) })
            .await?
            .shareable
    );
    assert!(
        c.worker_facts(h.worker.clone(), &h.context, || async {
            Err(error("partial facts").into())
        })
        .await
        .is_err()
    );
    let shared = c
        .worker_facts(h.worker.clone(), &h.context, || async { Ok(facts(true)) })
        .await?;
    assert!(Arc::ptr_eq(
        &shared,
        &c.worker_facts(h.worker.clone(), &h.context, || async {
            panic!("shareable completed facts should be cached")
        })
        .await?
    ));
    Ok(())
}

#[tokio::test]
async fn throwing_old_connection_getter_forces_a_replacement_load_without_publishing_a_failure() -> TestResult {
    let h = Harness::new();
    let c = WorkerProvisioningContext::new(worker("worker"), h.clock.clone());
    c.refresh_with(h.worker.clone(), || async { Ok(inventory("old")) })
        .await?;
    *h.worker.on_connected_check.lock().unwrap() = Arc::new(|| panic!("closing"));
    let replacement = Harness::new();
    assert_eq!(
        c.inventory_with(replacement.worker.clone(), &h.context, || async {
            Ok(inventory("replacement"))
        })
        .await?
        .devices
        .devices[0]
            .udid,
        "replacement"
    );
    Ok(())
}
#[tokio::test]
async fn facts_single_flight_waiters_can_cancel_and_inventory_loads_progress_independently() -> TestResult {
    let h = Harness::new();
    let c = WorkerProvisioningContext::new(worker("worker"), h.clock.clone());
    let (tx, rx) = oneshot::channel();
    let first = c.worker_facts(h.worker.clone(), &h.context, || async {
        rx.await.map_err(|e| error(&e.to_string()))?;
        Ok(facts(true))
    });
    tokio::pin!(first);
    assert!(futures_util::poll!(&mut first).is_pending());
    let signal = marathon_core::executor::shared::Signal::new();
    let caller = marathon_core::vendor::VendorContext {
        cancellation: signal.clone(),
        ..h.context.clone()
    };
    let waiter = c.worker_facts(h.worker.clone(), &caller, || async { panic!("waiter must not load") });
    tokio::pin!(waiter);
    assert!(futures_util::poll!(&mut waiter).is_pending());
    assert_eq!(
        c.inventory_with(h.worker.clone(), &h.context, || async { Ok(inventory("independent")) })
            .await?
            .devices
            .devices[0]
            .udid,
        "independent"
    );
    signal.cancel(marathon_apple::cmd::cancelled("waiting for facts"));
    assert!(waiter.await.unwrap_err().is_cancelled());
    let _ = tx.send(());
    first.await?;
    let inventory = WorkerProvisioningContext::new(worker("worker"), h.clock.clone());
    let (tx, rx) = oneshot::channel();
    let loading = inventory.inventory_with(h.worker.clone(), &h.context, || async {
        rx.await.map_err(|e| error(&e.to_string()))?;
        Ok(support::inventory("loaded"))
    });
    tokio::pin!(loading);
    assert!(futures_util::poll!(&mut loading).is_pending());
    assert!(
        inventory
            .inventory_with(h.worker.clone(), &caller, || async {
                panic!("cancelled inventory waiter must not load")
            })
            .await
            .unwrap_err()
            .is_cancelled()
    );
    let _ = tx.send(());
    loading.await?;
    Ok(())
}

#[tokio::test]
async fn unavailable_lookup_reports_the_transport_id() -> TestResult {
    let h = Harness::new();
    h.worker.add(["U"]);
    h.worker.simulators.lock().unwrap()["U"]
        .available
        .store(false, Ordering::SeqCst);
    let c = WorkerProvisioningContext::new(worker("worker-1"), h.clock.clone());
    let error = c.simulator("U", &h.bin, false, &h.context).await.unwrap_err();
    assert_eq!(
        error.failure().message.unwrap().to_string_lossy(),
        "udid U is not available at worker-1:22"
    );
    Ok(())
}
