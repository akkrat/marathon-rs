//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt
//! D87 in virtual time: what a simulator's preparation deadline lets start, on one simulator. A replacement is made the
//! way a recovery makes it: a second simulator opens an episode, time passes, and the simulator under test takes it over.
mod provider_support;
mod recovery_support;
mod simulator_support;
mod support;
use marathon_apple::{
    apple_device::AppleDevice,
    cmd::{CommandResult, FileBridge},
};
use marathon_config::devices::Transport;
use marathon_core::{
    error::{LeavePoolReason, VendorError},
    executor::shared::SharedDeferred,
    runtime::VendorFuture,
    vendor::Device,
};
use marathon_ios::{
    apple_simulator_device::AppleSimulatorDevice, device::worker_provisioning_context::WorkerProvisioningContext,
};
use simulator_support::*;
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::duration;

const SECOND: i64 = 1_000;
/// The commands of `a` the worker received, with their virtual time.
type Seen = Arc<Mutex<Vec<(i64, Vec<String>)>>>;
/// A disposal hold: armed, entered, and the gate it waits for.
type Hold = (Arc<AtomicBool>, Arc<SharedDeferred<()>>, Arc<SharedDeferred<()>>);

/// A harness with `bootWait` `boot_wait` s, `shell` `shell` s (`boot` and `shutdown` 30 s), and a 1 s poll.
fn harness(boot_wait: i64, shell: i64) -> TestResult<Harness> {
    let mut h = Harness::new(|v| {
        let t = &mut v.timeout_configuration;
        t.boot_wait = duration(boot_wait * SECOND);
        t.shell = duration(shell * SECOND);
        t.boot = duration(30 * SECOND);
        t.shutdown = duration(30 * SECOND);
    })?;
    h.services.timeouts.boot_poll_interval_millis = SECOND;
    Ok(h)
}

/// The deadline is the envelope: the wait for a device is set to nothing.
fn configuration(h: &Harness) -> marathon_config::Configuration {
    let mut c = (*h.services.configuration).clone();
    c.device_initialization_timeout_millis = 0;
    c
}

/// Simulator `a` as the replacement of a simulator whose preparation, `left` ms from its deadline, failed to boot.
async fn replacement(
    h: &Harness,
    bridge: Option<Arc<dyn FileBridge>>,
    left: i64,
) -> TestResult<Arc<AppleSimulatorDevice>> {
    let cycle = || Arc::new(WorkerProvisioningContext::new(Transport::Local, h.clock.clone()));
    let predecessor = h.device_on(h.worker.clone(), "b", cycle()).await?;
    let budget = predecessor
        .preparation_time_left_millis(&configuration(h))
        .ok_or("an episode")?;
    tokio::time::advance(Duration::from_millis((budget - left) as u64)).await;
    let episode = predecessor.preparation_episode().ok_or("an episode")?;
    let d = match bridge {
        Some(bridge) => h.device_on_bridge(h.worker.clone(), "a", cycle(), bridge).await?,
        None => h.device_on(h.worker.clone(), "a", cycle()).await?,
    };
    d.continue_preparation(episode);
    Ok(d)
}

fn not_booted(error: &VendorError) -> bool {
    matches!(
        error,
        VendorError::LeavePool {
            reason: LeavePoolReason::NotBooted,
            ..
        }
    )
}

/// The commands of `a` the worker received, with their virtual time.
fn on_commands(
    h: &Harness,
    answer: impl Fn(&Boot, &[String], i64) -> Option<VendorFuture<'static, Option<CommandResult>>> + Send + Sync + 'static,
) -> Seen {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    let plan = h.boot.clone();
    let clock = h.clock.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let now = clock.now();
        record.lock().unwrap().push((now, cmd.clone()));
        if let Some(future) = answer(&plan, &cmd, now) {
            return future;
        }
        let reply = plan.answer(&cmd);
        Box::pin(async move { Ok(reply) })
    });
    seen
}

