//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt
//! D87: a simulator whose readiness probe never succeeds, while another simulator keeps its pool alive, leaves the run
//! within one preparation deadline instead of after three replacements. The real scheduler, pool, device actor,
//! provider, recovery and boot loop run on the fake worker with short timeouts (below), so that the deadline,
//! 2 × (`bootWait` + `boot` + `shutdown` + `shell`) + the pause between boot attempts, is 9.1 s.
use marathon_apple::cmd::CommandResult;
#[path = "../../marathon-core/tests/support/pool.rs"]
mod pool_support;
mod provider_support;
mod recovery_support;
mod simulator_support;
mod support;
use marathon_core::{
    analytics::Analytics,
    ci::Ci,
    execution::scheduler::{Scheduler, SchedulerParts},
    executor::{CoreExecutor, hooks::RuntimeHooks, shared::requested},
    model::TestShard,
    output::OutputDriver,
    runtime::JobKind,
    vendor::DeviceProvider,
};
use marathon_ios::device::simulator_provider::{AppleSimulatorProvider, recovery::SimulatorRecovery};
use simulator_support::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

const BOOT_WAIT: i64 = 3_000;
/// `shell`, `boot` and `shutdown` alike.
const COMMAND: i64 = 500;
const BOOT_RETRY_PAUSE: i64 = 100;
const BOOT_POLL: i64 = 20;
const RECOVERY_PAUSE: i64 = 300;
/// D87's envelope with these: 2 × (3,000 + 500 + 500 + 500) + 100 ms.
const DEADLINE: i64 = 2 * (BOOT_WAIT + 3 * COMMAND) + BOOT_RETRY_PAUSE;
/// The run's give-up messages, after the deadline (D87) or after the third replacement (Kotlin's limit).
const GIVEN_UP: &str = "not re-provisioning it again";
const HANG_GUARD: std::time::Duration = std::time::Duration::from_secs(90);

#[derive(Debug, Default)]
struct Timeline {
    /// Harness-clock millis of each setup (`simctl getenv a HOME`), boot and shutdown of simulator `a`.
    setups: Vec<i64>,
    boots: Vec<i64>,
    shutdowns: Vec<i64>,
    /// When the provider gave `a` up, and with which message.
    given_up: Option<(i64, String)>,
    /// When the sibling `b`, held in its preparation, was let go.
    released: Option<i64>,
    passed: bool,
    pool_lines: Vec<String>,
}

/// `a`'s probe fails where `probe_fails` says, given the setup (1 is the first) and how many times `a` was booted; `a`'s
/// first preparation is held `hold_a` ms before its boot; `b`, with `sibling`, is held in its preparation until `a` has
/// been given up.
fn run(
    sibling: bool,
    probe_fails: impl Fn(usize, usize) -> bool + Send + Sync + 'static,
    device_wait: i64,
    hold_a: u64,
) -> TestResult<Timeline> {
    let mut p = recovery_support::RecoveryHarness::settings(
        |v| {
            let t = &mut v.timeout_configuration;
            t.boot_wait = support::duration(BOOT_WAIT);
            t.shell = support::duration(COMMAND);
            t.boot = support::duration(COMMAND);
            t.shutdown = support::duration(COMMAND);
        },
        3,
        RECOVERY_PAUSE,
    )?;
    p.base.h.services.timeouts.boot_retry_pause_millis = BOOT_RETRY_PAUSE;
    p.base.h.services.timeouts.boot_poll_interval_millis = BOOT_POLL;
    let devices = if sibling {
        "[{type: simulator, udid: a}, {type: simulator, udid: b}]"
    } else {
        "[{type: simulator, udid: a}]"
    };
    std::fs::write(
        p.h().dir.path().join("Marathondevices"),
        format!("workers:\n- transport: {{type: local}}\n  devices: {devices}\n"),
    )?;
    let timeline = Arc::new(Mutex::new(Timeline::default()));
    let setups = Arc::new(AtomicUsize::new(0));
    {
        let boot = p.h().boot.clone();
        let clock = p.h().clock.clone();
        let logs = p.h().logs.clone();
        let timeline = timeline.clone();
        let held = Arc::new(AtomicUsize::new(0));
        *p.h().worker.before.lock().unwrap() = Arc::new(move |command| {
            let now = clock.now();
            let cmd: Vec<_> = command.iter().map(String::as_str).collect();
            let mut t = timeline.lock().unwrap();
            let mut hold = false;
            let mut pause = 0;
            let answer = match cmd.as_slice() {
                ["xcrun", "simctl", "getenv", "a", "HOME"] => {
                    setups.fetch_add(1, Ordering::SeqCst);
                    t.setups.push(now);
                    boot.answer(&command)
                }
                ["xcrun", "simctl", "boot", "a"] => {
                    t.boots.push(now);
                    boot.answer(&command)
                }
                ["xcrun", "simctl", "shutdown", "a"] => {
                    t.shutdowns.push(now);
                    boot.answer(&command)
                }
                // The readiness probe exits non-zero at once, whatever it printed.
                ["xcrun", "simctl", "spawn", "a", ..] if probe_fails(setups.load(Ordering::SeqCst), t.boots.len()) => {
                    Some(CommandResult {
                        stdout: vec!["A com.apple.springboard.services".into()],
                        stderr: vec![],
                        exit_code: Some(1),
                    })
                }
                // `b` joined the pool before its preparation, so it is live (D48) while it is held here.
                ["pkill", "-f", what] if what.contains(" a ") && t.boots.is_empty() => {
                    pause = hold_a;
                    boot.answer(&command)
                }
                ["pkill", "-f", what] if what.contains(" b ") => {
                    hold = held.fetch_add(1, Ordering::SeqCst) == 0;
                    boot.answer(&command)
                }
                _ => boot.answer(&command),
            };
            drop(t);
            let (clock, logs, timeline) = (clock.clone(), logs.clone(), timeline.clone());
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_millis(pause)).await;
                if hold {
                    let start = std::time::Instant::now();
                    loop {
                        let given_up = logs
                            .lock()
                            .unwrap()
                            .iter()
                            .find(|(_, _, message, _)| message.contains(GIVEN_UP))
                            .map(|(_, _, message, _)| message.clone());
                        if let Some(message) = given_up {
                            let mut t = timeline.lock().unwrap();
                            t.given_up.get_or_insert((clock.now(), message));
                            t.released = Some(clock.now());
                            break;
                        }
                        if start.elapsed() > HANG_GUARD {
                            timeline.lock().unwrap().released = Some(clock.now());
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                }
                Ok(answer)
            })
        });
    }
    // Whichever simulator is ready runs the batch, and passes.
    let batches = Arc::new(BatchScript::default());
    *batches.plan.lock().unwrap() = OutputPlan {
        lines: [passed(1), passed(2)].concat(),
        ..Default::default()
    };
    batches.attach(&p.h().worker);
    let result = timeline.clone();
    let logs = p.h().logs.clone();
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
                    configuration.device_initialization_timeout_millis = device_wait;
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
            {
                let mut t = result.lock().unwrap();
                t.passed = passed;
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
    let mut timeline = std::mem::take(&mut *timeline.lock().unwrap());
    if timeline.given_up.is_none() {
        timeline.given_up = logs
            .lock()
            .unwrap()
            .iter()
            .find(|(_, _, message, _)| message.contains(GIVEN_UP))
            .map(|(_, _, message, _)| (i64::MAX, message.clone()));
    }
    eprintln!("{timeline:#?}");
    Ok(timeline)
}

