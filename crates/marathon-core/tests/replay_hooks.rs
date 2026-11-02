//! Port of core/src/main/kotlin/com/malinskiy/marathon/time/SystemTimer.kt
//! (WP 0.2b hook/tape seams, Rust-side coverage).
mod support;
use marathon_core::{
    executor::{CoreExecutor, RuntimeClock, hooks::*},
    runtime::*,
    time::*,
};
use std::collections::HashMap;
use support::*;
#[test]
fn clocks_use_tape_modes_and_independent_per_site_read_cursors() {
    run(|core, driver| {
        Box::pin(async move {
            let mut tape = Tape {
                wall: ClockMode::Step { start: 100, step: 3 },
                monotonic: ClockMode::Scheduler(9),
                ..Tape::default()
            };
            tape.reads.insert("core.di.Clock".into(), vec![17, 2]);
            let hooks = RuntimeHooks::taped(tape, core.clock().as_ref());
            assert_eq!(replay(hooks.millis("core.di.Clock")), 17);
            assert_eq!(replay(hooks.millis("core.di.Clock")), 2);
            assert_eq!(replay(hooks.millis("core.TemporalTestResult.startTime")), 100);
            assert_eq!(replay(hooks.millis("core.TemporalTestResult.startTime")), 103);
            assert_eq!(replay(hooks.millis("core.TemporalTestResult.endTime")), 100);
            replay(driver.advance_to(10).await);
            assert_eq!(replay(hooks.nanos("ios.AppleSimulatorDevice.nanoTime")), 10_000_009);
            replay(hooks.verify_consumed());
            Ok(())
        })
    });
}
#[test]
fn wall_instant_and_scheduler_time_remain_separate() {
    run(|core, driver| {
        Box::pin(async move {
            let hooks = RuntimeHooks::taped(
                Tape {
                    wall: ClockMode::Frozen(-1),
                    monotonic: ClockMode::Frozen(42),
                    ..Tape::default()
                },
                core.clock().as_ref(),
            );
            replay(driver.advance_to(100).await);
            let instant = replay(hooks.instant("core.di.Clock"));
            assert_eq!(instant.seconds(), -1);
            assert_eq!(instant.nanos(), 999_000_000);
            assert_eq!(replay(hooks.nanos("ios.AppleSimulatorDevice.nanoTime")), 42);
            Ok(())
        })
    });
}
#[test]
fn signed_draws_key_selection_and_identity_aliases_consume_exact_entries() {
    run(|core, driver| {
        Box::pin(async move {
            let tape = Tape {
                draws: HashMap::from([
                    ("core.RandomOrderSortingStrategy.key".into(), vec![Draw::Int(i32::MIN)]),
                    (
                        "android.AdamDeviceProvider.borrow".into(),
                        vec![Draw::Key("serial".into())],
                    ),
                ]),
                ids: HashMap::from([(
                    "config.Configuration.identities".into(),
                    Ids::Values(vec!["one".into(), "two".into()]),
                )]),
                ..Tape::default()
            };
            let hooks = RuntimeHooks::taped(tape, core.clock().as_ref());
            assert_eq!(replay(hooks.draw_int("core.RandomOrderSortingStrategy.key")), i32::MIN);
            assert_eq!(
                replay(hooks.choose_key("android.AdamDeviceProvider.borrow", &["serial".into()])),
                "serial".into()
            );
            let first = replay(hooks.identity_suffix("config.Configuration.identities", ObjectIdentity(1)));
            assert_eq!(
                first,
                replay(hooks.identity_suffix("config.Configuration.identities", ObjectIdentity(1)))
            );
            assert_ne!(
                first,
                replay(hooks.identity_suffix("config.Configuration.identities", ObjectIdentity(2)))
            );
            driver.set_hooks(hooks);
            replay(driver.finish());
            Ok(())
        })
    });
}
#[test]
fn exhausted_tapes_fail_without_host_fallback_and_caught_errors_stay_visible() {
    run(|core, driver| {
        Box::pin(async move {
            let mut tape = Tape::default();
            tape.draws
                .insert("core.RandomOrderSortingStrategy.key".into(), vec![Draw::Int(1)]);
            let hooks = RuntimeHooks::taped(tape, core.clock().as_ref());
            assert_eq!(replay(hooks.draw_int("core.RandomOrderSortingStrategy.key")), 1);
            assert!(hooks.draw_int("core.RandomOrderSortingStrategy.key").is_err());
            assert!(hooks.verify_consumed().is_err());
            assert!(hooks.verify_consumed().is_err());
            driver.set_hooks(hooks);
            assert!(driver.finish().is_err());
            Ok(())
        })
    });
}
#[test]
fn unused_explicit_entries_fail_and_patterns_have_no_unused_tail() {
    run(|core, _| {
        Box::pin(async move {
            let hooks = RuntimeHooks::taped(
                Tape {
                    ids: HashMap::from([(
                        "core.TestBatch.id".into(),
                        Ids::Pattern {
                            prefix: "00000000-0000-0000-0000-".into(),
                            width: 12,
                            hex: false,
                            suffix: String::new(),
                            start: 1,
                        },
                    )]),
                    ..Tape::default()
                },
                core.clock().as_ref(),
            );
            assert_eq!(
                replay(hooks.allocate_id("core.TestBatch.id")),
                "00000000-0000-0000-0000-000000000001".into()
            );
            replay(hooks.verify_consumed());
            for kind in 0..3 {
                let mut tape = Tape::default();
                match kind {
                    0 => {
                        tape.reads.insert("core.di.Clock".into(), vec![1]);
                    }
                    1 => {
                        tape.draws
                            .insert("core.RandomOrderSortingStrategy.key".into(), vec![Draw::Int(1)]);
                    }
                    _ => {
                        tape.ids
                            .insert("core.TestBatch.id".into(), Ids::Values(vec!["unused".into()]));
                    }
                }
                assert!(
                    RuntimeHooks::taped(tape, core.clock().as_ref())
                        .verify_consumed()
                        .is_err()
                );
            }
            Ok(())
        })
    });
}
#[test]
fn virtual_clock_and_scheduler_hooks_require_a_driver() {
    CoreExecutor::run(|core| {
        Box::pin(async move {
            let clock = RuntimeClock::virtual_clock();
            let job = core.job(JobKind::Ordinary, None);
            assert!(clock.delay(1, job.clone()).await.is_err());
            let hooks = RuntimeHooks::taped(
                Tape {
                    wall: ClockMode::Scheduler(0),
                    ..Tape::default()
                },
                clock.as_ref(),
            );
            assert!(hooks.millis("core.di.Clock").is_err());
            assert!(hooks.verify_consumed().is_err());
            job.complete();
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn barriers_admit_in_order_and_cancelled_wait_does_not_consume_token() {
    run(|core, driver| {
        Box::pin(async move {
            let hooks = RuntimeHooks::taped(
                Tape {
                    barriers: vec!["core.DeviceActor.prepare:a".into(), "core.DeviceActor.prepare:b".into()],
                    ..Tape::default()
                },
                core.clock().as_ref(),
            );
            let barriers = hooks.barriers();
            let b = "b".into();
            let mut waiting = barriers.enter("core.DeviceActor.prepare", &b);
            assert!(futures_util::poll!(waiting.as_mut()).is_pending());
            drop(waiting);
            let a = "a".into();
            let first = replay(barriers.enter("core.DeviceActor.prepare", &a).await);
            let mut waiting = barriers.enter("core.DeviceActor.prepare", &b);
            assert!(futures_util::poll!(waiting.as_mut()).is_pending());
            replay(first.finish());
            let second = replay(waiting.await);
            replay(second.finish());
            replay(hooks.verify_consumed());
            driver.set_hooks(hooks);
            replay(driver.finish());
            Ok(())
        })
    });
}
#[test]
fn duplicate_and_missing_barriers_are_sticky_errors() {
    run(|core, _| {
        Box::pin(async move {
            let hooks = RuntimeHooks::taped(
                Tape {
                    barriers: vec!["core.DeviceActor.prepare:a".into()],
                    ..Tape::default()
                },
                core.clock().as_ref(),
            );
            let barriers = hooks.barriers();
            let key = "a".into();
            let first = replay(barriers.enter("core.DeviceActor.prepare", &key).await);
            assert!(barriers.enter("core.DeviceActor.prepare", &key).await.is_err());
            replay(first.finish());
            assert!(hooks.verify_consumed().is_err());
            Ok(())
        })
    });
}
#[test]
fn live_hooks_share_seams_without_tape_state() {
    let hooks = RuntimeHooks::live();
    let millis = replay(hooks.millis("core.di.Clock"));
    assert!(millis > 0);
    let key = replay(hooks.choose_key("android.AdamDeviceProvider.borrow", &["only".into()]));
    assert_eq!(key, "only".into());
    let id = replay(hooks.allocate_id("core.TestBatch.id"));
    assert_eq!(id.as_utf8().unwrap().len(), 36);
    let identity = ObjectIdentity(7);
    assert_eq!(
        replay(hooks.identity_suffix("config.Configuration.identities", identity)),
        replay(hooks.identity_suffix("config.Configuration.identities", identity))
    );
    let _ = replay(hooks.draw_int("core.RandomOrderSortingStrategy.key"));
    replay(hooks.verify_consumed());
}
#[path = "support/tape.rs"]
mod tape;
#[test]
fn checked_in_scenario_tape_drives_clock_ids_and_aliases() {
    run(|core, driver| {
        Box::pin(async move {
            let hooks = RuntimeHooks::taped(tape::scenario_tape(), core.clock().as_ref());
            assert_eq!(
                replay(hooks.allocate_id("test.ConfigurationFactory.outputDir")),
                "scenario".into()
            );
            replay(driver.advance_to(17).await);
            assert_eq!(replay(hooks.millis("core.di.Clock")), 1_577_836_800_017);
            assert_eq!(
                replay(hooks.allocate_id("core.TestBatch.id")),
                "00000000-0000-0000-0000-000000000001".into()
            );
            assert_eq!(
                replay(hooks.identity_suffix("core.RawJsonReporter.identities", ObjectIdentity(1))),
                "00002000".into()
            );
            driver.set_hooks(hooks);
            replay(driver.finish());
            Ok(())
        })
    });
}
#[test]
fn live_and_unspecified_barriers_pass_through() {
    run(|core, _| {
        Box::pin(async move {
            for hooks in [
                RuntimeHooks::live(),
                RuntimeHooks::taped(Tape::default(), core.clock().as_ref()),
            ] {
                let barriers = hooks.barriers();
                let key = "device".into();
                let permit = replay(barriers.enter("core.DeviceActor.prepare", &key).await);
                replay(permit.finish());
                replay(hooks.verify_consumed());
            }
            Ok(())
        })
    });
}
