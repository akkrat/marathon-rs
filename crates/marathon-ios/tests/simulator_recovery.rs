//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProviderReconnectTest.kt
mod provider_support;
mod recovery_support;
mod simulator_support;
mod support;
use marathon_apple::apple_device::AppleDevice;
use marathon_config::devices::Transport;
use marathon_core::{executor::shared::SharedDeferred, runtime::Deferred, vendor::DeviceProvider};
use provider_support::*;
use recovery_support::*;
use simulator_support::*;
use std::sync::{Arc, atomic::Ordering};

#[tokio::test(start_paused = true)]
async fn a_simulator_lost_while_its_connection_stays_up_is_set_up_again() -> TestResult {
    let p = RecoveryHarness::fast()?;
    p.start(&["a", "b"]).await?;
    let original = p.connected(2).await;
    let a = original.iter().find(|d| d.udid() == "a").unwrap();
    p.lose(a).await;
    let replacement = p.connected(1).await.remove(0);
    assert_eq!(replacement.udid(), "a");
    assert!(!Arc::ptr_eq(a, &replacement));
    assert!(a.disposed());
    assert!(Arc::ptr_eq(
        &p.current("b"),
        original.iter().find(|d| d.udid() == "b").unwrap()
    ));
    assert_eq!(p.base.connections.released.load(Ordering::SeqCst), 1);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_that_keeps_getting_lost_is_set_up_again_a_limited_number_of_times_after_growing_pauses()
-> TestResult {
    let p = RecoveryHarness::settings(|_| {}, 2, 500)?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let start = p.h().clock.now();
    for loss in 1..=2 {
        let old = p.current("a");
        let at = p.h().clock.now();
        p.lose(&old).await;
        let replacement = p.connected(1).await.remove(0);
        assert_eq!(replacement.udid(), "a");
        assert!(p.h().clock.now() - at >= 500 * (loss - 1));
    }
    p.lose(&p.current("a")).await;
    eventually(|| p.log("has been lost 3 times")).await;
    advance(2000).await;
    assert_eq!(home(&p.h().worker, "a"), 3);
    assert_eq!(home(&p.h().worker, "b"), 1);
    assert!(p.current("a").disposed());
    assert!(p.h().clock.now() - start >= 500);
    p.update(&["a", "b"]).await?;
    assert_eq!(home(&p.h().worker, "a"), 3);
    assert!(
        !p.provider
            .current(&Transport::Local)?
            .eligible_disposed
            .contains(&"a".into())
    );
    p.balanced().await
}
/// D39 (U165), the audit's scenario for U165: every batch goes silent. Up to 50 batches are offered, and the
/// simulator is set up again after every third until its recoveries, the default 3, run out. The pause is short
/// only because the default's third, 60 s, would meet this harness's watchdog.
#[tokio::test(start_paused = true)]
async fn fifty_silent_batches_recycle_the_simulator_until_its_recoveries_run_out_d39() -> TestResult {
    let p = RecoveryHarness::settings(|_| {}, 3, 500)?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let silent = Arc::new(BatchScript::default());
    *silent.plan.lock().unwrap() = OutputPlan {
        lines: vec!["Test Case '-[Suite test1]' started.".into()],
        failure: Some(marathon_apple::cmd::failure(
            "java.util.concurrent.TimeoutException",
            "idle timeout reached",
        )),
        ..Default::default()
    };
    silent.attach(&p.h().worker);
    let mut handle = p.current("a");
    p.h().prepare(&handle).await.unwrap();
    let (mut batches, mut per_handle, mut recycled) = (0, vec![], 0);
    let mut on_handle = 0;
    while batches < 50 {
        batches += 1;
        on_handle += 1;
        let outcome = run(p.h(), &handle).await.0;
        if !matches!(outcome, Err(marathon_core::error::VendorError::DeviceLost(_))) {
            continue;
        }
        per_handle.push(on_handle);
        on_handle = 0;
        if recycled == 3 {
            break;
        }
        handle = p.connected(1).await.remove(0);
        recycled += 1;
        p.h().prepare(&handle).await.unwrap();
    }
    assert_eq!(per_handle, [3, 3, 3, 3]);
    assert_eq!(batches, 12);
    eventually(|| p.log("has been lost 4 times")).await;
    advance(2000).await;
    assert!(p.current("a").disposed());
    p.balanced().await
// D47's witness: a lost connection isn't charged to the limit above, and each replacement starts with a fresh
// maxBatchesWithoutProgress count, so a worker that drops every batch has no bound but uncompletedTestRetryQuota.
#[tokio::test(start_paused = true)]
async fn a_worker_that_keeps_dropping_its_connection_is_not_charged_to_the_limit_on_losses() -> TestResult {
    let p = RecoveryHarness::settings(|_| {}, 2, 500)?;
    p.start(&["a"]).await?;
    p.connected(1).await;
    let mut worker = p.h().worker.clone();
    for drops in 1..=5 {
        let old = p.current("a");
        let fresh = p.base.worker(Transport::Local, &["a"]);
        *p.base.connections.workers.lock().unwrap() = vec![(Transport::Local, fresh.clone())];
        worker.connected.store(false, Ordering::SeqCst);
        worker = fresh.clone();
        let replacement = p.connected(1).await.remove(0);
        assert!(!Arc::ptr_eq(&old, &replacement), "drop {drops}");
        assert_eq!(home(&fresh, "a"), 1, "drop {drops}");
    }
    assert!(!p.log("has been lost"));
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_lost_for_the_rest_of_the_run_stays_out_of_device_updates_and_connection_recovery() -> TestResult {
    let p = RecoveryHarness::fast()?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let old = p.current("a");
    old.retire();
    let fresh = p.base.worker(Transport::Local, &["a", "b"]);
    *p.base.connections.workers.lock().unwrap() = vec![(Transport::Local, fresh.clone())];
    p.h().worker.connected.store(false, Ordering::SeqCst);
    assert_eq!(p.connected(1).await[0].udid(), "b");
    advance(100).await;
    assert!(Arc::ptr_eq(&old, &p.current("a")));
    assert_eq!(home(&fresh, "a"), 0);
    p.update(&["a", "b"]).await?;
    assert_eq!(home(&fresh, "a"), 0);
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_taken_out_for_the_rest_of_the_run_while_it_is_being_recovered_stays_out() -> TestResult {
    let p = RecoveryHarness::fast()?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let old = p.current("a");
    let retired = old.clone();
    let boot = p.h().boot.clone();
    *p.h().worker.before.lock().unwrap() = Arc::new(move |c| {
        if c.last().is_some_and(|s| s == "HOME") {
            retired.retire();
        }
        let a = boot.answer(&c);
        Box::pin(async move { Ok(a) })
    });
    p.lose(&old).await;
    eventually(|| home(&p.h().worker, "a") == 2).await;
    eventually(|| p.log("its new setup is dropped")).await;
    assert!(Arc::ptr_eq(&old, &p.current("a")));
    assert!(old.retired());
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_whose_test_parsing_session_doesn_t_close_is_not_set_up_again_after_its_connection_is_lost()
-> TestResult {
    use marathon_apple::xcodebuild_session::XcodebuildSession;
    let p = RecoveryHarness::fast()?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let old = p.current("a");
    p.h().prepare(&old).await.map_err(test_error)?;
    let script = Arc::new(BatchScript::default());
    let gate = SharedDeferred::new();
    *script.plan.lock().unwrap() = OutputPlan {
        lines: vec!["Test runner exited".into()],
        close_gate: Some(gate.clone()),
        ..Default::default()
    };
    script.attach(&p.h().worker);
    let session = XcodebuildSession::new(p.h().services.runtime.clone(), p.h().services.logging.clone());
    let events = old
        .execute_test_request(
            marathon_apple::test::TestRequest::new("/tmp/marathon", "/tmp/marathon/parse.xctestrun", false),
            session.clone(),
            &p.h().services.context,
        )
        .await
        .map_err(test_error)?;
    events.receive(&p.h().services.context).await.map_err(test_error)?;
    assert!(
        session
            .stop(support::duration(20), &p.h().services.context)
            .await
            .is_some()
    );
    let fresh = p.base.worker(Transport::Local, &["a", "b"]);
    *p.base.connections.workers.lock().unwrap() = vec![(Transport::Local, fresh.clone())];
    p.h().worker.connected.store(false, Ordering::SeqCst);
    eventually(|| old.retired()).await;
    assert_eq!(p.connected(1).await[0].udid(), "b");
    assert_eq!(home(&fresh, "a"), 0);
    gate.complete(Ok(()));
    session.join_producer().await.map_err(test_error)?;
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn a_device_update_leaves_a_simulator_lost_while_connected_to_its_recovery() -> TestResult {
    let p = RecoveryHarness::settings(|_| {}, 3, 600_000)?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    p.lose(&p.current("a")).await;
    p.connected(1).await;
    let old = p.current("a");
    p.lose(&old).await;
    eventually(|| old.disposed()).await;
    p.update(&["a", "b"]).await?;
    assert!(Arc::ptr_eq(&old, &p.current("a")));
    assert!(p.provider.current(&Transport::Local)?.eligible_disposed.is_empty());
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_that_has_left_the_devices_file_stays_out_of_the_recovery_of_its_worker_until_it_is_added_back()
-> TestResult {
    let p = RecoveryHarness::fast()?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    p.update(&["b"]).await?;
    let old = p.current("a");
    assert!(old.disposed());
    let fresh = p.base.worker(Transport::Local, &["a", "b"]);
    *p.base.connections.workers.lock().unwrap() = vec![(Transport::Local, fresh.clone())];
    p.h().worker.connected.store(false, Ordering::SeqCst);
    assert_eq!(p.connected(1).await[0].udid(), "b");
    advance(100).await;
    assert_eq!(home(&fresh, "a"), 0);
    p.update(&["a", "b"]).await?;
    assert_eq!(p.connected(1).await[0].udid(), "a");
    assert_eq!(home(&fresh, "a"), 1);
    p.finish().await
}
async fn blocked_replacement(readd: bool) -> TestResult {
    let p = RecoveryHarness::new(|v| {
        if readd {
            v.lifecycle_configuration.on_dispose = vec![marathon_config::vendor::LifecycleAction::Shutdown];
        }
    })?;
    p.h().worker.add(["c"]);
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let old = p.current("a");
    let entered = SharedDeferred::new();
    let gate = SharedDeferred::new();
    let dropping = SharedDeferred::new();
    let dispose_gate = SharedDeferred::new();
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let e = entered.clone();
    let g = gate.clone();
    let dropping_started = dropping.clone();
    let release_disposal = dispose_gate.clone();
    let arm = armed.clone();
    let boot = p.h().boot.clone();
    let once = Arc::new(std::sync::atomic::AtomicBool::new(true));
    *p.h().worker.before.lock().unwrap() = Arc::new(move |c| {
        let block = c == ["xcrun", "simctl", "getenv", "a", "HOME"] && once.swap(false, Ordering::SeqCst);
        let dispose = c == ["xcrun", "simctl", "shutdown", "a"] && arm.swap(false, Ordering::SeqCst);
        let e = e.clone();
        let g = g.clone();
        let dropping = dropping_started.clone();
        let dispose_gate = release_disposal.clone();
        let answer = boot.answer(&c);
        Box::pin(async move {
            if block {
                e.complete(Ok(()));
                g.wait().await?;
            }
            if dispose {
                dropping.complete(Ok(()));
                dispose_gate.wait().await?;
            }
            Ok(answer)
        })
    });
    p.lose(&old).await;
    entered.wait().await.map_err(test_error)?;
    p.update(&["b"]).await?;
    armed.store(readd, Ordering::SeqCst);
    if readd {
        p.h().boot.state.store(3, Ordering::SeqCst);
    }
    gate.complete(Ok(()));
    eventually(|| p.provider.skipped_removed(&"a@local".into())).await;
    if readd {
        support::watchdog(dropping.wait()).await.map_err(test_error)?;
        p.update(&["a", "b", "c"]).await?;
        assert_eq!(p.connected(1).await[0].udid(), "c");
        assert!(Arc::ptr_eq(&old, &p.current("a")));
        dispose_gate.complete(Ok(()));
        assert_eq!(p.connected(1).await[0].udid(), "a");
        assert_eq!(home(&p.h().worker, "a"), 3);
    } else {
        advance(100).await;
        assert!(Arc::ptr_eq(&old, &p.current("a")));
    }
    assert_eq!(home(&p.h().worker, "b"), 1);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_that_leaves_the_devices_file_while_its_recovery_sets_it_up_again_stays_out() -> TestResult {
    blocked_replacement(false).await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_added_back_while_its_recovery_drops_it_is_set_up_again_once_the_recovery_has_finished()
-> TestResult {
    blocked_replacement(true).await
}
#[tokio::test(start_paused = true)]
async fn a_device_update_sets_up_again_a_simulator_whose_recovery_has_failed_and_leaves_the_healthy_one_alone()
-> TestResult {
    let p = RecoveryHarness::fast()?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let old = p.current("a");
    let boot = p.h().boot.clone();
    let once = Arc::new(std::sync::atomic::AtomicBool::new(true));
    *p.h().worker.before.lock().unwrap() = Arc::new(move |c| {
        let fail = c == ["xcrun", "simctl", "getenv", "a", "HOME"] && once.swap(false, Ordering::SeqCst);
        let answer = boot.answer(&c);
        Box::pin(async move {
            if fail {
                Err(support::error("setup failed"))
            } else {
                Ok(answer)
            }
        })
    });
    p.lose(&old).await;
    eventually(|| {
        p.provider
            .current(&Transport::Local)
            .is_ok_and(|c| c.eligible_disposed == vec!["a".to_owned()])
    })
    .await;
    p.update(&["a", "b", "not-found"]).await?;
    let replacement = p.connected(1).await.remove(0);
    assert_eq!(replacement.udid(), "a");
    assert_eq!(home(&p.h().worker, "b"), 1);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_device_update_leaves_alone_a_simulator_that_its_recovery_replaces_while_the_update_is_planned() -> TestResult
{
    let p = RecoveryHarness::fast()?;
    p.start(&["a"]).await?;
    p.connected(1).await;
    let old = p.current("a");
    let listing = SharedDeferred::new();
    let gate = SharedDeferred::new();
    let entered = listing.clone();
    let release = gate.clone();
    let boot = p.h().boot.clone();
    let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
    *p.h().worker.before.lock().unwrap() = Arc::new(move |c| {
        let block =
            c == marathon_test_support::apple::fake_simulator_worker::LIST && first.swap(false, Ordering::SeqCst);
        let e = entered.clone();
        let g = release.clone();
        let answer = boot.answer(&c);
        Box::pin(async move {
            if block {
                e.complete(Ok(()));
                g.wait().await?;
            }
            Ok(answer)
        })
    });
    let provider = p.provider.clone();
    let caller = p.h().services.context.clone();
    let update = tokio::spawn(async move {
        provider
            .process_update(snapshot(vec![(Transport::Local, targets(&["a", "b"]))]), caller)
            .await
    });
    listing.wait().await.map_err(test_error)?;
    p.lose(&old).await;
    let replacement = p.connected(1).await.remove(0);
    gate.complete(Ok(()));
    update.await?.map_err(test_error)?;
    p.connected(1).await;
    assert!(Arc::ptr_eq(&replacement, &p.current("a")));
    assert_eq!(home(&p.h().worker, "a"), 2);
    assert_eq!(home(&p.h().worker, "b"), 1);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_lost_worker_is_torn_down_in_parallel_without_holding_up_the_recovery_of_another_worker() -> TestResult {
    parallel_teardown(false).await
}
#[tokio::test(start_paused = true)]
async fn a_device_update_leaves_the_simulators_of_a_lost_worker_to_their_recovery() -> TestResult {
    parallel_teardown(true).await
}
async fn parallel_teardown(update: bool) -> TestResult {
    let p = RecoveryHarness::fast()?;
    p.h().worker.add(["c"]);
    let peer = p.base.worker(support::worker("peer"), &["peer"]);
    std::fs::write(
        p.h().dir.path().join("Marathondevices"),
        "workers:\n- transport: {type: local}\n  devices: [{type: simulator, udid: a}, {type: simulator, udid: b}]\n- transport: {type: ssh, addr: peer}\n  devices: [{type: simulator, udid: peer}]\n",
    )?;
    p.provider
        .initialize(p.h().services.context.clone())
        .await
        .map_err(test_error)?;
    p.connected(3).await;
    let gate = SharedDeferred::new();
    let script = Arc::new(BatchScript::default());
    *script.plan.lock().unwrap() = OutputPlan {
        hang: true,
        close_gate: Some(gate.clone()),
        ..Default::default()
    };
    script.attach(&p.h().worker);
    for id in ["a", "b"] {
        p.h().prepare(&p.current(id)).await.map_err(test_error)?;
    }
    let a = spawn_batch(p.h(), p.current("a"), p.h().services.context.clone());
    let b = spawn_batch(p.h(), p.current("b"), p.h().services.context.clone());
    eventually(|| script.count.load(Ordering::SeqCst) == 2).await;
    let fresh = p.base.worker(Transport::Local, &["a", "b", "c"]);
    *p.base.connections.workers.lock().unwrap() = vec![
        (Transport::Local, fresh.clone()),
        (support::worker("peer"), peer.clone()),
    ];
    p.h().worker.connected.store(false, Ordering::SeqCst);
    script.closing.wait().await.map_err(test_error)?;
    assert_eq!(home(&fresh, "a") + home(&fresh, "b"), 0);
    if update {
        p.update_workers(vec![
            (Transport::Local, targets(&["b", "c"])),
            (support::worker("peer"), targets(&["peer"])),
        ])
        .await?;
        assert_eq!(p.connected(1).await[0].udid(), "c");
    }
    let next = p.base.worker(support::worker("peer"), &["peer"]);
    *p.base.connections.workers.lock().unwrap() =
        vec![(Transport::Local, fresh.clone()), (support::worker("peer"), next)];
    peer.connected.store(false, Ordering::SeqCst);
    assert_eq!(p.connected(1).await[0].udid(), "peer");
    assert!(!a.is_finished());
    assert!(!b.is_finished());
    gate.complete(Ok(()));
    let _ = a.await?;
    let _ = b.await?;
    let replacements = p.connected(if update { 1 } else { 2 }).await;
    assert!(replacements.iter().any(|d| d.udid() == "b"));
    if update {
        assert_eq!(home(&fresh, "a"), 0);
    }
    p.finish().await
}
