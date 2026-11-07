//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt:215-218,267-272,279-369,445-450
//! D44: Kotlin tries a lost worker every 16 ms until the run ends (A101), a failed re-provisioning is otherwise final, and
//! borrow can hand out a removed, disposed simulator (A80). Kotlin has no such limits, so these have no Kotlin test.
mod provider_support;
mod recovery_support;
mod simulator_support;
mod support;
use marathon_apple::{
    apple_device::AppleDevice,
    cmd::remote::ssh::sshj::SshFailure,
    device::connection_factory::ConnectionError,
    logparser::parser::{DeviceFailureException, DeviceFailureReason},
};
use marathon_config::devices::Transport;
use marathon_core::vendor::{Device, DeviceEvent, DeviceProvider};
use marathon_ios::device::{
    simulator_factory::SimulatorCreating, simulator_provisioner::ProvisioningConnections,
    worker_provisioning_context::WorkerProvisioningContext,
};
use marathon_test_support::apple::FakeSimulatorWorker;
use provider_support::*;
use recovery_support::*;
use simulator_support::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use support::{duration, watchdog};

/// Every event the provider raised, since `connected` would skip the disconnects.
#[derive(Default)]
struct Seen {
    connected: Vec<String>,
    disconnected: usize,
}
impl Seen {
    async fn drain(&mut self, p: &RecoveryHarness) {
        settle().await;
        while p.provider.subscribe().state().queued > 0 {
            match p.provider.subscribe().receive().await.unwrap() {
                Some(DeviceEvent::Connected(device)) => {
                    self.connected.push(device.0.info().serial_number.to_string_lossy())
                }
                Some(DeviceEvent::Disconnected(_)) => self.disconnected += 1,
                None => break,
            }
        }
    }
    async fn until_connected(&mut self, p: &RecoveryHarness, count: usize) {
        // Longer than the worker retry's longest pause.
        support::bounded(120, async {
            loop {
                self.drain(p).await;
                if self.connected.len() >= count {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await
    }
}
/// The clock runs from timer to timer, so that pauses end when they are due.
async fn pass(seconds: u64) {
    tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
    settle().await;
}
struct Lost {
    down: Arc<AtomicBool>,
    fresh: Arc<FakeSimulatorWorker>,
    at: i64,
    /// The calls before the loss. A count, as a first setup's run at the same virtual time as a loss right after it.
    before: usize,
}
/// The worker's connection dies. New connections fail until `down` is cleared, and reach `fresh` after that.
fn lose_worker(p: &RecoveryHarness, ids: &[&str]) -> Lost {
    let down = Arc::new(AtomicBool::new(true));
    let fresh = p.base.worker(Transport::Local, ids);
    *p.base.connections.workers.lock().unwrap() = vec![(Transport::Local, fresh.clone())];
    let flag = down.clone();
    p.answer(move |_, _, _| {
        let down = flag.load(Ordering::SeqCst);
        Box::pin(async move { if down { Err(unreachable()) } else { Ok(None) } })
    });
    let at = p.h().clock.now();
    let before = p.connections.calls.lock().unwrap().len();
    p.h().worker.connected.store(false, Ordering::SeqCst);
    Lost {
        down,
        fresh,
        at,
        before,
    }
}
fn attempts_since(p: &RecoveryHarness, lost: &Lost, worker: bool) -> Vec<i64> {
    p.connections
        .calls
        .lock()
        .unwrap()
        .iter()
        .skip(lost.before)
        .filter(|(_, w, _)| *w == worker)
        .map(|(_, _, time)| time - lost.at)
        .collect()
}
fn assert_seconds(actual: &[i64], seconds: &[i64]) {
    assert_eq!(actual.len(), seconds.len(), "{actual:?}");
    for (actual, seconds) in actual.iter().zip(seconds) {
        assert!(
            (seconds * 1000..seconds * 1000 + 100).contains(actual),
            "{actual} for {seconds} s"
        );
    }
}
/// `getenv HOME` fails on the first `failures` setups.
fn fail_home(worker: &FakeSimulatorWorker, h: &Harness, failures: usize) {
    fail_home_of(worker, h, None, failures)
}
/// As `fail_home`, for one simulator only when `udid` is given.
fn fail_home_of(worker: &FakeSimulatorWorker, h: &Harness, udid: Option<&'static str>, failures: usize) {
    let boot = h.boot.clone();
    let left = Arc::new(AtomicUsize::new(failures));
    *worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let answer = boot.answer(&cmd);
        let fail = cmd.last().is_some_and(|s| s == "HOME")
            && udid.is_none_or(|udid| cmd.len() >= 2 && cmd[cmd.len() - 2] == udid)
            && left
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
        Box::pin(async move {
            if fail {
                Err(support::error("setup failed"))
            } else {
                Ok(answer)
            }
        })
    });
}
const CAPPED: [i64; 11] = [10, 30, 90, 150, 210, 270, 330, 390, 450, 510, 570];

#[tokio::test(start_paused = true)]
async fn a_worker_that_stays_down_is_tried_at_capped_pauses_until_the_limit_and_then_left_alone() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let lost = lose_worker(&p, &["a", "b"]);
    // The limit is looked at when a pause ends, so the end comes with the pause after the attempt at 570 s.
    pass(625).await;
    assert!(!p.log("has no connection"));
    pass(10).await;
    assert!(p.log("has no connection"));
    pass(65).await;
    // One connection each for the two simulators, then one per worker however many simulators wait on it.
    assert_seconds(&attempts_since(&p, &lost, false), &[0, 0]);
    assert_seconds(&attempts_since(&p, &lost, true), &CAPPED);
    assert!(p.log("Simulator a@local has no connection 600 s after it lost its worker, it stays out of the run"));
    assert!(p.log("Simulator b@local has no connection 600 s after it lost its worker, it stays out of the run"));
    pass(700).await;
    assert_eq!(attempts_since(&p, &lost, true).len(), CAPPED.len());
    let mut seen = Seen::default();
    seen.drain(&p).await;
    assert_eq!(seen.disconnected, 2);
    assert!(seen.connected.is_empty());
    p.finish().await
}
/// The most a worker loss acquires with the defaults: the first attempt's one connection per simulator, then for each of
/// the worker retry's 11 attempts one planning connection and, when planning gets through, one per waiting simulator.
#[tokio::test(start_paused = true)]
async fn a_worker_that_answers_while_its_simulators_cannot_connect_acquires_at_most_n_plus_11_times_n_plus_1()
-> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let lost = lose_worker(&p, &["a", "b"]);
    p.answer(|_, worker, _| Box::pin(async move { if worker { Ok(None) } else { Err(unreachable()) } }));
    pass(1300).await;
    assert_seconds(&attempts_since(&p, &lost, true), &CAPPED);
    let simulators = attempts_since(&p, &lost, false);
    let expected: Vec<i64> = [0].into_iter().chain(CAPPED).flat_map(|s| [s, s]).collect();
    assert_seconds(&simulators, &expected);
    // N + 11 × (N + 1) for N = 2.
    assert_eq!(simulators.len() + CAPPED.len(), 35);
    assert!(p.log("Simulator a@local has no connection 600 s after it lost its worker, it stays out of the run"));
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn a_worker_that_comes_back_recovers_its_simulators_exactly_once() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a", "b"]).await?;
    let old = p.connected(2).await;
    let lost = lose_worker(&p, &["a", "b"]);
    pass(100).await;
    lost.down.store(false, Ordering::SeqCst);
    let mut seen = Seen::default();
    seen.until_connected(&p, 2).await;
    // Back at the attempt due 150 s after the loss, and nothing after it.
    assert_seconds(&attempts_since(&p, &lost, true), &[10, 30, 90, 150]);
    eventually(|| home(&lost.fresh, "a") == 1 && home(&lost.fresh, "b") == 1).await;
    pass(300).await;
    seen.drain(&p).await;
    assert_eq!(seen.connected.len(), 2);
    assert_eq!(seen.disconnected, 2);
    assert_eq!((home(&lost.fresh, "a"), home(&lost.fresh, "b")), (1, 1));
    assert_eq!(attempts_since(&p, &lost, true).len(), 4);
    for device in &old {
        assert!(!Arc::ptr_eq(device, &p.current(device.udid())));
        assert!(p.current(device.udid()).connected());
    }
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn a_worker_that_comes_back_long_after_the_late_join_cut_off_gets_its_simulators_back_but_no_new_ones()
-> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a"]).await?;
    p.connected(1).await;
    pass(700).await;
    let lost = lose_worker(&p, &["a", "c"]);
    pass(5).await;
    p.update(&["a", "c"]).await?;
    assert!(p.log("its 1 new devices will be set up once it connects"));
    pass(10).await;
    assert!(p.log("Worker local: 1 devices don't join the run late"));
    assert!(!p.log("No longer connecting to it"));
    lost.down.store(false, Ordering::SeqCst);
    let mut seen = Seen::default();
    seen.until_connected(&p, 1).await;
    assert_eq!(p.current("a").udid(), "a");
    assert!(p.current("a").connected());
    pass(120).await;
    seen.drain(&p).await;
    assert_eq!(seen.connected.len(), 1);
    assert_eq!(home(&lost.fresh, "c"), 0);
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn the_limit_can_be_switched_off_or_tightened_with_the_worker_connection_settings() -> TestResult {
    // No cut-off: the run's end is the limit, at the capped pace.
    let p = RecoveryHarness::new(|v| v.worker_connection.late_join_cutoff = duration(0))?;
    p.start(&["a"]).await?;
    p.connected(1).await;
    let lost = lose_worker(&p, &["a"]);
    pass(1000).await;
    assert_eq!(attempts_since(&p, &lost, true).len(), CAPPED.len() + 7);
    p.finish().await?;
    // No late join: the configured attempts.
    let p = RecoveryHarness::new(|v| v.worker_connection.late_join = false)?;
    p.start(&["a"]).await?;
    p.connected(1).await;
    let lost = lose_worker(&p, &["a"]);
    pass(300).await;
    assert_seconds(&attempts_since(&p, &lost, true), &[10, 30]);
    assert!(p.log("after 3 attempts"));
    p.finish().await?;
    // The way to ask for no retries of a worker's devices: the first reconnect is all there is.
    let p = RecoveryHarness::new(|v| {
        v.worker_connection.late_join = false;
        v.worker_connection.attempts = 1;
    })?;
    p.start(&["a"]).await?;
    p.connected(1).await;
    let lost = lose_worker(&p, &["a"]);
    pass(300).await;
    assert_seconds(&attempts_since(&p, &lost, false), &[0]);
    assert!(attempts_since(&p, &lost, true).is_empty());
    assert!(p.log("1 devices could not connect, they stay out of the run"));
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn a_refused_connection_is_not_tried_again() -> TestResult {
    let kex = || {
        ConnectionError::Device(DeviceFailureException::new(
            DeviceFailureReason::UnreachableHost,
            None,
            Some(SshFailure::no_common_algorithm("Unable to reach a settlement")),
        ))
    };
    for error in [auth(), kex()] {
        let p = RecoveryHarness::new(|_| {})?;
        p.start(&["a"]).await?;
        p.connected(1).await;
        let lost = lose_worker(&p, &["a"]);
        p.answer(move |_, _, _| {
            let error = error.clone();
            Box::pin(async move { Err(error) })
        });
        pass(1000).await;
        assert_seconds(&attempts_since(&p, &lost, false), &[0]);
        assert!(attempts_since(&p, &lost, true).is_empty());
        assert!(p.log("Simulator a on worker local: its connection was refused, it stays out of the run"));
        let mut seen = Seen::default();
        seen.drain(&p).await;
        assert_eq!(seen.disconnected, 1);
        p.finish().await?;
    }
    Ok(())
}
/// A refused simulator is out for good: a sibling's setup retries plan its worker again, and must not take it along.
#[tokio::test(start_paused = true)]
async fn a_simulator_refused_after_a_worker_loss_is_not_set_up_again_by_a_siblings_setup_retries() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let lost = lose_worker(&p, &["a", "b"]);
    fail_home_of(&lost.fresh, p.h(), Some("b"), usize::MAX);
    let first = Arc::new(AtomicBool::new(true));
    // The recovery connects to a first, then to b. Only that first connection is refused, so a simulator taken along
    // by a later plan would connect, and be counted below.
    p.answer(move |_, worker, _| {
        let refuse = !worker && first.swap(false, Ordering::SeqCst);
        Box::pin(async move { if refuse { Err(auth()) } else { Ok(None) } })
    });
    for (retry, pause) in [(1, 30), (2, 60), (3, 90)] {
        eventually(|| p.log(&format!("(retry {retry} of 3)"))).await;
        pass(pause).await;
    }
    eventually(|| p.log("on worker local: setup failed 4 times")).await;
    pass(1000).await;
    assert!(p.log("Simulator a on worker local: its connection was refused, it stays out of the run"));
    // a once, refused; b at the recovery and at each of its three retries.
    assert_seconds(&attempts_since(&p, &lost, false), &[0, 0, 30, 90, 180]);
    assert_eq!((home(&lost.fresh, "a"), home(&lost.fresh, "b")), (0, 4));
    let mut seen = Seen::default();
    seen.drain(&p).await;
    assert!(seen.connected.is_empty(), "{:?}", seen.connected);
    assert_eq!(seen.disconnected, 2);
    p.finish().await
}
/// The way back for a refused simulator: the devices file drops it and adds it back.
#[tokio::test(start_paused = true)]
async fn a_refused_simulator_that_is_removed_and_added_back_is_set_up_again() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a"]).await?;
    let old = p.connected(1).await.remove(0);
    let lost = lose_worker(&p, &["a"]);
    p.answer(|_, worker, _| Box::pin(async move { if worker { Ok(None) } else { Err(auth()) } }));
    eventually(|| p.log("its connection was refused, it stays out of the run")).await;
    p.answer(|_, _, _| Box::pin(async { Ok(None) }));
    p.update(&[]).await?;
    p.update(&["a"]).await?;
    let mut seen = Seen::default();
    seen.until_connected(&p, 1).await;
    assert!(!Arc::ptr_eq(&old, &p.current("a")));
    assert!(p.current("a").connected());
    assert_eq!(home(&lost.fresh, "a"), 1);
    p.finish().await
}
/// a and b wait for the same worker retry, so its plan names both. a is refused, and b's setup then loses the planning
/// connection, which sends what failed back to the retry: but not a, whose own connection said no.
#[tokio::test(start_paused = true)]
async fn a_refusal_stays_final_when_the_same_plan_loses_its_planning_connection() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let lost = lose_worker(&p, &["a", "b"]);
    let (boot, worker, dropped) = (p.h().boot.clone(), lost.fresh.clone(), Arc::new(AtomicBool::new(false)));
    *lost.fresh.before.lock().unwrap() = Arc::new(move |cmd| {
        let answer = boot.answer(&cmd);
        let b_home = cmd.last().is_some_and(|s| s == "HOME") && cmd.len() >= 2 && cmd[cmd.len() - 2] == "b";
        let drop = b_home && !dropped.swap(true, Ordering::SeqCst);
        if drop {
            worker.connected.store(false, Ordering::SeqCst);
        }
        Box::pin(async move {
            if drop {
                Err(support::error("connection lost"))
            } else {
                Ok(answer)
            }
        })
    });
    let (worker, simulators) = (lost.fresh.clone(), Arc::new(AtomicUsize::new(0)));
    // The first attempt reaches neither; at 10 s a, which comes first, is refused. Everything after that connects.
    p.answer(move |_, planning, _| {
        if planning {
            worker.connected.store(true, Ordering::SeqCst);
        }
        let k = if planning {
            0
        } else {
            simulators.fetch_add(1, Ordering::SeqCst) + 1
        };
        Box::pin(async move {
            match k {
                1 | 2 => Err(unreachable()),
                3 => Err(auth()),
                _ => Ok(None),
            }
        })
    });
    let mut seen = Seen::default();
    seen.until_connected(&p, 1).await;
    pass(300).await;
    seen.drain(&p).await;
    assert_eq!(seen.connected, ["b@local"]);
    assert_eq!((home(&lost.fresh, "a"), home(&lost.fresh, "b")), (0, 2));
    // Two at once, two at 10 s, and b alone at 30 s.
    assert_seconds(&attempts_since(&p, &lost, false), &[0, 0, 10, 10, 30]);
    p.finish().await
}
/// A plan that was setting up a parked simulator when the devices file dropped it and added it back: its result is about
/// the old claim, and the update's own plan decides.
#[tokio::test(start_paused = true)]
async fn a_refusal_seen_by_a_plan_that_outlived_a_remove_and_re_add_does_not_stop_the_new_one() -> TestResult {
    use marathon_core::{executor::shared::SharedDeferred, runtime::Deferred};
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a"]).await?;
    p.connected(1).await;
    let lost = lose_worker(&p, &["a"]);
    let gate = SharedDeferred::new();
    let (release, simulators) = (gate.clone(), Arc::new(AtomicUsize::new(0)));
    // The first attempt finds the worker gone; the worker retry's plan at 10 s waits on its connection to a, and is
    // then refused. Later ones work.
    p.answer(move |_, worker, _| {
        let k = if worker {
            0
        } else {
            simulators.fetch_add(1, Ordering::SeqCst) + 1
        };
        let release = release.clone();
        Box::pin(async move {
            match k {
                1 => Err(unreachable()),
                2 => {
                    release.wait().await.map_err(ConnectionError::from)?;
                    Err(auth())
                }
                _ => Ok(None),
            }
        })
    });
    pass(11).await;
    p.update(&[]).await?;
    let (added, ()) = tokio::join!(p.update(&["a"]), async {
        pass(1).await;
        gate.complete(Ok(()));
    });
    added?;
    let mut seen = Seen::default();
    seen.until_connected(&p, 1).await;
    assert!(p.current("a").connected());
    assert_eq!(home(&lost.fresh, "a"), 1);
    p.finish().await
}
/// A simulator its retry gave up on stays out when a devices update plans its worker for another one: D33's budget
/// spent, and the worker retry at its limit.
#[tokio::test(start_paused = true)]
async fn a_simulator_given_up_on_is_not_set_up_again_by_a_later_plan_on_its_worker() -> TestResult {
    // The worker answers and a's setup keeps failing.
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a"]).await?;
    p.connected(1).await;
    let lost = lose_worker(&p, &["a", "c"]);
    fail_home_of(&lost.fresh, p.h(), Some("a"), usize::MAX);
    lost.down.store(false, Ordering::SeqCst);
    for (retry, pause) in [(1, 30), (2, 60), (3, 90)] {
        eventually(|| p.log(&format!("(retry {retry} of 3)"))).await;
        pass(pause).await;
    }
    eventually(|| p.log("on worker local: setup failed 4 times, it stays out of the run")).await;
    pass(10).await;
    p.update(&["a", "c"]).await?;
    let mut seen = Seen::default();
    seen.until_connected(&p, 1).await;
    pass(300).await;
    seen.drain(&p).await;
    assert_eq!(seen.connected, ["c@local"]);
    assert_eq!((home(&lost.fresh, "a"), home(&lost.fresh, "c")), (4, 1));
    p.finish().await?;
    // The worker stays down past the limit, then comes back.
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a"]).await?;
    p.connected(1).await;
    let lost = lose_worker(&p, &["a", "c"]);
    pass(700).await;
    assert!(p.log("Simulator a@local has no connection 600 s after it lost its worker"));
    lost.down.store(false, Ordering::SeqCst);
    p.update(&["a", "c"]).await?;
    let mut seen = Seen::default();
    seen.until_connected(&p, 1).await;
    pass(300).await;
    seen.drain(&p).await;
    assert_eq!(seen.connected, ["c@local"]);
    assert_eq!((home(&lost.fresh, "a"), home(&lost.fresh, "c")), (0, 1));
    p.finish().await
}
/// D36: the login works and the next command finds the connection gone, which fail2ban or sshd's penalties can see.
#[tokio::test(start_paused = true)]
async fn a_connection_dropped_after_each_login_is_tried_at_the_capped_pace() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a"]).await?;
    p.connected(1).await;
    let lost = lose_worker(&p, &["a"]);
    lost.down.store(false, Ordering::SeqCst);
    lost.fresh.connected.store(false, Ordering::SeqCst);
    *lost.fresh.before.lock().unwrap() = Arc::new(|_| {
        Box::pin(async {
            Err(marathon_core::error::VendorError::Other(
                SshFailure::not_connected().into_vendor().failure().clone(),
            ))
        })
    });
    pass(100).await;
    assert_seconds(&attempts_since(&p, &lost, false), &[0]);
    assert_seconds(&attempts_since(&p, &lost, true), &[10, 30, 90]);
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn a_re_provisioning_that_fails_once_and_then_succeeds_gives_exactly_one_actor_after_a_worker_loss() -> TestResult
{
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a"]).await?;
    let old = p.connected(1).await.remove(0);
    let lost = lose_worker(&p, &["a"]);
    fail_home(&lost.fresh, p.h(), 1);
    lost.down.store(false, Ordering::SeqCst);
    eventually(|| p.log("(retry 1 of 3)")).await;
    let mut seen = Seen::default();
    seen.until_connected(&p, 1).await;
    pass(300).await;
    seen.drain(&p).await;
    assert_eq!(seen.connected.len(), 1);
    assert_eq!(seen.disconnected, 1);
    assert_eq!(home(&lost.fresh, "a"), 2);
    assert!(!Arc::ptr_eq(&old, &p.current("a")));
    assert_eq!(p.provider.devices().len(), 1);
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn a_re_provisioning_that_fails_once_and_then_succeeds_gives_exactly_one_actor_with_the_connection_up()
-> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a"]).await?;
    let old = p.connected(1).await.remove(0);
    fail_home(&p.h().worker, p.h(), 1);
    p.lose(&old).await;
    eventually(|| p.log("(retry 1 of 3)")).await;
    let mut seen = Seen::default();
    seen.until_connected(&p, 1).await;
    pass(300).await;
    seen.drain(&p).await;
    assert_eq!(seen.connected.len(), 1);
    assert_eq!(seen.disconnected, 1);
    // The first setup, the failure and the one that worked.
    assert_eq!(home(&p.h().worker, "a"), 3);
    assert!(!Arc::ptr_eq(&old, &p.current("a")));
    // A90: the predecessor of a connected loss keeps its lease for termination, also when a retry replaces it.
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_worker_that_answers_and_keeps_failing_setup_uses_the_setup_retry_budget_and_stays_out() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a"]).await?;
    p.connected(1).await;
    let lost = lose_worker(&p, &["a"]);
    fail_home(&lost.fresh, p.h(), usize::MAX);
    lost.down.store(false, Ordering::SeqCst);
    for (retry, pause) in [(1, 30), (2, 60), (3, 90)] {
        eventually(|| p.log(&format!("(retry {retry} of 3)"))).await;
        pass(pause).await;
    }
    eventually(|| p.log("setup failed 4 times, it stays out of the run")).await;
    pass(1000).await;
    assert_eq!(home(&lost.fresh, "a"), 4);
    let mut seen = Seen::default();
    seen.drain(&p).await;
    assert!(seen.connected.is_empty());
    assert_eq!(seen.disconnected, 1);
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn removing_a_waiting_simulator_ends_its_retry_and_adding_it_back_starts_over() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a"]).await?;
    p.connected(1).await;
    let lost = lose_worker(&p, &["a"]);
    pass(500).await;
    p.update(&[]).await?;
    pass(100).await;
    let tried = attempts_since(&p, &lost, true).len();
    pass(100).await;
    assert_eq!(attempts_since(&p, &lost, true).len(), tried);
    // 700 s after the loss, so only a limit that started over lets this one come back.
    p.update(&["a"]).await?;
    pass(30).await;
    assert!(attempts_since(&p, &lost, true).len() > tried);
    assert!(!p.log("has no connection"));
    lost.down.store(false, Ordering::SeqCst);
    let mut seen = Seen::default();
    seen.until_connected(&p, 1).await;
    assert!(p.current("a").connected());
    assert_eq!(home(&lost.fresh, "a"), 1);
    pass(120).await;
    seen.drain(&p).await;
    assert_eq!(seen.connected.len(), 1);
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn terminating_cancels_a_waiting_reconnect_without_waiting_for_its_pause() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a"]).await?;
    p.connected(1).await;
    let lost = lose_worker(&p, &["a"]);
    pass(40).await;
    let tried = attempts_since(&p, &lost, true).len();
    let at = p.h().clock.now();
    p.finish().await?;
    assert!(p.h().clock.now() - at < 1000);
    pass(600).await;
    assert_eq!(attempts_since(&p, &lost, true).len(), tried);
    Ok(())
}
/// A replacement registers after the scan took its snapshot and before it looks at the old handle. Real threads: the
/// window has no await in it, so nothing else can reach it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_leaves_alone_a_handle_replaced_since_its_snapshot() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.update(&["a"]).await?;
    let old = p.connected(1).await.remove(0);
    p.connections
        .connect(Transport::Local, p.h().services.context.clone())
        .await
        .map_err(test_error)?;
    let fresh = p.base.worker(Transport::Local, &["a"]);
    let replacement = p
        .h()
        .factory()
        .create(
            Transport::Local,
            fresh.clone(),
            Arc::new(support::BridgeMock),
            "a".into(),
            Arc::new(WorkerProvisioningContext::new(Transport::Local, p.h().clock.clone())),
            p.h().services.context.clone(),
        )
        .await
        .map_err(test_error)?;
    let (homes, connects) = (home(&fresh, "a"), p.connections.attempts(&Transport::Local, false));
    let (checking, in_check) = std::sync::mpsc::channel();
    let (replaced, was_replaced) = std::sync::mpsc::channel::<()>();
    let was_replaced = Mutex::new(was_replaced);
    let once = AtomicBool::new(true);
    // Only the scan's second look at the old handle, once it has passed the first check.
    *p.h().worker.on_connected_check.lock().unwrap() = Arc::new(move || {
        if once.swap(false, Ordering::SeqCst) {
            checking.send(()).unwrap();
            was_replaced
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(60))
                .unwrap();
        }
    });
    p.h().worker.connected.store(false, Ordering::SeqCst);
    let (provider, recovery, context) = (p.provider.clone(), p.recovery.clone(), p.h().services.context.clone());
    use marathon_ios::device::simulator_provider::ProviderRecovery;
    let scan = tokio::spawn(async move { recovery.reconnect(provider, context).await });
    let in_check = tokio::task::spawn_blocking(move || in_check.recv_timeout(std::time::Duration::from_secs(60)));
    in_check.await?.unwrap();
    p.provider
        .register("a@local".into(), replacement)
        .await
        .map_err(test_error)?;
    replaced.send(()).unwrap();
    scan.await?.map_err(test_error)?;
    p.connected(1).await;
    assert!(!Arc::ptr_eq(&old, &p.current("a")));
    // A stale scan would have raised a disconnect for the old handle and started a recovery that sets it up again.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(p.provider.subscribe().state().queued, 0);
    assert_eq!(home(&fresh, "a"), homes);
    assert_eq!(p.connections.attempts(&Transport::Local, false), connects);
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn borrow_takes_a_simulator_that_can_work_and_not_one_that_left_the_devices_file() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.update(&["a", "b"]).await?;
    p.connected(2).await;
    p.update(&["b"]).await?;
    assert!(p.current("a").disposed());
    // The harness picks the first key, which was the removed one.
    let borrowed = watchdog(p.provider.borrow(p.h().services.context.clone()))
        .await
        .map_err(test_error)?;
    assert!(Arc::ptr_eq(&borrowed.0, &(p.current("b") as Arc<dyn Device>)));
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn borrow_waits_while_a_simulator_waits_for_its_worker_and_takes_its_replacement() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a"]).await?;
    p.connected(1).await;
    let lost = lose_worker(&p, &["a"]);
    pass(5).await;
    let provider = p.provider.clone();
    let caller = p.h().services.context.clone();
    let borrowed = tokio::spawn(async move { provider.borrow(caller).await });
    pass(20).await;
    assert!(!borrowed.is_finished());
    lost.down.store(false, Ordering::SeqCst);
    let mut seen = Seen::default();
    seen.until_connected(&p, 1).await;
    let device = watchdog(borrowed).await?.map_err(test_error)?;
    assert!(Arc::ptr_eq(&device.0, &(p.current("a") as Arc<dyn Device>)));
    p.finish().await
}

