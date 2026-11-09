//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt
//! D38 on a pool's only simulator, after D44 and D48: what a readiness probe that never succeeds costs when no other
//! simulator keeps the pool alive. The real scheduler, pool, device actor, provider, recovery and boot loop run with every
//! default that sets the timeline divided by [`SCALE`]: `bootWait` 180 s, the 10 s pause between boot attempts, the 1 s
//! poll, the 30 s recovery pause step and `deviceInitializationTimeoutMillis` 180 s. The figures in
//! docs/wp/x42-findings.md are these timelines multiplied back.
use marathon_error::FailureKind;
#[path = "../../marathon-core/tests/support/pool.rs"]
mod pool_support;
mod provider_support;
mod recovery_support;
mod simulator_support;
mod support;
use marathon_apple::cmd::CommandResult;
use marathon_core::{
    analytics::Analytics,
    ci::Ci,
    execution::scheduler::{Scheduler, SchedulerParts},
    executor::{CoreExecutor, hooks::RuntimeHooks, shared::requested},
    model::TestShard,
    output::OutputDriver,
    runtime::{CoreRuntime, JobKind},
    vendor::DeviceProvider,
};
use marathon_ios::device::simulator_provider::{AppleSimulatorProvider, recovery::SimulatorRecovery};
use simulator_support::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

const SCALE: i64 = 100;
const BOOT_WAIT: i64 = 180_000 / SCALE;
const BOOT_RETRY_PAUSE: i64 = 10_000 / SCALE;
const BOOT_POLL: i64 = 1_000 / SCALE;
const RECOVERY_PAUSE: i64 = 30_000 / SCALE;
const DEVICE_WAIT: i64 = 180_000 / SCALE;
/// How long the re-setup is held before it can register, so that it joins the pool halfway into D48's wait. Half the wait
/// is left for the rest of its setup to register before the wait expires, and a wait that its arrival restarted would end
/// at least this much later than the one its loss started, which is what the upper bounds below tell apart: each side has
/// half the wait, 900 ms, as its margin (docs/testing.md rule 6).
const READMISSION_DELAY: i64 = DEVICE_WAIT / 2;

/// From which setup of the simulator on the probe fails: 1 is the first setup, 2 the first re-setup after a loss.
#[derive(Clone, Copy)]
enum From {
    FirstSetup,
    ReSetup,
}

#[derive(Debug, Default)]
struct Timeline {
    /// Harness-clock millis of each setup (`simctl getenv HOME`), boot and shutdown, and of the scheduler's return.
    setups: Vec<i64>,
    boots: Vec<i64>,
    shutdowns: Vec<i64>,
    first_failed_probe: Option<i64>,
    /// When the held re-setup was let go, just before it registers with the pool.
    released: Option<i64>,
    batch_starts: Vec<i64>,
    batches: usize,
    ended: i64,
    passed: bool,
    pool_lines: Vec<String>,
}

