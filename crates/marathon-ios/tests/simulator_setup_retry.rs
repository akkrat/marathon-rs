//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt:372,429–440
//! D33: a simulator whose setup failed is set up again, a bounded number of times (A81). Kotlin has no such retry.
mod provider_support;
mod recovery_support;
mod simulator_support;
mod support;
use marathon_apple::apple_device::AppleDevice;
use marathon_config::devices::{AppleTarget, Transport};
use marathon_core::{
    error::{LeavePoolReason, VendorError, VendorResult},
    executor::shared::SharedDeferred,
    runtime::{Deferred, VendorFuture, VendorRuntime, VendorTask, VendorTaskBody},
    vendor::{DeviceEvent, DeviceProvider},
};
use marathon_ios::device::simulator_provider::AppleSimulatorProvider;
use marathon_test_support::apple::FakeSimulatorWorker;
use provider_support::*;
use recovery_support::*;
use simulator_support::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use support::{duration, watchdog, worker};

type Gate = Arc<SharedDeferred<()>>;

/// `failures` setups fail at `getenv HOME`; the next one waits for `gate`, if there is one.
fn fail_home(p: &RecoveryHarness, target: &FakeSimulatorWorker, failures: usize, gate: Option<Gate>) {
    fail_home_of(p.h(), target, failures, gate)
}
fn fail_home_of(h: &Harness, target: &FakeSimulatorWorker, failures: usize, gate: Option<Gate>) {
    let boot = h.boot.clone();
    let left = Arc::new(AtomicUsize::new(failures));
    *target.before.lock().unwrap() = Arc::new(move |cmd| {
        let answer = boot.answer(&cmd);
        let left = left.clone();
        let gate = gate.clone();
        Box::pin(async move {
            if cmd.get(2).is_some_and(|s| s == "getenv") && cmd.last().is_some_and(|s| s == "HOME") {
                if left
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok()
                {
                    return Err(support::error("setup failed"));
                }
                if let Some(gate) = gate {
                    gate.wait().await?;
                }
            }
            Ok(answer)
        })
    });
}
fn homes(p: &RecoveryHarness) -> usize {
    p.h()
        .worker
        .commands
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.get(2).is_some_and(|s| s == "getenv") && c.last().is_some_and(|s| s == "HOME"))
        .count()
}
fn simctl(p: &RecoveryHarness, verb: &str) -> usize {
    p.h()
        .worker
        .commands
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.len() > 2 && c[0] == "xcrun" && c[1] == "simctl" && c[2] == verb)
        .count()
}
fn attempts_at(p: &RecoveryHarness) -> Vec<i64> {
    p.connections
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, worker, _)| *worker)
        .map(|(_, _, time)| *time)
        .collect()
}
fn queued(p: &RecoveryHarness) -> usize {
    p.provider.subscribe().state().queued
}
const LONG_AFTER: u64 = 400_000;
/// Waits for the failure that starts this retry's pause, then lets the pause pass. A retry runs on its own task, so
/// the failure has to be seen first (docs/testing.md).
async fn pass_pause(p: &RecoveryHarness, retry: i32, millis: u64) {
    eventually(|| p.log(&format!("(retry {retry} of"))).await;
    advance(millis).await;
}
/// Waiting on the paused clock can move it by a few virtual milliseconds.
fn assert_attempts_at(p: &RecoveryHarness, expected: &[i64]) {
    let actual = attempts_at(p);
    assert_eq!(actual.len(), expected.len(), "{actual:?}");
    for (actual, expected) in actual.iter().zip(expected) {
        assert!(
            (*expected..expected + 1_000).contains(actual),
            "{actual} for {expected}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_simulator_whose_setup_failed_is_set_up_again_after_a_pause_and_connects_exactly_once() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    fail_home(&p, &p.h().worker, 2, None);
    p.update(&["a"]).await?;
    assert_eq!(homes(&p), 1);
    assert!(p.provider.devices().is_empty());
    pass_pause(&p, 1, 30_000).await;
    eventually(|| homes(&p) == 2).await;
    pass_pause(&p, 2, 60_000).await;
    let device = p.connected(1).await.remove(0);
    assert_eq!(device.udid(), "a");
    assert_eq!(homes(&p), 3);
    assert_attempts_at(&p, &[0, 30_000, 90_000]);
    advance(LONG_AFTER).await;
    assert_eq!(homes(&p), 3);
    assert_eq!(queued(&p), 0);
    assert_eq!(p.provider.devices().len(), 1);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_that_always_fails_gets_three_retries_at_growing_pauses_and_then_stays_out() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    fail_home(&p, &p.h().worker, usize::MAX, None);
    p.update(&["a"]).await?;
    for (retry, pause) in [(1, 30_000), (2, 60_000), (3, 90_000)] {
        pass_pause(&p, retry, pause).await;
    }
    eventually(|| p.log("setup failed 4 times, it stays out of the run")).await;
    advance(LONG_AFTER).await;
    assert_attempts_at(&p, &[0, 30_000, 90_000, 180_000]);
    assert_eq!(homes(&p), 4);
    assert_eq!(queued(&p), 0);
    assert!(p.provider.devices().is_empty());
    let messages: Vec<String> = p
        .h()
        .logs
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, _, m, _)| m.contains("setting up again in"))
        .map(|(_, _, m, _)| m.clone())
        .collect();
    assert_eq!(
        messages,
        [
            "Worker local: setup of a failed, setting up again in 30 s (retry 1 of 3)",
            "Worker local: setup of a failed, setting up again in 60 s (retry 2 of 3)",
            "Worker local: setup of a failed, setting up again in 90 s (retry 3 of 3)",
        ]
    );
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn unchanged_snapshots_neither_retry_a_failed_setup_nor_refill_its_budget() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    fail_home(&p, &p.h().worker, usize::MAX, None);
    p.update(&["a"]).await?;
    for _ in 0..3 {
        p.update(&["a"]).await?;
    }
    assert_eq!(homes(&p), 1);
    assert_eq!(attempts_at(&p), [0]);
    for (retry, pause) in [(1, 30_000), (2, 60_000), (3, 90_000)] {
        pass_pause(&p, retry, pause).await;
        p.update(&["a"]).await?;
    }
    eventually(|| p.log("setup failed 4 times, it stays out of the run")).await;
    advance(LONG_AFTER).await;
    p.update(&["a"]).await?;
    assert_eq!(homes(&p), 4);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_removed_during_its_pause_is_not_retried_and_starts_over_when_added_back() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    fail_home(&p, &p.h().worker, 1, None);
    p.update(&["a"]).await?;
    advance(10_000).await;
    p.update(&[]).await?;
    advance(LONG_AFTER).await;
    assert_eq!(homes(&p), 1);
    assert_eq!(attempts_at(&p).len(), 1);
    assert_eq!(queued(&p), 0);
    p.update(&["a"]).await?;
    assert_eq!(homes(&p), 2);
    assert_eq!(p.connected(1).await.len(), 1);
    advance(LONG_AFTER).await;
    assert_eq!(homes(&p), 2);
    assert_eq!(queued(&p), 0);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_retry_from_before_a_removal_doesn_t_act_on_the_simulator_added_back_during_its_pause() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    fail_home(&p, &p.h().worker, 2, None);
    p.update(&["a"]).await?;
    advance(10_000).await;
    p.update(&[]).await?;
    p.update(&["a"]).await?;
    assert_eq!(homes(&p), 2);
    // The old pause ends here. The new one, 30 s from the second failure, is still running.
    advance(20_000).await;
    assert_eq!(homes(&p), 2);
    assert_eq!(attempts_at(&p), [0, 10_000]);
    advance(10_000).await;
    assert_eq!(p.connected(1).await.len(), 1);
    assert_eq!(homes(&p), 3);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_removed_during_its_retry_never_connects_and_isn_t_retried_again() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    let gate = SharedDeferred::new();
    fail_home(&p, &p.h().worker, 1, Some(gate.clone()));
    p.update(&["a"]).await?;
    pass_pause(&p, 1, 30_000).await;
    eventually(|| homes(&p) == 2).await;
    p.update(&[]).await?;
    gate.complete(Ok(()));
    eventually(|| p.log("has left the devices file, its new setup is dropped")).await;
    advance(LONG_AFTER).await;
    assert_eq!(homes(&p), 2);
    assert_eq!(queued(&p), 0);
    assert!(p.provider.devices().is_empty());
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_added_back_during_its_retry_has_one_setup_and_one_actor() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    let gate = SharedDeferred::new();
    fail_home(&p, &p.h().worker, 1, Some(gate.clone()));
    p.update(&["a"]).await?;
    pass_pause(&p, 1, 30_000).await;
    eventually(|| homes(&p) == 2).await;
    p.update(&[]).await?;
    // The snapshot that adds it back waits for the attempt on this worker instead of setting it up next to it.
    let (provider, context) = (p.provider.clone(), p.h().services.context.clone());
    let added = tokio::spawn(async move {
        provider
            .process_update(snapshot(vec![(Transport::Local, targets(&["a"]))]), context)
            .await
    });
    settle().await;
    assert!(!added.is_finished());
    assert_eq!(homes(&p), 2);
    gate.complete(Ok(()));
    watchdog(added).await?.map_err(test_error)?;
    assert_eq!(p.connected(1).await.len(), 1);
    advance(LONG_AFTER).await;
    assert_eq!(homes(&p), 2);
    assert_eq!(queued(&p), 0);
    assert_eq!(p.provider.devices().len(), 1);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn failed_simulators_of_one_worker_are_retried_together_in_one_attempt() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.h().worker.add(["b"]);
    fail_home(&p, &p.h().worker, 2, None);
    p.update(&["a", "b"]).await?;
    assert_eq!(homes(&p), 2);
    let devices = p.connected(2).await;
    assert_eq!(devices.len(), 2);
    assert_attempts_at(&p, &[0, 30_000]);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_failed_managed_simulator_is_deleted_and_replaced_so_that_retries_leave_only_one() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.h().worker.simulators.lock().unwrap().clear();
    fail_home(&p, &p.h().worker, 2, None);
    p.update_workers(vec![(
        Transport::Local,
        vec![AppleTarget::SimulatorProfile(support::profile())],
    )])
    .await?;
    assert_eq!((simctl(&p, "create"), simctl(&p, "delete")), (1, 1));
    pass_pause(&p, 1, 30_000).await;
    pass_pause(&p, 2, 60_000).await;
    assert_eq!(p.connected(1).await.len(), 1);
    assert_eq!((simctl(&p, "create"), simctl(&p, "delete")), (3, 2));
    assert_eq!(p.h().worker.simulators.lock().unwrap().len(), 1);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_managed_simulator_that_couldn_t_be_deleted_is_reused_instead_of_creating_another() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.h().worker.simulators.lock().unwrap().clear();
    let boot = p.h().boot.clone();
    let left = Arc::new(AtomicUsize::new(2));
    *p.h().worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let answer = boot.answer(&cmd);
        let left = left.clone();
        Box::pin(async move {
            if cmd.get(2).is_some_and(|s| s == "delete") {
                return Err(support::error("delete failed"));
            }
            if cmd.get(2).is_some_and(|s| s == "getenv")
                && cmd.last().is_some_and(|s| s == "HOME")
                && left
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok()
            {
                return Err(support::error("setup failed"));
            }
            Ok(answer)
        })
    });
    p.update_workers(vec![(
        Transport::Local,
        vec![AppleTarget::SimulatorProfile(support::profile())],
    )])
    .await?;
    pass_pause(&p, 1, 30_000).await;
    pass_pause(&p, 2, 60_000).await;
    assert_eq!(p.connected(1).await.len(), 1);
    assert_eq!(simctl(&p, "create"), 1);
    assert_eq!(p.h().worker.simulators.lock().unwrap().len(), 1);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn the_instances_of_a_profile_share_one_budget_and_only_failed_ones_are_created_again() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.h().worker.simulators.lock().unwrap().clear();
    fail_home(&p, &p.h().worker, usize::MAX, None);
    p.update_workers(vec![(
        Transport::Local,
        vec![AppleTarget::SimulatorProfile(support::profile()); 2],
    )])
    .await?;
    for (retry, pause) in [(1, 30_000), (2, 60_000), (3, 90_000)] {
        pass_pause(&p, retry, pause).await;
    }
    eventually(|| p.log("setup failed 4 times, it stays out of the run")).await;
    advance(LONG_AFTER).await;
    assert_eq!(attempts_at(&p).len(), 4);
    assert_eq!(simctl(&p, "create"), 8);
    assert_eq!(simctl(&p, "delete"), 8);
    assert!(p.h().worker.simulators.lock().unwrap().is_empty());
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn terminating_cancels_a_pending_retry_without_waiting_for_its_pause() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    fail_home(&p, &p.h().worker, usize::MAX, None);
    p.update(&["a"]).await?;
    let before = p.h().clock.now();
    watchdog(p.provider.terminate()).await.map_err(test_error)?;
    assert_eq!(p.h().clock.now(), before);
    advance(LONG_AFTER).await;
    assert_eq!(homes(&p), 1);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn terminating_doesn_t_wait_for_a_retry_blocked_on_its_connection() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    fail_home(&p, &p.h().worker, 1, None);
    let (entered, gate) = (SharedDeferred::new(), SharedDeferred::new());
    let (e, g) = (entered.clone(), gate.clone());
    p.answer(move |_, w, n| {
        let (e, g) = (e.clone(), g.clone());
        Box::pin(async move {
            if w && n == 2 {
                e.complete(Ok(()));
                g.wait()
                    .await
                    .map_err(marathon_apple::device::connection_factory::ConnectionError::from)?;
            }
            Ok(None)
        })
    });
    p.update(&["a"]).await?;
    pass_pause(&p, 1, 30_000).await;
    watchdog(entered.wait()).await.map_err(test_error)?;
    watchdog(p.provider.terminate()).await.map_err(test_error)?;
    gate.complete(Ok(()));
    eventually(|| {
        p.base.connections.acquired.load(Ordering::SeqCst) == p.base.connections.released.load(Ordering::SeqCst)
    })
    .await;
    assert_eq!(homes(&p), 1);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_retry_blocked_on_one_worker_doesn_t_hold_up_the_setup_of_another() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    let slow = p.base.worker(worker("slow"), &["x"]);
    p.base.worker(worker("healthy"), &["y"]);
    let gate = SharedDeferred::new();
    fail_home(&p, &slow, 1, Some(gate.clone()));
    p.update_workers(vec![(worker("slow"), targets(&["x"]))]).await?;
    pass_pause(&p, 1, 30_000).await;
    eventually(|| home(&slow, "x") == 2).await;
    p.update_workers(vec![
        (worker("slow"), targets(&["x"])),
        (worker("healthy"), targets(&["y"])),
    ])
    .await?;
    assert!(p.connected(1).await[0].serial().starts_with("y@"));
    gate.complete(Ok(()));
    assert!(p.connected(1).await[0].serial().starts_with("x@"));
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn an_unreachable_worker_during_a_retry_goes_to_its_connection_retry_without_refilling_the_budget() -> TestResult
{
    let p = RecoveryHarness::settings(
        |v| {
            v.worker_connection.backoff = duration(50);
            v.worker_connection.retry_interval = duration(100);
        },
        2,
        1_000,
    )?;
    fail_home(&p, &p.h().worker, usize::MAX, None);
    p.answer(|_, w, n| Box::pin(async move { if w && n == 2 { Err(unreachable()) } else { Ok(None) } }));
    p.update(&["a"]).await?;
    pass_pause(&p, 1, 1_000).await;
    eventually(|| p.log("Failed to connect to worker local (No route to host), retrying")).await;
    assert_eq!(homes(&p), 1);
    while !p.log("setup failed 3 times, it stays out of the run") {
        advance(100).await;
    }
    advance(LONG_AFTER).await;
    // The first setup, one through the worker retry, then the last retry of the budget of 2.
    assert_eq!(homes(&p), 3);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_worker_configured_for_no_retries_gets_no_retry_of_a_failed_setup() -> TestResult {
    let p = RecoveryHarness::new(|v| {
        v.worker_connection.attempts = 1;
        v.worker_connection.late_join = false;
    })?;
    fail_home(&p, &p.h().worker, 1, None);
    p.update(&["a"]).await?;
    advance(LONG_AFTER).await;
    assert_eq!(homes(&p), 1);
    assert_eq!(attempts_at(&p).len(), 1);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_refused_by_authentication_or_missing_from_the_worker_is_not_retried() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.answer(|_, w, _| Box::pin(async move { if w { Ok(None) } else { Err(auth()) } }));
    p.update(&["a", "not-on-this-worker"]).await?;
    assert!(p.log("udid not-on-this-worker is not available"));
    advance(LONG_AFTER).await;
    assert_eq!(attempts_at(&p).len(), 1);
    assert_eq!(homes(&p), 0);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_failed_plan_on_a_connected_worker_is_retried_for_all_of_its_simulators() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.h().worker.add(["b"]);
    let boot = p.h().boot.clone();
    let failed = Arc::new(AtomicBool::new(false));
    *p.h().worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let answer = boot.answer(&cmd);
        let failed = failed.clone();
        Box::pin(async move {
            if cmd.get(2).is_some_and(|s| s == "list") && !failed.swap(true, Ordering::SeqCst) {
                return Err(support::error("list failed"));
            }
            Ok(answer)
        })
    });
    p.update(&["a", "b"]).await?;
    assert!(p.log("Failed to initialize worker"));
    assert!(p.provider.devices().is_empty());
    assert_eq!(p.connected(2).await.len(), 2);
    assert_attempts_at(&p, &[0, 30_000]);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn after_the_late_join_cut_off_a_failed_setup_is_no_longer_retried() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    fail_home(&p, &p.h().worker, 1, None);
    p.update(&["a"]).await?;
    p.time.wall_offset.store(600_001, Ordering::SeqCst);
    pass_pause(&p, 1, 30_000).await;
    eventually(|| p.log("doesn't join the run late, since the run started more than 600 s ago")).await;
    advance(LONG_AFTER).await;
    assert_eq!(homes(&p), 1);
    assert_eq!(attempts_at(&p).len(), 1);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_retried_simulator_ready_after_the_cut_off_leaves_before_its_first_batch() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    fail_home(&p, &p.h().worker, 1, None);
    p.update(&["a"]).await?;
    let device = p.connected(1).await.remove(0);
    p.time.wall_offset.store(600_001, Ordering::SeqCst);
    let error = p.h().prepare(&device).await.unwrap_err();
    assert!(matches!(
        error,
        VendorError::LeavePool {
            reason: LeavePoolReason::NotAdmitted,
            ..
        }
    ));
    assert!(device.retired());
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn borrow_gets_the_simulator_once_its_retry_has_set_it_up() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    fail_home(&p, &p.h().worker, 1, None);
    p.update(&["a"]).await?;
    let borrowed = watchdog(p.provider.borrow(p.h().services.context.clone()))
        .await
        .map_err(test_error)?;
    assert_eq!(
        p.connected(1).await[0].serial(),
        borrowed.0.info().serial_number.to_string_lossy()
    );
    p.balanced().await
}