fn at(seen: &Mutex<Vec<(i64, Vec<String>)>>, verb: &str) -> Vec<i64> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|(_, c)| c.len() > 3 && c[0] == "xcrun" && c[2] == verb && c[3] == "a")
        .map(|(at, _)| *at)
        .collect()
}

/// D87 (review item 2): a replacement that started before the deadline boots once with the whole of `bootWait`, even
/// with less left: here 10 s are left and the simulator needs 20 s to get ready, within the 60 s `bootWait`.
#[tokio::test(start_paused = true)]
async fn a_replacement_gets_one_whole_boot_attempt_with_less_time_left_d87() -> TestResult {
    let h = harness(60, 30)?;
    let d = replacement(&h, None, 10 * SECOND).await?;
    h.boot.polls.store(0, Ordering::SeqCst);
    h.boot.finish_after.store(20, Ordering::SeqCst);
    let started = h.clock.now();
    h.prepare(&d).await.map_err(|e| format!("{e:?}"))?;
    assert!(h.clock.now() - started >= 19 * SECOND);
    assert_eq!(h.boot.boots.load(Ordering::SeqCst), 1);
    assert_eq!(d.preparation_episode(), None, "a ready simulator's episode ends");
    h.finish(&[d]).await;
    Ok(())
}

/// Sends that run until they are let go, and say whether the first was dropped (cancelled) or finished.
struct HeldSend {
    release: tokio::sync::Semaphore,
    sends: AtomicUsize,
    dropped: AtomicBool,
    finished: AtomicBool,
}
struct DropFlag<'a>(&'a AtomicBool, bool);
impl Drop for DropFlag<'_> {
    fn drop(&mut self) {
        if !self.1 {
            self.0.store(true, Ordering::SeqCst);
        }
    }
}
impl FileBridge for HeldSend {
    fn send<'a>(&'a self, _: &'a Path, _: &'a str) -> VendorFuture<'a, bool> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let mut flag = DropFlag(&self.dropped, false);
            let _ = self.release.acquire().await;
            flag.1 = true;
            self.finished.store(true, Ordering::SeqCst);
            Ok(true)
        })
    }
    fn receive<'a>(&'a self, _: &'a str, _: &'a Path) -> VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
}

/// D87 (review item 2): the deadline never cancels a transfer. The replacement's whole boot attempt fails, no time is
/// left for a second, and the simulator leaves its pool as not booted only once the installation's send, still in
/// progress, has finished; the send is not cancelled.
#[tokio::test(start_paused = true)]
async fn a_boot_that_runs_out_of_time_leaves_the_transfer_beside_it_to_finish_d87() -> TestResult {
    let h = harness(60, 30)?;
    let bridge = Arc::new(HeldSend {
        release: tokio::sync::Semaphore::new(0),
        sends: AtomicUsize::new(0),
        dropped: AtomicBool::new(false),
        finished: AtomicBool::new(false),
    });
    let d = replacement(&h, Some(bridge.clone()), 10 * SECOND).await?;
    h.boot.finish_after.store(usize::MAX, Ordering::SeqCst);
    let boots_before = h.boot.boots.load(Ordering::SeqCst);
    let preparing = {
        let (d, configuration, context) = (d.clone(), h.services.configuration.clone(), h.services.context.clone());
        tokio::spawn(async move { d.prepare(configuration, context).await })
    };
    tokio::time::sleep(Duration::from_secs(300)).await;
    assert!(!preparing.is_finished(), "it waits for the transfer");
    assert_eq!(bridge.sends.load(Ordering::SeqCst), 1);
    assert!(!bridge.dropped.load(Ordering::SeqCst), "the send was cancelled");
    assert_eq!(
        h.boot.shutdowns.load(Ordering::SeqCst),
        1,
        "the whole attempt was shut down"
    );
    assert_eq!(
        h.boot.boots.load(Ordering::SeqCst) - boots_before,
        1,
        "one whole boot, no second"
    );
    bridge.release.add_permits(tokio::sync::Semaphore::MAX_PERMITS / 2);
    let error = preparing.await?.unwrap_err();
    assert!(not_booted(&error), "{error:?}");
    assert!(bridge.finished.load(Ordering::SeqCst));
    assert!(!bridge.dropped.load(Ordering::SeqCst));
    h.finish(&[d]).await;
    Ok(())
}

