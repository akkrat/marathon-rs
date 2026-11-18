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
) -> Arc<Mutex<Vec<(i64, Vec<String>)>>> {
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

/// D87 (review item 4): a replacement's second, held attempt cuts its final probe where the shutdown and the end of a
/// cut command still fit. With 200 s left at the start, `shutdown` + `shell` = 60 s and D30's 30 s, the held attempt
/// must end its wait 110 s in; its probes never answer, and the shutdown starts there rather than when they time out.
#[tokio::test(start_paused = true)]
async fn a_held_boot_cuts_its_final_probe_so_that_the_shutdown_keeps_its_reserve_d87() -> TestResult {
    let h = harness(60, 30)?;
    h.worker.cancellable_before.store(true, Ordering::SeqCst);
    let d = replacement(&h, None, 200 * SECOND).await?;
    h.boot.finish_after.store(usize::MAX, Ordering::SeqCst);
    let start = h.clock.now();
    let seen = on_commands(&h, |boot, cmd, _| {
        // The second attempt's probes hang, as a probe that needs D30's escalation does.
        let probe = cmd.get(4).is_some_and(|s| s == "launchctl");
        (probe && boot.shutdowns.load(Ordering::SeqCst) >= 1)
            .then(|| Box::pin(std::future::pending()) as VendorFuture<'static, Option<CommandResult>>)
    });
    // A hang guard: without the cut, the hanging probes would hold the attempt for ever.
    let error = support::bounded(600, h.prepare(&d)).await.unwrap_err();
    assert!(not_booted(&error), "{error:?}");
    let shutdowns: Vec<_> = at(&seen, "shutdown").into_iter().map(|t| t - start).collect();
    assert_eq!(shutdowns.len(), 2, "{shutdowns:?}");
    assert!(shutdowns[0] >= 60 * SECOND, "the first attempt is whole: {shutdowns:?}");
    assert!(
        (110 * SECOND..=111 * SECOND).contains(&shutdowns[1]),
        "the held attempt's shutdown starts at its bound: {shutdowns:?}"
    );
    h.finish(&[d]).await;
    Ok(())
}

/// D87 (review item 4): a held attempt whose first state read ends after its bound boots nothing. With `shell` 60 s the
/// reserve is 90 + 30 s; with 230 s left the held attempt must end 110 s in, and its read takes from about 60 s to 115 s.
#[tokio::test(start_paused = true)]
async fn a_held_boot_whose_state_read_outlasts_its_bound_does_not_boot_d87() -> TestResult {
    let h = harness(60, 60)?;
    let d = replacement(&h, None, 230 * SECOND).await?;
    h.boot.finish_after.store(usize::MAX, Ordering::SeqCst);
    let slow = Arc::new(AtomicBool::new(true));
    let seen = on_commands(&h, move |boot, cmd, _| {
        let read = cmd.first().is_some_and(|s| s == "cat");
        (read && boot.shutdowns.load(Ordering::SeqCst) >= 1 && slow.swap(false, Ordering::SeqCst)).then(|| {
            let reply = boot.answer(cmd);
            Box::pin(async move {
                tokio::time::sleep(Duration::from_secs(55)).await;
                Ok(reply)
            }) as VendorFuture<'static, Option<CommandResult>>
        })
    });
    let error = h.prepare(&d).await.unwrap_err();
    assert!(not_booted(&error), "{error:?}");
    assert_eq!(at(&seen, "boot").len(), 1, "only the whole attempt booted");
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

use marathon_core::executor::shared::SharedDeferred;
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
fn fail_next_setup(
    p: &RecoveryHarness,
    entered: Option<(Arc<AtomicBool>, Arc<SharedDeferred<()>>, Arc<SharedDeferred<()>>)>,
) {
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

/// D87 (review items 1 and 5): with 1 s commands the deadline is about 7 s, and D33's retry 30 s after the recovery's
/// failed setup starts past it: its registration is dropped and the simulator stays out of the run, as after its last
/// recovery. Before, it registered with a fresh preparation of its own.
#[tokio::test(start_paused = true)]
async fn a_setup_retry_that_registers_past_the_deadline_is_dropped_d87() -> TestResult {
    let p = recovery(1_000, |_| {})?;
    p.start(&["a", "b"]).await?;
    p.connected(2).await;
    let a = p.current("a");
    open(&p, &a)?;
    fail_next_setup(&p, None);
    p.lose(&a).await;
    eventually(|| p.log("its new setup is dropped")).await;
    advance(100_000).await;
    assert_eq!(home(&p.h().worker, "a"), 3);
    assert!(Arc::ptr_eq(&p.current("a"), &a), "no replacement registered");
    p.update(&["a", "b"]).await?;
    advance(100_000).await;
    assert_eq!(home(&p.h().worker, "a"), 3, "it stays out, as after its last recovery");
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