fn probe_never_succeeds_on_the_only_simulator(from: From) -> TestResult<Timeline> {
    let mut p = recovery_support::RecoveryHarness::settings(
        |v| v.timeout_configuration.boot_wait = support::duration(BOOT_WAIT),
        3,
        RECOVERY_PAUSE,
    )?;
    p.base.h.services.timeouts.boot_retry_pause_millis = BOOT_RETRY_PAUSE;
    p.base.h.services.timeouts.boot_poll_interval_millis = BOOT_POLL;
    std::fs::write(
        p.h().dir.path().join("Marathondevices"),
        "workers:\n- transport: {type: local}\n  devices: [{type: simulator, udid: a}]\n",
    )?;
    let timeline = Arc::new(Mutex::new(Timeline::default()));
    let setups = Arc::new(AtomicUsize::new(0));
    {
        let boot = p.h().boot.clone();
        let clock = p.h().clock.clone();
        let timeline = timeline.clone();
        let setups = setups.clone();
        let failing_from = match from {
            From::FirstSetup => 1,
            From::ReSetup => 2,
        };
        *p.h().worker.before.lock().unwrap() = Arc::new(move |command| {
            let now = clock.now();
            let cmd: Vec<_> = command.iter().map(String::as_str).collect();
            let mut t = timeline.lock().unwrap();
            let mut hold = 0;
            let answer = match cmd.as_slice() {
                ["xcrun", "simctl", "getenv", _, "HOME"] => {
                    if setups.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                        hold = READMISSION_DELAY;
                    }
                    t.setups.push(now);
                    boot.answer(&command)
                }
                ["xcrun", "simctl", "boot", _] => {
                    t.boots.push(now);
                    boot.answer(&command)
                }
                ["xcrun", "simctl", "shutdown", _] => {
                    t.shutdowns.push(now);
                    boot.answer(&command)
                }
                // (a): the probe exits non-zero at once, whatever it printed.
                ["xcrun", "simctl", "spawn", ..] if setups.load(Ordering::SeqCst) >= failing_from => {
                    t.first_failed_probe.get_or_insert(now);
                    Some(CommandResult {
                        stdout: vec!["A com.apple.springboard.services".into()],
                        stderr: vec![],
                        exit_code: Some(1),
                    })
                }
                _ => boot.answer(&command),
            };
            drop(t);
            let clock = clock.clone();
            let timeline = timeline.clone();
            Box::pin(async move {
                if hold > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(hold as u64)).await;
                    timeline.lock().unwrap().released = Some(clock.now());
                }
                Ok(answer)
            })
        });
    }
    let silent = Arc::new(BatchScript::default());
    *silent.plan.lock().unwrap() = OutputPlan {
        // Three silent batches in a row lose the simulator (D39, U165): the loss that starts the re-setup.
        lines: vec!["Test Case '-[Suite test1]' started.".into()],
        failure: Some(marathon_apple::cmd::failure(
            FailureKind::Timeout,
            "idle timeout reached",
        )),
        ..Default::default()
    };
    silent.attach(&p.h().worker);
    {
        // The start of each batch, so that the loss is known to within one silent batch.
        let inner = p.h().worker.session.lock().unwrap().clone();
        let clock = p.h().clock.clone();
        let timeline = timeline.clone();
        *p.h().worker.session.lock().unwrap() = Arc::new(move |cmd| {
            let session = inner(cmd)?;
            if session.is_some() {
                timeline.lock().unwrap().batch_starts.push(clock.now());
            }
            Ok(session)
        });
    }
    let result = timeline.clone();
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            p.base.h.services.runtime = core.vendor();
            let provider = AppleSimulatorProvider::new(provider_support::ProviderHarness::services(
                p.h(),
                p.connections.clone(),
                SimulatorRecovery::with_recovery_settings(3, RECOVERY_PAUSE),
            ))
            .map_err(|e| e.failure())?;
            provider
                .initialize(p.h().services.context.clone())
                .await
                .map_err(|e| e.failure().clone())?;
            let hooks = RuntimeHooks::live();
            let parent = core.job(JobKind::Supervisor, None);
            let scheduler = Scheduler::new(SchedulerParts {
                core: core.clone(),
                provider: provider.clone(),
                analytics: Arc::new(Analytics::from_lookup(Arc::new(pool_support::NoMetrics))),
                configuration: {
                    let mut configuration = (*p.h().services.configuration).clone();
                    configuration.device_initialization_timeout_millis = DEVICE_WAIT;
                    configuration.uncompleted_test_retry_quota = 100;
                    Arc::new(configuration)
                },
                shard: TestShard {
                    tests: tests(),
                    flaky_tests: vec![],
                },
                tracker: Arc::new(pool_support::RecordingTracker::default()),
                timer: p.h().services.timer.clone(),
                bundle_identifier: Some(p.h().services.identifier.clone()),
                hooks: hooks.clone(),
                barriers: hooks.barriers(),
                ci: Ci::None,
                output: OutputDriver {
                    vendor: core.vendor(),
                    sink: Arc::new(pool_support::NullSink),
                },
                context: core.context(parent.clone()),
            });
            let passed = scheduler.execute().await.map_err(|e| e.failure().clone())?;
            let ended = p.h().clock.now();
            assert_eq!(scheduler.pools().len(), 1);
            assert!(scheduler.pools()[0].1.terminated());
            {
                let mut t = result.lock().unwrap();
                t.ended = ended;
                t.passed = passed;
                t.batches = silent.count.load(Ordering::SeqCst);
                t.pool_lines = core
                    .log_queue()
                    .records()
                    .into_iter()
                    .map(|r| r.message.to_owned())
                    .filter(|m| m.contains("pool "))
                    .collect();
            }
            provider.terminate().await.map_err(|e| e.failure().clone())?;
            parent.cancel(requested("test finished"));
            let _ = parent.join().await;
            Ok(())
        })
    })
    .map_err(|e| format!("{e:?}"))?;
    let timeline = std::mem::take(&mut *timeline.lock().unwrap());
    eprintln!("{timeline:#?}");
    Ok(timeline)
}