/// Counts the commands that were dropped (cancelled) before they finished.
struct Dropped(Arc<AtomicUsize>, bool);
impl Drop for Dropped {
    fn drop(&mut self) {
        if !self.1 {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// D87 (review round 2, item 1): the deadline never cuts a command of a replacement's held attempt; it only keeps one
/// from starting that, run to its worst case, could end after the held bound, the deadline less `shutdown` + `shell`
/// (60 s). A state read is two commands, each at worst 3 × 30 s and `shell` (30 s): 240 s. With 1,500 s left and a
/// 1,000 s `bootWait`, the whole first attempt fails at 1,000 s; the held attempt's probes each take 25 s, its reads start
/// every 26 s from 1,000 s to 1,182 s, and the next, at 1,208 s, could end after 1,440 s, so it does not start and the
/// shutdown follows. No command is dropped, where the held attempt used to cut the probe running at its bound.
#[tokio::test(start_paused = true)]
async fn a_held_boot_never_cuts_a_command_and_starts_none_that_could_outlast_its_bound_d87() -> TestResult {
    let h = harness(1_000, 30)?;
    h.worker.cancellable_before.store(true, Ordering::SeqCst);
    let d = replacement(&h, None, 1_500 * SECOND).await?;
    h.boot.finish_after.store(usize::MAX, Ordering::SeqCst);
    let start = h.clock.now();
    let dropped = Arc::new(AtomicUsize::new(0));
    let count = dropped.clone();
    let seen = on_commands(&h, move |boot, cmd, _| {
        let probe = cmd.get(4).is_some_and(|s| s == "launchctl");
        (probe && boot.shutdowns.load(Ordering::SeqCst) >= 1).then(|| {
            let reply = boot.answer(cmd);
            let flag = Dropped(count.clone(), false);
            Box::pin(async move {
                let mut flag = flag;
                tokio::time::sleep(Duration::from_secs(25)).await;
                flag.1 = true;
                Ok(reply)
            }) as VendorFuture<'static, Option<CommandResult>>
        })
    });
    let error = support::bounded(3_000, h.prepare(&d)).await.unwrap_err();
    assert!(not_booted(&error), "{error:?}");
    assert_eq!(dropped.load(Ordering::SeqCst), 0, "a command was cut");
    let shutdowns: Vec<_> = at(&seen, "shutdown").into_iter().map(|t| t - start).collect();
    assert_eq!(shutdowns.len(), 2, "{shutdowns:?}");
    assert!(
        shutdowns[0] >= 200 * SECOND,
        "the first attempt is whole: {shutdowns:?}"
    );
    assert!(
        (1_207 * SECOND..=1_209 * SECOND).contains(&shutdowns[1]),
        "the held attempt ends when no further read could end by 1,440 s: {shutdowns:?}"
    );
    h.finish(&[d]).await;
    Ok(())
}

/// D87 (review round 2, item 1): a held attempt whose first state read, run to its limit, could end after its bound
/// starts nothing, and boots nothing. With `shell` 60 s a read can take 2 × (60 + 30) s, the reserve is 90 s, and with
/// 230 s left the held attempt starts at about 60 s with 80 s to its bound. The held attempt used to read the state and
/// boot the simulator, cutting whatever ran at its bound.
#[tokio::test(start_paused = true)]
async fn a_held_boot_starts_no_command_that_could_outlast_its_bound_d87() -> TestResult {
    let h = harness(60, 60)?;
    let d = replacement(&h, None, 230 * SECOND).await?;
    h.boot.finish_after.store(usize::MAX, Ordering::SeqCst);
    let seen = on_commands(&h, |_, _, _| None);
    let error = h.prepare(&d).await.unwrap_err();
    assert!(not_booted(&error), "{error:?}");
    assert_eq!(at(&seen, "boot").len(), 1, "only the whole attempt booted");
    h.finish(&[d]).await;
    Ok(())
}

/// Sends that run until they are let go and then report a stall (D54, U163).
struct StallingSend {
    release: tokio::sync::Semaphore,
}
impl FileBridge for StallingSend {
    fn send<'a>(&'a self, _: &'a Path, _: &'a str) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            let _ = self.release.acquire().await;
            Err(VendorError::LeavePool {
                reason: LeavePoolReason::TransferStalled,
                detail: marathon_apple::cmd::failure(
                    marathon_error::FailureKind::Injected(marathon_error::InjectedKind::TransferStalled),
                    "no progress",
                ),
            })
        })
    }
    fn receive<'a>(&'a self, _: &'a str, _: &'a Path) -> VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
}