/// D33's four setups per simulator hold when two retries run on one worker: neither plan takes the other along.
#[tokio::test(start_paused = true)]
async fn two_simulators_lost_with_their_connection_up_get_four_setups_each() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a", "b"]).await?;
    let old = p.connected(2).await;
    fail_home(&p.h().worker, p.h(), usize::MAX);
    p.lose(old.iter().find(|d| d.udid() == "a").unwrap()).await;
    eventually(|| p.log("setup of a failed")).await;
    pass(5).await;
    p.lose(old.iter().find(|d| d.udid() == "b").unwrap()).await;
    eventually(|| p.log("setup of b failed")).await;
    pass(300).await;
    let a = home(&p.h().worker, "a") - 1;
    let b = home(&p.h().worker, "b") - 1;
    p.finish().await?;
    assert!(a <= 4 && b <= 4, "setup attempts after loss: a={a}, b={b}");
    Ok(())
}

/// a is refused in the worker retry's plan while b, on D33, drops the planning connection or not: a is asked once.
#[tokio::test(start_paused = true)]
async fn a_refusal_in_a_later_plan_is_final_even_when_the_planning_connection_drops() -> TestResult {
    for drop_planning in [false, true] {
        let p = RecoveryHarness::new(|_| {})?;
        p.start(&["a", "b"]).await?;
        p.connected(2).await;
        let lost = lose_worker(&p, &["a", "b"]);
        let boot = p.h().boot.clone();
        let worker = lost.fresh.clone();
        let homes = AtomicUsize::new(0);
        *lost.fresh.before.lock().unwrap() = Arc::new(move |cmd| {
            let answer = boot.answer(&cmd);
            let b_home = cmd.last().is_some_and(|s| s == "HOME") && cmd.len() >= 2 && cmd[cmd.len() - 2] == "b";
            if b_home && homes.fetch_add(1, Ordering::SeqCst) == 1 && drop_planning {
                worker.connected.store(false, Ordering::SeqCst);
            }
            Box::pin(async move {
                if b_home {
                    Err(support::error("setup failed"))
                } else {
                    Ok(answer)
                }
            })
        });
        let refused = Arc::new(AtomicUsize::new(0));
        let count = refused.clone();
        let worker = lost.fresh.clone();
        p.answer(move |_, planning, n| {
            if planning {
                worker.connected.store(true, Ordering::SeqCst);
            }
            let failure = if !planning && n == 3 {
                Some(unreachable())
            } else if !planning && (n == 5 || n >= 7) {
                count.fetch_add(1, Ordering::SeqCst);
                Some(auth())
            } else {
                None
            };
            Box::pin(async move { failure.map_or(Ok(None), Err) })
        });
        pass(25).await;
        assert_eq!(refused.load(Ordering::SeqCst), 1);
        pass(10).await;
        let a_failures = p
            .h()
            .logs
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, _, message, _)| message.contains("to set up simulator a"))
            .count();
        p.finish().await?;
        assert_eq!(a_failures, 2, "drop_planning={drop_planning}: a refused twice");
    }
    Ok(())
}