fn count(lines: &[String], text: &str) -> usize {
    lines.iter().filter(|l| l.contains(text)).count()
}

/// The probe fails from the first setup: the simulator joined the pool, so it is live (D48), and its first prepare fails
/// after two boot waits. D48's wait starts there; the first recovery has no pause, and the test holds its registration
/// until well into the wait. A device added while the pool lacks one is not live until it is ready, so its arrival neither
/// moves nor restarts the wait, and it is stopped when the wait ends. The run ends one failed prepare plus one wait after it began, with `remaining tests will not run`, during
/// the second setup: the later recoveries never start.
#[test]
fn a_probe_failing_from_the_first_setup_of_the_only_simulator_ends_the_run_one_wait_after_the_first_failed_prepare_d38_d48()
-> TestResult {
    let t = probe_never_succeeds_on_the_only_simulator(From::FirstSetup)?;
    assert!(!t.passed);
    assert_eq!(t.batches, 0, "no batch runs");
    // The first prepare: boot, a whole bootWait, shutdown, the pause, boot again, a whole bootWait, shutdown. Each
    // bootWait starts before its descriptor read, ahead of `simctl boot`, so the bound counts from the setup's first
    // command, which comes before both; counting from the first boot would fail a correct run whose read is slow.
    assert!(t.shutdowns.len() >= 2, "{t:?}");
    let failed = t.shutdowns[1];
    assert!(failed - t.setups[0] >= 2 * BOOT_WAIT + BOOT_RETRY_PAUSE, "{t:?}");
    // One re-setup, which the end of the wait stops; none after it.
    assert_eq!(t.setups.len(), 2, "{t:?}");
    assert!(t.setups[1] >= failed, "{t:?}");
    assert_eq!(
        count(
            &t.pool_lines,
            "no device returned within deviceInitializationTimeoutMillis"
        ),
        1,
        "{t:?}"
    );
    assert_eq!(count(&t.pool_lines, "remaining tests will not run"), 1, "{t:?}");
    assert_eq!(count(&t.pool_lines, "is not ready when the wait ends"), 1, "{t:?}");
    // The run ends no earlier than one wait after the failed prepare, and at the end of that wait, not of one restarted
    // when the held re-setup arrived, which would end at least READMISSION_DELAY later.
    let released = t.released.ok_or("the re-setup was held")?;
    assert!(released < t.ended, "{t:?}");
    assert!(t.ended - failed >= DEVICE_WAIT, "{t:?}");
    assert!(t.ended - failed < DEVICE_WAIT + READMISSION_DELAY, "{t:?}");
    Ok(())
}

/// The probe fails from the first re-setup after the simulator was in the pool running batches: its loss starts D48's
/// wait, the re-setup, held until well into the wait, arrives and is not live, and the run ends one wait after the loss, before the re-setup's
/// prepare has failed, so no recovery pause and no further setup counts.
#[test]
fn a_probe_failing_from_a_re_setup_of_the_only_simulator_ends_the_run_one_wait_after_its_loss_d38_d48() -> TestResult {
    let t = probe_never_succeeds_on_the_only_simulator(From::ReSetup)?;
    assert!(!t.passed);
    assert_eq!(t.batches, 3, "three silent batches lose the simulator");
    assert_eq!(t.setups.len(), 2, "{t:?}");
    // The third silent batch starts just before the loss, and the re-setup just after it.
    let lost = t.batch_starts[2];
    let probe = t.first_failed_probe.ok_or("the re-setup probed")?;
    assert!(probe >= t.setups[1] && t.setups[1] >= lost, "{t:?}");
    assert_eq!(
        count(
            &t.pool_lines,
            "no device returned within deviceInitializationTimeoutMillis"
        ),
        1,
        "{t:?}"
    );
    assert_eq!(count(&t.pool_lines, "remaining tests will not run"), 1, "{t:?}");
    assert_eq!(count(&t.pool_lines, "is not ready when the wait ends"), 1, "{t:?}");
    // At least one wait after the loss, and at the end of that wait, not of one restarted when the held re-setup arrived,
    // which would end at least READMISSION_DELAY later; the re-setup's prepare cannot have failed by then either.
    let released = t.released.ok_or("the re-setup was held")?;
    assert!(released < t.ended, "{t:?}");
    assert!(t.ended - lost >= DEVICE_WAIT, "{t:?}");
    assert!(t.ended - lost < DEVICE_WAIT + READMISSION_DELAY, "{t:?}");
    Ok(())
}