/// D87 (review round 2, item 4): a transfer that stalls is what the preparation reports, whatever the boot beside it did.
/// The replacement's whole boot fails with no time left for a second; the send then reports a stall. The simulator leaves
/// as a stalled transfer, retired so that no recovery sets it up again (U163), not as not booted.
#[tokio::test(start_paused = true)]
async fn a_stalled_transfer_wins_over_a_boot_out_of_time_beside_it_d87() -> TestResult {
    let h = harness(60, 30)?;
    let bridge = Arc::new(StallingSend {
        release: tokio::sync::Semaphore::new(0),
    });
    let d = replacement(&h, Some(bridge.clone()), 10 * SECOND).await?;
    h.boot.finish_after.store(usize::MAX, Ordering::SeqCst);
    let preparing = {
        let (d, configuration, context) = (d.clone(), h.services.configuration.clone(), h.services.context.clone());
        tokio::spawn(async move { d.prepare(configuration, context).await })
    };
    tokio::time::sleep(Duration::from_secs(300)).await;
    assert!(!preparing.is_finished(), "it waits for the transfer");
    bridge.release.add_permits(tokio::sync::Semaphore::MAX_PERMITS / 2);
    let error = preparing.await?.unwrap_err();
    assert!(
        matches!(
            error,
            VendorError::LeavePool {
                reason: LeavePoolReason::TransferStalled,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(d.retired());
    assert!(!d.lost());
    h.finish(&[d]).await;
    Ok(())
}

/// Every send fails after `after`, counted.
struct SlowFailingSends {
    after: Duration,
    sends: AtomicUsize,
}
impl FileBridge for SlowFailingSends {
    fn send<'a>(&'a self, _: &'a Path, _: &'a str) -> VendorFuture<'a, bool> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            tokio::time::sleep(self.after).await;
            Ok(false)
        })
    }
    fn receive<'a>(&'a self, _: &'a str, _: &'a Path) -> VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
}

/// D87 (review item 5): the installer sends a failed transfer again only before the deadline. A first preparation's send
/// that fails after the deadline has passed is not sent again, where the installer used to make its three attempts.
#[tokio::test(start_paused = true)]
async fn a_send_that_fails_after_the_deadline_is_not_sent_again_d87() -> TestResult {
    let h = harness(60, 30)?;
    let bridge = Arc::new(SlowFailingSends {
        after: Duration::from_secs(400),
        sends: AtomicUsize::new(0),
    });
    let d = h
        .device_on_bridge(
            h.worker.clone(),
            "a",
            Arc::new(WorkerProvisioningContext::new(Transport::Local, h.clock.clone())),
            bridge.clone(),
        )
        .await?;
    // The envelope with these: 2 × (60 + 30 + 30 + 30) s and the pause, about 300 s.
    let left = d.preparation_time_left_millis(&configuration(&h)).ok_or("an episode")?;
    assert!(left < 400 * SECOND);
    let error = h.prepare(&d).await.unwrap_err();
    assert!(matches!(error, VendorError::DeviceSetup(_)), "{error:?}");
    assert_eq!(bridge.sends.load(Ordering::SeqCst), 1);
    h.finish(&[d]).await;
    Ok(())
}