#[tokio::test(start_paused = true)]
async fn a_healthy_simulator_added_while_a_retry_waits_for_its_turn_is_not_a_late_one() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.h().worker.add(["b"]);
    let boot = p.h().boot.clone();
    let (left, gate) = (Arc::new(AtomicUsize::new(1)), SharedDeferred::new());
    let g = gate.clone();
    *p.h().worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let answer = boot.answer(&cmd);
        let (left, g) = (left.clone(), g.clone());
        Box::pin(async move {
            if cmd.get(2).is_some_and(|s| s == "getenv") && cmd.last().is_some_and(|s| s == "HOME") {
                if cmd.get(3).is_some_and(|s| s == "b") {
                    g.wait().await?;
                } else if left
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok()
                {
                    return Err(support::error("setup failed"));
                }
            }
            Ok(answer)
        })
    });
    p.update(&["a"]).await?;
    eventually(|| p.log("(retry 1 of")).await;
    // b's first cycle holds the worker while a's retry connects and waits for its turn.
    let (provider, context) = (p.provider.clone(), p.h().services.context.clone());
    let added = tokio::spawn(async move {
        provider
            .process_update(snapshot(vec![(Transport::Local, targets(&["a", "b"]))]), context)
            .await
    });
    eventually(|| home(&p.h().worker, "b") == 1).await;
    advance(30_000).await;
    eventually(|| attempts_at(&p).len() == 3).await;
    gate.complete(Ok(()));
    watchdog(added).await?.map_err(test_error)?;
    let b = p.connected(1).await.remove(0);
    assert_eq!(b.udid(), "b");
    p.time.wall_offset.store(600_001, Ordering::SeqCst);
    p.h().prepare(&b).await.map_err(test_error)?;
    assert!(!b.retired());
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_failed_release_of_the_planning_lease_keeps_the_retry_of_the_first_cycle() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    fail_home(&p, &p.h().worker, 1, None);
    // The failed simulator's own lease goes first, then the planning lease of the cycle.
    p.base.connections.fail_release.store(2, Ordering::SeqCst);
    p.update(&["a"]).await?;
    assert!(p.log("Failed to initialize worker"));
    assert!(p.log("release failed"));
    assert_eq!(p.connected(1).await.len(), 1);
    assert_eq!(homes(&p), 2);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_removal_during_a_predecessor_s_disposal_follows_the_replacement_s_connect_and_holds_up_nobody_else()