/// The way back from each way a retry gives up: its limit, `attempts` with `lateJoin: false`, no retry, D33's budget.
#[tokio::test(start_paused = true)]
async fn every_given_up_simulator_comes_back_when_removed_and_added_back() -> TestResult {
    for mode in 0..4 {
        let p = RecoveryHarness::new(|v| {
            if mode == 1 {
                v.worker_connection.late_join = false;
            }
            if mode == 2 {
                v.worker_connection.late_join = false;
                v.worker_connection.attempts = 1;
            }
        })?;
        p.start(&["a"]).await?;
        let old = p.connected(1).await.remove(0);
        let lost = lose_worker(&p, &["a"]);
        if mode == 3 {
            fail_home(&lost.fresh, p.h(), usize::MAX);
            lost.down.store(false, Ordering::SeqCst);
        }
        pass(700).await;
        lost.down.store(false, Ordering::SeqCst);
        let boot = p.h().boot.clone();
        *lost.fresh.before.lock().unwrap() = Arc::new(move |cmd| {
            let answer = boot.answer(&cmd);
            Box::pin(async move { Ok(answer) })
        });
        p.update(&[]).await?;
        p.update(&["a"]).await?;
        let mut seen = Seen::default();
        seen.until_connected(&p, 1).await;
        pass(300).await;
        seen.drain(&p).await;
        assert_eq!(seen.connected.len(), 1, "mode={mode}");
        assert!(!Arc::ptr_eq(&old, &p.current("a")));
        assert!(p.current("a").connected());
        p.finish().await?;
    }
    Ok(())
}