// ---- Recovery: the episode through setup retries, per D44 claim ------------------------------------------------------

use provider_support::{eventually, home};
use recovery_support::{RecoveryHarness, advance};

/// A recovery harness whose simulators' timeouts are `shell`, `boot` and `shutdown` = `command` ms, with `bootWait` the
/// harness's 0.5 s: the envelope is 2 × (0.5 s + 3 × `command`) and the 10 ms pause.
fn recovery(
    command: i64,
    edit: impl FnOnce(&mut marathon_config::vendor::IosConfiguration),
) -> TestResult<RecoveryHarness> {
    RecoveryHarness::settings(
        |v| {
            let t = &mut v.timeout_configuration;
            t.shell = duration(command);
            t.boot = duration(command);
            t.shutdown = duration(command);
            edit(v);
        },
        3,
        30_000,
    )
}

/// The next `getenv a HOME` fails once: the recovery's own setup fails, and D33 sets the simulator up again 30 s later.
/// With `hold_disposal`, the first `simctl shutdown a` after `armed` waits for `gate` and says so on `entered`.
fn fail_next_setup(p: &RecoveryHarness, entered: Option<Hold>) {
    let boot = p.h().boot.clone();
    let fail = Arc::new(AtomicBool::new(false));
    let armed_fail = fail.clone();
    *p.h().worker.before.lock().unwrap() = Arc::new(move |c| {
        let answer = boot.answer(&c);
        let failing = c == ["xcrun", "simctl", "getenv", "a", "HOME"] && armed_fail.swap(false, Ordering::SeqCst);
        let hold = entered
            .as_ref()
            .filter(|(armed, _, _)| c == ["xcrun", "simctl", "shutdown", "a"] && armed.swap(false, Ordering::SeqCst))
            .map(|(_, entered, gate)| (entered.clone(), gate.clone()));
        Box::pin(async move {
            if failing {
                return Err(support::error("setup failed"));
            }
            if let Some((entered, gate)) = hold {
                entered.complete(Ok(()));
                gate.wait().await?;
            }
            Ok(answer)
        })
    });
    fail.store(true, Ordering::SeqCst);
}

/// Opens `a`'s episode, as its device actor does, with the envelope as the deadline.
fn open(
    p: &RecoveryHarness,
    a: &AppleSimulatorDevice,
) -> TestResult<marathon_ios::apple_simulator_device::PreparationEpisode> {
    let mut c = (*p.h().services.configuration).clone();
    c.device_initialization_timeout_millis = 0;
    a.preparation_time_left_millis(&c).ok_or("an episode")?;
    Ok(a.preparation_episode().ok_or("an episode")?)
}

/// D87 (review item 1): the recovery's setup of a simulator that failed to boot fails, and D33 sets it up again 30 s
/// later, within the 181 s deadline. The simulator it registers continues the same episode, deadline and all, instead
/// of starting a whole new one.
#[tokio::test(start_paused = true)]
async fn a_setup_retry_after_a_failed_boot_continues_the_preparations_deadline_d87() -> TestResult {
    let p = recovery(30_000, |_| {})?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let a = p.current("a");
    let episode = open(&p, &a)?;
    fail_next_setup(&p, None);
    p.lose(&a).await;
    let replacement = p.connected(1).await.remove(0);
    assert_eq!(replacement.udid(), "a");
    assert_eq!(home(&p.h().worker, "a"), 3, "the first setup, the recovery's and D33's");
    assert_eq!(
        replacement.preparation_episode().map(|e| e.deadline()),
        Some(episode.deadline()),
        "the retry continues the preparation"
    );
    p.balanced().await
}