-> TestResult {
    use marathon_ios::device::{
        simulator_factory::SimulatorCreating, simulator_provisioner::ProvisioningConnections,
        worker_provisioning_context::WorkerProvisioningContext,
    };
    let p = ProviderHarness::new(|_| {})?;
    p.h.worker.add(["b"]);
    p.update(&["a"]).await?;
    let old = p.connected(1).await.remove(0);
    let gate = SharedDeferred::new();
    let (script, job) = batch(&p.h, old, Some(gate.clone())).await;
    p.connections
        .connect(Transport::Local, p.h.services.context.clone())
        .await
        .map_err(test_error)?;
    let replacement =
        p.h.factory()
            .create(
                Transport::Local,
                p.h.worker.clone(),
                Arc::new(support::BridgeMock),
                "a".into(),
                Arc::new(WorkerProvisioningContext::new(Transport::Local, p.h.clock.clone())),
                p.h.services.context.clone(),
            )
            .await?;
    let device = replacement.device.clone();
    let provider = p.provider.clone();
    let registration = tokio::spawn(async move { provider.register("a@local".into(), replacement).await });
    script.closing.wait().await.map_err(test_error)?;
    p.update(&["b"]).await?;
    // The disconnect waits for the connect of its own device only.
    assert_eq!(p.connected(1).await[0].udid(), "b");
    assert_eq!(p.provider.subscribe().state().queued, 0);
    gate.complete(Ok(()));
    registration.await??;
    let _ = job.await?;
    let channel = p.provider.subscribe();
    let first = watchdog(channel.receive()).await.map_err(test_error)?;
    let second = watchdog(channel.receive()).await.map_err(test_error)?;
    assert!(matches!(&first, Some(DeviceEvent::Connected(d)) if *d == device));
    assert!(matches!(&second, Some(DeviceEvent::Disconnected(d)) if *d == device));
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn a_lost_device_s_disconnect_doesn_t_overtake_its_queued_connect() -> TestResult {
    use marathon_ios::device::{
        simulator_factory::SimulatorCreating, simulator_provider::ProviderRecovery,
        simulator_provider::recovery::SimulatorRecovery, worker_provisioning_context::WorkerProvisioningContext,
    };
    let base = ProviderHarness::new(|_| {})?;
    let runtime = Arc::new(HeldRuntime::default());
    let recovery = SimulatorRecovery::new();
    let mut services = ProviderHarness::services(&base.h, base.connections.clone(), recovery.clone());
    services.runtime = runtime.clone();
    let provider = AppleSimulatorProvider::new(services)?;
    let device = base
        .h
        .factory()
        .create(
            Transport::Local,
            base.h.worker.clone(),
            Arc::new(support::BridgeMock),
            "a".into(),
            Arc::new(WorkerProvisioningContext::new(Transport::Local, base.h.clock.clone())),
            base.h.services.context.clone(),
        )
        .await?;
    provider.register("a@local".into(), device).await?;
    base.h.worker.connected.store(false, Ordering::SeqCst);
    recovery
        .reconnect(provider.clone(), base.h.services.context.clone())
        .await
        .map_err(test_error)?;
    runtime.pass_next.store(usize::MAX, Ordering::SeqCst);
    runtime.release().await.map_err(test_error)?;
    let channel = provider.subscribe();
    let first = watchdog(channel.receive()).await.map_err(test_error)?;
    let second = watchdog(channel.receive()).await.map_err(test_error)?;
    assert!(matches!(first, Some(DeviceEvent::Connected(_))));
    assert!(matches!(second, Some(DeviceEvent::Disconnected(_))));
    watchdog(provider.terminate()).await.map_err(test_error)?;
    Ok(())
}

/// Keeps what is spawned until `release`, which runs it newest first. The next `pass_next` spawns go straight through.
#[derive(Default)]
struct HeldRuntime {
    held: Mutex<Vec<VendorFuture<'static, ()>>>,
    pass_next: AtomicUsize,
}
struct NoTask;
impl VendorTask for NoTask {
    fn cancel(&self, _: marathon_core::error::Cancellation) {}
    fn join(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
impl HeldRuntime {
    async fn release(&self) -> VendorResult<()> {
        let mut held = std::mem::take(&mut *self.held.lock().unwrap());
        while let Some(task) = held.pop() {
            task.await?;
        }
        Ok(())
    }
}
impl VendorRuntime for HeldRuntime {
    fn spawn_cancellable(&self, body: VendorTaskBody) -> Arc<dyn VendorTask> {
        support::Runtime.spawn_cancellable(body)
    }
    fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn VendorTask> {
        if self
            .pass_next
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return support::Runtime.spawn(body);
        }
        self.held.lock().unwrap().push(body);
        Arc::new(NoTask)
    }
    fn spawn_blocking(&self, body: Box<dyn FnOnce() -> VendorResult<()> + Send>) -> Arc<dyn VendorTask> {
        support::Runtime.spawn_blocking(body)
    }
}
#[tokio::test(start_paused = true)]
async fn a_connect_raised_before_a_removal_is_published_first_whatever_order_their_tasks_run_in() -> TestResult {
    use marathon_ios::device::{
        simulator_factory::SimulatorCreating, worker_provisioning_context::WorkerProvisioningContext,
    };
    let base = ProviderHarness::new(|_| {})?;
    let runtime = Arc::new(HeldRuntime::default());
    runtime.pass_next.store(usize::MAX, Ordering::SeqCst);
    let mut services = ProviderHarness::services(&base.h, base.connections.clone(), base.recovery.clone());
    services.runtime = runtime.clone();
    let provider = AppleSimulatorProvider::new(services)?;
    // A failed setup leaves the simulator tracked and unregistered, so the empty snapshot below removes it.
    fail_home_of(&base.h, &base.h.worker, 1, None);
    provider
        .process_update(
            snapshot(vec![(Transport::Local, targets(&["a"]))]),
            base.h.services.context.clone(),
        )
        .await
        .map_err(test_error)?;
    assert_eq!(base.recovery.failed.lock().unwrap().len(), 1);
    let device = base
        .h
        .factory()
        .create(
            Transport::Local,
            base.h.worker.clone(),
            Arc::new(support::BridgeMock),
            "a".into(),
            Arc::new(WorkerProvisioningContext::new(Transport::Local, base.h.clock.clone())),
            base.h.services.context.clone(),
        )
        .await?;
    // The connect's task stays held; only the snapshot's own task runs, and it holds its disconnect too.
    runtime.pass_next.store(0, Ordering::SeqCst);
    provider.register("a@local".into(), device).await?;
    runtime.pass_next.store(1, Ordering::SeqCst);
    provider
        .process_update(snapshot(vec![]), base.h.services.context.clone())
        .await
        .map_err(test_error)?;
    assert_eq!(runtime.held.lock().unwrap().len(), 2);
    runtime.release().await.map_err(test_error)?;
    let channel = provider.subscribe();
    let first = channel.receive().await.map_err(test_error)?;
    let second = channel.receive().await.map_err(test_error)?;
    assert!(matches!(first, Some(DeviceEvent::Connected(_))));
    assert!(matches!(second, Some(DeviceEvent::Disconnected(_))));
    Ok(())
}