/// A D33 retry waiting for its connection when the simulator is removed and added back: its refusal is about the old
/// claim, and the new one is the worker retry's.
#[tokio::test(start_paused = true)]
async fn a_setup_retry_from_before_a_removal_cannot_give_up_the_simulator_added_back() -> TestResult {
    use marathon_core::{executor::shared::SharedDeferred, runtime::Deferred};
    let p = RecoveryHarness::new(|_| {})?;
    p.start(&["a"]).await?;
    let old = p.connected(1).await.remove(0);
    fail_home(&p.h().worker, p.h(), 1);
    let gate = SharedDeferred::new();
    let waiting = Arc::new(AtomicBool::new(false));
    let flag = waiting.clone();
    let release = gate.clone();
    p.answer(move |_, planning, n| {
        let release = release.clone();
        let flag = flag.clone();
        Box::pin(async move {
            if planning && n == 2 {
                flag.store(true, Ordering::SeqCst);
                release.wait().await.map_err(ConnectionError::from)?;
                return Err(auth());
            }
            if !planning && n == 3 {
                return Err(unreachable());
            }
            Ok(None)
        })
    });
    p.lose(&old).await;
    eventually(|| p.log("setup of a failed")).await;
    pass(31).await;
    assert!(waiting.load(Ordering::SeqCst));
    p.update(&[]).await?;
    p.update(&["a"]).await?;
    assert!(p.log("1 devices could not connect, retrying"));
    // This error belongs to the retry from before removal, not the new generation's transient failure.
    gate.complete(Ok(()));
    pass(200).await;
    let mut seen = Seen::default();
    seen.drain(&p).await;
    p.finish().await?;
    assert_eq!(seen.connected, ["a@local"]);
    Ok(())
}