/// D87 (review items 1 and 5; round 2, item 2): with 1 s commands the deadline is about 7 s, and D33's retry 30 s after
/// the recovery's failed setup would start past it. The retry asks where the setup starts, its provisioning permit held,
/// and no setup starts: no `getenv HOME` reaches the worker after the recovery's, and the simulator stays out of the run,
/// as after its last recovery. It used to run the setup and drop it only at registration.
#[tokio::test(start_paused = true)]
async fn a_setup_retry_that_would_start_past_the_deadline_sets_nothing_up_d87() -> TestResult {
    let p = recovery(1_000, |_| {})?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let a = p.current("a");
    open(&p, &a)?;
    fail_next_setup(&p, None);
    p.lose(&a).await;
    eventually(|| p.log("not setting it up again")).await;
    advance(100_000).await;
    assert_eq!(
        home(&p.h().worker, "a"),
        2,
        "the first setup and the recovery's, none after the deadline"
    );
    assert!(Arc::ptr_eq(&p.current("a"), &a), "no replacement registered");
    p.update(&["a", "b"]).await?;
    advance(100_000).await;
    assert_eq!(home(&p.h().worker, "a"), 2, "it stays out, as after its last recovery");
    p.balanced().await
}

/// D87 (review round 2, item 3): a simulator that failed to boot and whose worker dropped before the next scan is still a
/// loss of a failed preparation: its recovery counts it and hands its preparation on, so that the worker coming back does
/// not start the time afresh. The replacement, set up once the worker answers again, continues the same deadline. The
/// scan used to take it for a worker's loss, which counts nothing and hands nothing on.
#[tokio::test(start_paused = true)]
async fn a_worker_dropping_after_a_failed_boot_does_not_renew_the_preparations_deadline_d87() -> TestResult {
    let p = recovery(30_000, |_| {})?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let a = p.current("a");
    let episode = open(&p, &a)?;
    p.lose(&a).await;
    // Before any scan: the worker is gone when the scan looks at the simulator.
    p.h().worker.connected.store(false, Ordering::SeqCst);
    advance(5_000).await;
    p.h().worker.connected.store(true, Ordering::SeqCst);
    // `b` shares the worker and comes back too.
    let replacement = loop {
        let device = p.connected(1).await.remove(0);
        if device.udid() == "a" {
            break device;
        }
    };
    assert_eq!(
        replacement.preparation_episode().map(|e| e.deadline()),
        Some(episode.deadline()),
        "the replacement continues the preparation"
    );
    p.balanced().await
}

/// D87 (review item 3): the simulator is removed from the devices file while its old recovery is disposing of it, and
/// the deadline passes meanwhile. The old recovery's claim is stale, so it neither gives the simulator up nor marks it out
/// of the run: added back, the simulator is set up again, as D44 has it. Before, the old recovery marked it exhausted,
/// and nothing set it up again.
#[tokio::test(start_paused = true)]
async fn an_old_recovery_out_of_time_does_not_take_a_simulator_added_back_out_of_the_run_d87() -> TestResult {
    let p = recovery(1_000, |v| {
        v.lifecycle_configuration.on_dispose = vec![marathon_config::vendor::LifecycleAction::Shutdown];
    })?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let a = p.current("a");
    open(&p, &a)?;
    let (armed, entered, gate) = (
        Arc::new(AtomicBool::new(false)),
        SharedDeferred::new(),
        SharedDeferred::new(),
    );
    fail_next_setup(&p, Some((armed.clone(), entered.clone(), gate.clone())));
    p.lose(&a).await;
    // Booting, so that the recovery's disposal shuts it down, and is held there.
    p.h().boot.state.store(3, Ordering::SeqCst);
    armed.store(true, Ordering::SeqCst);
    support::watchdog(entered.wait()).await.map_err(|e| format!("{e:?}"))?;
    let removal = {
        let provider = p.provider.clone();
        let caller = p.h().services.context.clone();
        tokio::spawn(async move {
            provider
                .process_update(
                    provider_support::snapshot(vec![(Transport::Local, provider_support::targets(&["b"]))]),
                    caller,
                )
                .await
        })
    };
    // The removal moves the generation on before it waits for the disposal the recovery holds.
    advance(20_000).await;
    assert!(!removal.is_finished(), "the removal waits for the disposal");
    gate.complete(Ok(()));
    removal.await?.map_err(|e| format!("{e:?}"))?;
    assert!(!p.log("has not got ready within its preparation time"));
    p.update(&["a", "b"]).await?;
    assert_eq!(p.connected(1).await[0].udid(), "a");
    p.balanced().await
}