/// The worst case D87 documents: the deadline plus one whole boot attempt of a replacement that started before it.
const ONE_BOOT: i64 = BOOT_WAIT + 3 * COMMAND;

/// D87, the whole run in real time: an integration check of the bound only. How many replacements fit depends on the
/// host's speed (a stall after the first preparation can rightly leave none), so the exact sequence is witnessed in
/// virtual time (`simulator_preparation_episode`). The first preparation fails after both of its boot waits, at about
/// 6.1 s of the 9.1 s deadline; whatever starts before the deadline runs, and `a` is given up within the deadline plus one
/// whole boot attempt, where Kotlin's limits gave it three replacements and about 25 s with these timeouts (26 to 30
/// minutes with the defaults). `b`, live all along, then runs the tests. `deviceInitializationTimeoutMillis` is 1 s, below
/// the envelope, so the deadline is the envelope; with `b` live D48 does not apply.
#[test]
fn a_simulator_whose_probe_never_succeeds_leaves_within_its_preparation_deadline_while_a_sibling_keeps_the_pool_d87()
-> TestResult {
    let t = run(true, |_, _| true, 1_000, 0)?;
    assert!(t.passed, "{t:?}");
    let first = t.setups[0];
    let (given_up, message) = t.given_up.clone().ok_or("a was given up")?;
    assert!(message.contains("within its preparation time"), "{t:?}");
    assert!(given_up - first <= DEADLINE + ONE_BOOT, "{t:?}");
    assert!(
        t.pool_lines.iter().all(|l| !l.contains("remaining tests will not run")),
        "{t:?}"
    );
    Ok(())
}

/// D87 keeps a single failure's recovery: the probe fails in the first preparation only, and the replacement, within what
/// is left of the deadline, boots at its first attempt and runs the tests. On the pool's only simulator, with D48's wait
/// shorter than the deadline (the deadline is the greater of the two).
#[test]
fn a_replacement_within_the_preparation_deadline_still_recovers_a_simulator_d87() -> TestResult {
    let t = run(false, |setup, _| setup <= 1, 5_000, 0)?;
    assert!(t.passed, "{t:?}");
    assert_eq!(t.setups.len(), 2, "{t:?}");
    assert_eq!(t.boots.len(), 3, "{t:?}");
    assert!(t.given_up.is_none(), "{t:?}");
    Ok(())
}

/// D87 leaves a first preparation's boots as they were: here it is held longer than its whole deadline before it boots,
/// and its first boot fails once. The second boot attempt still waits the whole of `bootWait`, the simulator gets ready
/// and runs the tests, with no replacement.
#[test]
fn a_first_preparation_held_past_its_deadline_keeps_its_second_boot_attempt_d87() -> TestResult {
    let t = run(
        false,
        |setup, boots| setup == 1 && boots == 1,
        1_000,
        DEADLINE as u64 + 500,
    )?;
    assert!(t.passed, "{t:?}");
    assert_eq!(t.setups.len(), 1, "{t:?}");
    assert_eq!(t.boots.len(), 2, "{t:?}");
    assert!(t.boots[0] - t.setups[0] > DEADLINE, "{t:?}");
    assert!(t.given_up.is_none(), "{t:?}");
    Ok(())
}
