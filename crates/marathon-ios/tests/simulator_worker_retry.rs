//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProviderWorkerRetryTest.kt
mod provider_support;
mod recovery_support;
mod simulator_support;
mod support;
use marathon_apple::apple_device::AppleDevice;
use marathon_config::devices::{AppleTarget, Transport};
use marathon_core::{executor::shared::SharedDeferred, runtime::Deferred, vendor::DeviceProvider};
use provider_support::*;
use recovery_support::*;
use simulator_support::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use support::{duration, watchdog, worker};

#[tokio::test(start_paused = true)]
async fn a_worker_unreachable_at_start_up_joins_once_it_connects_and_healthy_workers_don_t_wait_for_it() -> TestResult {
    let p = RecoveryHarness::fast()?;
    let late_worker = p.base.worker(worker("late"), &["a"]);
    p.base.worker(worker("healthy"), &["b"]);
    let gate = SharedDeferred::new();
    let g = gate.clone();
    p.answer(move |t, w, n| {
        let g = g.clone();
        Box::pin(async move {
            if t == worker("late") && w {
                if n <= 2 {
                    return Err(unreachable());
                }
                g.wait()
                    .await
                    .map_err(marathon_apple::device::connection_factory::ConnectionError::from)?;
            }
            Ok(None)
        })
    });
    p.update_workers(vec![
        (worker("late"), targets(&["a"])),
        (worker("healthy"), targets(&["b"])),
    ])
    .await?;
    assert!(p.connected(1).await[0].serial().starts_with("b@"));
    gate.complete(Ok(()));
    let late = p.connected(1).await.remove(0);
    p.h().prepare(&late).await.map_err(test_error)?;
    let script = Arc::new(BatchScript::default());
    script.output(passed(1));
    script.attach(&late_worker);
    let outcome = run(p.h(), &late).await;
    assert!(outcome.0.is_ok());
    assert!(outcome.1.is_some());
    assert_eq!(p.connections.attempts(&worker("late"), true), 3);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_worker_that_stays_unreachable_is_retried_in_the_background_until_the_run_ends() -> TestResult {
    let p = RecoveryHarness::fast()?;
    p.answer(|_, _, _| Box::pin(async { Err(unreachable()) }));
    p.update(&["a"]).await?;
    eventually(|| p.connections.attempts(&Transport::Local, true) >= 5).await;
    p.finish().await?;
    let n = p.connections.attempts(&Transport::Local, true);
    advance(500).await;
    assert_eq!(p.connections.attempts(&Transport::Local, true), n);
    assert!(p.provider.devices().is_empty());
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn without_late_join_a_worker_is_given_up_after_its_attempts() -> TestResult {
    let p = RecoveryHarness::new(|v| {
        v.worker_connection.late_join = false;
        v.worker_connection.backoff = duration(50);
        v.worker_connection.retry_interval = duration(100);
    })?;
    p.answer(|_, _, _| Box::pin(async { Err(unreachable()) }));
    p.update(&["a"]).await?;
    advance(50).await;
    advance(100).await;
    advance(500).await;
    assert_eq!(p.connections.attempts(&Transport::Local, true), 3);
    assert!(p.log("after 3 attempts"));
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn authentication_and_configuration_errors_are_not_retried_also_when_they_follow_a_network_failure() -> TestResult
{
    let p = RecoveryHarness::fast()?;
    p.answer(|t, _, n| {
        Box::pin(async move {
            if t == worker("network") && n == 1 {
                Err(unreachable())
            } else if t == worker("config") {
                Err(support::error("no ssh auth provided for config:22").into())
            } else {
                Err(auth())
            }
        })
    });
    p.update_workers(vec![
        (worker("auth"), targets(&["a"])),
        (worker("config"), targets(&["b"])),
        (worker("network"), targets(&["c"])),
    ])
    .await?;
    advance(500).await;
    assert_eq!(
        [
            p.connections.attempts(&worker("auth"), true),
            p.connections.attempts(&worker("config"), true),
            p.connections.attempts(&worker("network"), true)
        ],
        [1, 1, 2]
    );
    p.balanced().await
}
async fn blocked_acquisition(simulator: bool) -> TestResult {
    let p = RecoveryHarness::fast()?;
    let entered = SharedDeferred::new();
    let gate = SharedDeferred::new();
    let e = entered.clone();
    let g = gate.clone();
    p.answer(move |_, w, n| {
        let e = e.clone();
        let g = g.clone();
        Box::pin(async move {
            if w && n == 1 {
                return Err(unreachable());
            }
            if w != simulator {
                e.complete(Ok(()));
                g.wait()
                    .await
                    .map_err(marathon_apple::device::connection_factory::ConnectionError::from)?;
            }
            Ok(None)
        })
    });
    p.update(&["a"]).await?;
    watchdog(entered.wait()).await.map_err(test_error)?;
    watchdog(p.provider.terminate()).await.map_err(test_error)?;
    assert!(p.provider.devices().is_empty());
    gate.complete(Ok(()));
    settle().await;
    eventually(|| {
        p.base.connections.acquired.load(Ordering::SeqCst) == p.base.connections.released.load(Ordering::SeqCst)
    })
    .await;
    assert!(p.provider.subscribe().receive().await.map_err(test_error)?.is_none());
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn the_end_of_the_run_doesn_t_wait_for_a_blocked_attempt_and_releases_the_connection_it_opens_afterwards()
-> TestResult {
    blocked_acquisition(false).await
}
#[tokio::test(start_paused = true)]
async fn the_end_of_the_run_doesn_t_wait_for_a_blocked_connection_of_a_late_simulator_setup() -> TestResult {
    blocked_acquisition(true).await
}
#[tokio::test(start_paused = true)]
async fn devices_that_an_update_adds_to_an_unreachable_worker_are_set_up_once_it_connects() -> TestResult {
    let p = RecoveryHarness::fast()?;
    let gate = SharedDeferred::new();
    let g = gate.clone();
    let entered = SharedDeferred::new();
    let e = entered.clone();
    p.answer(move |_, w, n| {
        let g = g.clone();
        let e = e.clone();
        Box::pin(async move {
            if w && n == 1 {
                return Err(unreachable());
            }
            if w {
                e.complete(Ok(()));
                g.wait()
                    .await
                    .map_err(marathon_apple::device::connection_factory::ConnectionError::from)?;
            }
            Ok(None)
        })
    });
    p.update(&["a"]).await?;
    entered.wait().await.map_err(test_error)?;
    p.update(&["a", "b"]).await?;
    gate.complete(Ok(()));
    assert_eq!(p.connected(2).await.len(), 2);
    assert_eq!(p.connections.attempts(&Transport::Local, true), 2);
    assert_eq!(home(&p.h().worker, "a"), 1);
    assert_eq!(home(&p.h().worker, "b"), 1);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_connection_lost_while_the_late_worker_s_plan_is_made_is_retried() -> TestResult {
    let p = RecoveryHarness::fast()?;
    p.fail_initial();
    let original = p.h().worker.clone();
    let fresh = p.base.worker(Transport::Local, &["a"]);
    // Replace the default worker after the failed late inventory.
    let workers = p.base.connections.clone();
    let old = original.clone();
    let new = fresh.clone();
    *original.before.lock().unwrap() = Arc::new(move |cmd| {
        let workers = workers.clone();
        let old = old.clone();
        let new = new.clone();
        Box::pin(async move {
            if cmd == marathon_test_support::apple::fake_simulator_worker::LIST {
                old.connected.store(false, Ordering::SeqCst);
                *workers.workers.lock().unwrap() = vec![(Transport::Local, new)];
                return Err(support::error("channel closed"));
            }
            Ok(None)
        })
    });
    p.update(&["a"]).await?;
    assert_eq!(p.connected(1).await[0].udid(), "a");
    assert_eq!(p.connections.attempts(&Transport::Local, true), 3);
    assert!(p.log("while planning its simulators"));
    p.balanced().await
}
async fn setup_failure(profile: bool, authentication: bool, disconnected: bool) -> TestResult {
    let p = RecoveryHarness::fast()?;
    if profile {
        p.h().worker.simulators.lock().unwrap().clear();
    }
    let dead = p.base.worker(worker("dead"), &["a", "b"]);
    dead.connected.store(false, Ordering::SeqCst);
    *dead.before.lock().unwrap() = Arc::new(|_| Box::pin(async { Err(support::error("channel closed")) }));
    p.answer(move |_, w, n| {
        let dead = dead.clone();
        Box::pin(async move {
            if w && profile && n == 1 {
                return Err(unreachable());
            }
            if !w && n == 2 {
                if authentication {
                    return Err(auth());
                }
                if disconnected {
                    return Ok(Some(dead as marathon_apple::device::connection_factory::Executor));
                }
                return Err(unreachable());
            }
            Ok(None)
        })
    });
    if profile {
        p.update_workers(vec![(
            Transport::Local,
            vec![AppleTarget::SimulatorProfile(support::profile()); 2],
        )])
        .await?;
    } else {
        p.update(&["a", "b"]).await?;
    }
    assert_eq!(
        p.connected(if authentication { 1 } else { 2 }).await.len(),
        if authentication { 1 } else { 2 }
    );
    advance(400).await;
    assert_eq!(
        p.connections.attempts(&Transport::Local, true),
        if authentication {
            1
        } else if profile {
            3
        } else {
            2
        }
    );
    if profile {
        let commands = p.h().worker.commands.lock().unwrap();
        assert_eq!(
            commands
                .iter()
                .filter(|c| c.starts_with(&["xcrun".into(), "simctl".into(), "create".into()]))
                .count(),
            3
        );
        assert_eq!(
            commands
                .iter()
                .filter(|c| c.starts_with(&["xcrun".into(), "simctl".into(), "delete".into()]))
                .count(),
            1
        );
    }
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_whose_connection_fails_at_start_up_is_set_up_once_its_worker_connects_again() -> TestResult {
    setup_failure(false, false, false).await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_whose_own_connection_is_lost_during_its_setup_is_set_up_again_while_the_worker_stays_connected()
-> TestResult {
    setup_failure(false, false, true).await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_whose_connection_fails_to_authenticate_is_not_retried() -> TestResult {
    setup_failure(false, true, false).await
}
#[tokio::test(start_paused = true)]
async fn only_the_simulator_profiles_that_weren_t_set_up_are_created_again() -> TestResult {
    setup_failure(true, false, false).await
}
#[tokio::test(start_paused = true)]
async fn a_worker_that_connects_after_the_late_join_cut_off_doesn_t_join_and_is_no_longer_retried() -> TestResult {
    let p = RecoveryHarness::fast()?;
    let time = p.time.clone();
    p.answer(move |_, w, n| {
        let t = time.clone();
        Box::pin(async move {
            if w && n == 1 {
                return Err(unreachable());
            }
            t.wall_offset.store(600_001, Ordering::SeqCst);
            Ok(None)
        })
    });
    p.update(&["a"]).await?;
    advance(50).await;
    advance(500).await;
    eventually(|| p.log("No longer connecting to it")).await;
    assert!(p.provider.devices().is_empty());
    assert!(p.log("No longer connecting to it"));
    assert_eq!(p.connections.attempts(&Transport::Local, true), 2);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_late_simulator_that_is_ready_after_the_cut_off_doesn_t_join() -> TestResult {
    let p = RecoveryHarness::fast()?;
    p.fail_initial();
    let time = p.time.clone();
    let boot = p.h().boot.clone();
    *p.h().worker.before.lock().unwrap() = Arc::new(move |c| {
        if c.last().is_some_and(|s| s == "HOME") {
            time.wall_offset.store(600_001, Ordering::SeqCst);
        }
        let answer = boot.answer(&c);
        Box::pin(async move { Ok(answer) })
    });
    p.update(&["a"]).await?;
    advance(50).await;
    advance(500).await;
    eventually(|| p.log("doesn't join the run late")).await;
    assert!(p.provider.devices().is_empty());
    assert!(p.log("doesn't join the run late"));
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn late_join_stops_once_a_simulator_of_the_run_has_waited_for_a_batch() -> TestResult {
    let p = RecoveryHarness::fast()?;
    p.update(&["b"]).await?;
    let healthy = p.connected(1).await.remove(0);
    let script = Arc::new(BatchScript::default());
    script.output(passed(1));
    script.attach(&p.h().worker);
    p.h().prepare(&healthy).await.map_err(test_error)?;
    let _ = run(p.h(), &healthy).await;
    assert!(healthy.waiting_since().is_some());
    p.answer(|_, w, _| Box::pin(async move { if w { Err(unreachable()) } else { Ok(None) } }));
    p.update(&["a", "b"]).await?;
    p.time.nano_offset.store(30_001_000_000, Ordering::SeqCst);
    advance(100).await;
    eventually(|| p.log("has been waiting for a batch for more than 30 s")).await;
    assert!(p.log("has been waiting for a batch for more than 30 s"));
    let n = p.connections.attempts(&Transport::Local, true);
    advance(500).await;
    assert_eq!(p.connections.attempts(&Transport::Local, true), n);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn once_late_join_has_stopped_it_stays_stopped_also_for_the_worker_s_other_simulators() -> TestResult {
    let p = RecoveryHarness::new(|v| v.threading_configuration.provisioning_concurrency = 1)?;
    p.fail_initial();
    let time = p.time.clone();
    let boot = p.h().boot.clone();
    let n = Arc::new(AtomicUsize::new(0));
    *p.h().worker.before.lock().unwrap() = Arc::new(move |c| {
        if c.last().is_some_and(|s| s == "HOME") {
            time.wall_offset.store(
                if n.fetch_add(1, Ordering::SeqCst) == 0 {
                    600_001
                } else {
                    -600_000
                },
                Ordering::SeqCst,
            );
        }
        let answer = boot.answer(&c);
        Box::pin(async move { Ok(answer) })
    });
    p.update(&["a", "b"]).await?;
    advance(10_000).await;
    // Inventory parsing runs on the blocking pool; scheduler yields cannot join it.
    // This timing line follows both setups, their admission checks and cleanup.
    eventually(|| p.log("connected late on attempt 2")).await;
    assert_eq!(home(&p.h().worker, "a"), 1);
    assert_eq!(home(&p.h().worker, "b"), 1);
    assert!(p.provider.devices().is_empty());
    assert!(p.time.wall_offset.load(Ordering::SeqCst) < 0);
    assert!(p.recovery.late_join_cutoff().map_err(test_error)?.is_some());
    advance(60_000).await;
    assert_eq!(p.connections.attempts(&Transport::Local, true), 2);
    assert_eq!(home(&p.h().worker, "a"), 1);
    assert_eq!(home(&p.h().worker, "b"), 1);
    assert!(p.provider.devices().is_empty());
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_pending_worker_whose_devices_have_all_left_the_devices_file_is_not_planned_when_it_connects() -> TestResult {
    let p = RecoveryHarness::fast()?;
    let gate = SharedDeferred::new();
    let entered = SharedDeferred::new();
    let g = gate.clone();
    let e = entered.clone();
    p.answer(move |_, w, n| {
        let g = g.clone();
        let e = e.clone();
        Box::pin(async move {
            if w && n == 1 {
                return Err(unreachable());
            }
            if w {
                e.complete(Ok(()));
                g.wait()
                    .await
                    .map_err(marathon_apple::device::connection_factory::ConnectionError::from)?;
            }
            Ok(None)
        })
    });
    p.update(&["a"]).await?;
    entered.wait().await.map_err(test_error)?;
    p.update(&[]).await?;
    gate.complete(Ok(()));
    settle().await;
    assert!(p.h().worker.commands.lock().unwrap().is_empty());
    assert!(p.provider.devices().is_empty());
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_profile_whose_creation_loses_the_late_worker_s_connection_is_created_on_the_next_connection() -> TestResult {
    let p = RecoveryHarness::fast()?;
    p.fail_initial();
    p.h().worker.simulators.lock().unwrap().clear();
    let old = p.h().worker.clone();
    let new = p.base.worker(Transport::Local, &[]);
    let workers = p.base.connections.clone();
    let previous = old.clone();
    let boot = p.h().boot.clone();
    *old.before.lock().unwrap() = Arc::new(move |c| {
        let fail = c.starts_with(&["xcrun".into(), "simctl".into(), "create".into()]);
        let answer = boot.answer(&c);
        if fail {
            previous.connected.store(false, Ordering::SeqCst);
            *workers.workers.lock().unwrap() = vec![(Transport::Local, new.clone())];
        }
        Box::pin(async move {
            if fail {
                Err(support::error("channel closed"))
            } else {
                Ok(answer)
            }
        })
    });
    p.update_workers(vec![(
        Transport::Local,
        vec![AppleTarget::SimulatorProfile(support::profile())],
    )])
    .await?;
    assert_eq!(p.connected(1).await.len(), 1);
    assert_eq!(p.connections.attempts(&Transport::Local, true), 3);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn the_replacement_of_a_late_simulator_that_was_lost_before_its_admission_is_admitted_too() -> TestResult {
    let p = RecoveryHarness::fast()?;
    p.fail_initial();
    p.update(&["a"]).await?;
    let old = p.connected(1).await.remove(0);
    p.lose(&old).await;
    p.scan().await;
    let replacement = p.connected(1).await.remove(0);
    assert!(!Arc::ptr_eq(&old, &replacement));
    p.time.wall_offset.store(600_001, Ordering::SeqCst);
    let err = p.h().prepare(&replacement).await.unwrap_err();
    assert!(matches!(
        err,
        marathon_core::error::VendorError::LeavePool {
            reason: marathon_core::error::LeavePoolReason::NotAdmitted,
            ..
        }
    ));
    assert!(replacement.retired());
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_late_simulator_lost_before_its_admission_after_the_cut_off_is_not_set_up_again() -> TestResult {
    let p = RecoveryHarness::fast()?;
    p.fail_initial();
    p.update(&["a"]).await?;
    let old = p.connected(1).await.remove(0);
    p.lose(&old).await;
    p.time.wall_offset.store(600_001, Ordering::SeqCst);
    p.scan().await;
    eventually(|| old.retired()).await;
    assert!(Arc::ptr_eq(&old, &p.current("a")));
    assert_eq!(home(&p.h().worker, "a"), 1);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn retries_of_unreachable_workers_hold_at_most_two_connection_attempts_at_a_time() -> TestResult {
    let p = RecoveryHarness::fast()?;
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let gate = SharedDeferred::new();
    let a = active.clone();
    let pk = peak.clone();
    let g = gate.clone();
    p.answer(move |_, _, n| {
        let a = a.clone();
        let pk = pk.clone();
        let g = g.clone();
        Box::pin(async move {
            if n == 1 {
                return Err(unreachable());
            }
            pk.fetch_max(a.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
            g.wait()
                .await
                .map_err(marathon_apple::device::connection_factory::ConnectionError::from)?;
            a.fetch_sub(1, Ordering::SeqCst);
            Err(unreachable())
        })
    });
    p.update_workers((1..=4).map(|n| (worker(&format!("w{n}")), targets(&["a"]))).collect())
        .await?;
    eventually(|| active.load(Ordering::SeqCst) == 2).await;
    advance(300).await;
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    assert_eq!(p.connections.calls.lock().unwrap().len(), 6);
    p.finish().await?;
    gate.complete(Ok(()));
    settle().await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_connection_lost_after_the_late_worker_s_plan_is_retried_for_the_simulators_that_weren_t_set_up() -> TestResult
{
    let p = RecoveryHarness::new(|v| {
        v.threading_configuration.provisioning_concurrency = 1;
        v.worker_connection.backoff = duration(50);
        v.worker_connection.retry_interval = duration(100);
    })?;
    let first = p.h().worker.clone();
    let second = p.base.worker(Transport::Local, &["a", "b"]);
    let all = p.base.connections.clone();
    let old = first.clone();
    let new = second.clone();
    p.answer(move |_, w, n| {
        let all = all.clone();
        let old = old.clone();
        let new = new.clone();
        Box::pin(async move {
            if w && n == 1 {
                return Err(unreachable());
            }
            if !w && n == 2 {
                old.connected.store(false, Ordering::SeqCst);
                *all.workers.lock().unwrap() = vec![(Transport::Local, new)];
                return Err(unreachable());
            }
            Ok(None)
        })
    });
    p.update(&["a", "b"]).await?;
    let first_device = p.connected(1).await.remove(0);
    assert!(Arc::ptr_eq(
        &first_device.executor(),
        &(first as marathon_apple::device::connection_factory::Executor)
    ));
    let second_device = p.connected(1).await.remove(0);
    assert!(!Arc::ptr_eq(&first_device, &second_device));
    assert_eq!(p.connections.attempts(&Transport::Local, true), 3);
    assert_eq!(home(&second, second_device.udid()), 1);
    assert_eq!(home(&second, first_device.udid()), 0);
    p.balanced().await
}