// ---- Review round 3 ------------------------------------------------------------------------------------------------

/// D87 (review round 3, item 1): a retry the deadline refuses stops nothing beside it. A replacement with 10 s left
/// starts its whole boot; `simctl boot` takes 25 s. At 20 s the first send of the test bundle fails, and the deadline
/// refuses its resend. The preparation lets the boot end on its own, then reports the send's failure; the boot's command
/// is not cancelled, where the refusal used to fail the installation and cancel the boot beside it.
#[tokio::test(start_paused = true)]
async fn a_refused_transfer_retry_leaves_the_boot_beside_it_to_end_d87() -> TestResult {
    let h = harness(60, 30)?;
    h.worker.cancellable_before.store(true, Ordering::SeqCst);
    let bridge = Arc::new(SlowFailingSends {
        after: Duration::from_secs(20),
        sends: AtomicUsize::new(0),
    });
    let d = replacement(&h, Some(bridge.clone()), 10 * SECOND).await?;
    let dropped = Arc::new(AtomicUsize::new(0));
    let count = dropped.clone();
    let seen = on_commands(&h, move |boot, cmd, _| {
        (cmd.len() == 4 && cmd[2] == "boot" && cmd[3] == "a").then(|| {
            let reply = boot.answer(cmd);
            let flag = Dropped(count.clone(), false);
            Box::pin(async move {
                let mut flag = flag;
                tokio::time::sleep(Duration::from_secs(25)).await;
                flag.1 = true;
                Ok(reply)
            }) as VendorFuture<'static, Option<CommandResult>>
        })
    });
    let started = h.clock.now();
    let error = h.prepare(&d).await.unwrap_err();
    assert!(matches!(error, VendorError::DeviceSetup(_)), "{error:?}");
    assert_eq!(bridge.sends.load(Ordering::SeqCst), 1, "the resend was refused");
    assert_eq!(dropped.load(Ordering::SeqCst), 0, "the boot was cancelled");
    assert_eq!(at(&seen, "boot").len(), 1);
    assert!(h.clock.now() - started >= 25 * SECOND, "it waited for the boot");
    h.finish(&[d]).await;
    Ok(())
}

/// D87 (review round 3, item 2): every check of a held attempt counts a command's worst case, from asking for its SSH
/// channel to D30's escalation: 3 × 30 s and its own limit. With 600 s left and a 400 s `bootWait`, the whole first
/// attempt fails at 400 s and the held attempt's bound is 540 s (the deadline less `shutdown` + `shell`). Its commands
/// here take what a slow worker can make them take within their limits: `simctl boot` 15 s, then the descriptor read and
/// the probe 65 s each (20 s opening the channel, 20 s for the exec's acknowledgment, 25 s for the result). Counting only
/// `shell` and D30 for each, a read admitted at 415 s would end at 545 s and the shutdown start with 55 s of its 60 s
/// reserve; counting the worst case, no read starts, and every shutdown keeps its whole reserve.
#[tokio::test(start_paused = true)]
async fn a_held_boot_counts_each_commands_worst_case_so_the_shutdown_keeps_its_reserve_d87() -> TestResult {
    let h = harness(400, 30)?;
    let d = replacement(&h, None, 600 * SECOND).await?;
    h.boot.finish_after.store(usize::MAX, Ordering::SeqCst);
    let start = h.clock.now();
    let seen = on_commands(&h, |boot, cmd, _| {
        let slow = match cmd.first().map(String::as_str) {
            Some("xcrun") if cmd.get(2).is_some_and(|s| s == "boot") && boot.boots.load(Ordering::SeqCst) >= 1 => 15,
            Some("cat") if boot.boots.load(Ordering::SeqCst) >= 2 => 65,
            Some("xcrun") if cmd.get(4).is_some_and(|s| s == "launchctl") && boot.boots.load(Ordering::SeqCst) >= 2 => {
                65
            }
            _ => 0,
        };
        (slow > 0).then(|| {
            let reply = boot.answer(cmd);
            Box::pin(async move {
                tokio::time::sleep(Duration::from_secs(slow)).await;
                Ok(reply)
            }) as VendorFuture<'static, Option<CommandResult>>
        })
    });
    let error = h.prepare(&d).await.unwrap_err();
    assert!(not_booted(&error), "{error:?}");
    let shutdowns: Vec<_> = at(&seen, "shutdown").into_iter().map(|t| t - start).collect();
    assert!(!shutdowns.is_empty());
    assert!(
        shutdowns.iter().all(|t| *t <= 540 * SECOND),
        "a shutdown started within its reserve: {shutdowns:?}"
    );
    h.finish(&[d]).await;
    Ok(())
}

/// D87 (review round 3, item 3): the preparation belongs to the recovery's claim, not to how the loss was classified. A
/// replacement inherits the episode; its worker drops while its first boot is still waiting for `simctl boot`; the scan
/// takes it for a worker's loss. When the worker answers again, the simulator set up continues the same deadline, where
/// a worker's loss used to hand nothing on and the reconnect started the time afresh under the same claim.
#[tokio::test(start_paused = true)]
async fn a_worker_dropping_during_a_replacements_boot_does_not_renew_the_preparations_deadline_d87() -> TestResult {
    let p = recovery(30_000, |_| {})?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let a = p.current("a");
    let episode = open(&p, &a)?;
    p.lose(&a).await;
    let first = loop {
        let device = p.connected(1).await.remove(0);
        if device.udid() == "a" {
            break device;
        }
    };
    assert_eq!(
        first.preparation_episode().map(|e| e.deadline()),
        Some(episode.deadline())
    );
    // Its first boot waits for `simctl boot`, which ends only when the command is cancelled.
    p.h().worker.cancellable_before.store(true, Ordering::SeqCst);
    let boot = p.h().boot.clone();
    *p.h().worker.before.lock().unwrap() = Arc::new(move |c| {
        let answer = boot.answer(&c);
        let hold = c == ["xcrun", "simctl", "boot", "a"];
        Box::pin(async move {
            if hold {
                std::future::pending::<()>().await;
            }
            Ok(answer)
        })
    });
    let preparing = {
        let (d, configuration, context) = (
            first.clone(),
            p.h().services.configuration.clone(),
            p.h().services.context.clone(),
        );
        tokio::spawn(async move { d.prepare(configuration, context).await })
    };
    advance(1_000).await;
    assert!(!preparing.is_finished(), "the first boot is pending");
    p.h().worker.connected.store(false, Ordering::SeqCst);
    advance(5_000).await;
    p.h().worker.connected.store(true, Ordering::SeqCst);
    let second = loop {
        let device = p.connected(1).await.remove(0);
        if device.udid() == "a" {
            break device;
        }
    };
    assert!(!Arc::ptr_eq(&second, &first));
    assert_eq!(
        second.preparation_episode().map(|e| e.deadline()),
        Some(episode.deadline()),
        "the setup after the worker's return continues the preparation"
    );
    let _ = preparing.await;
    p.balanced().await
}
