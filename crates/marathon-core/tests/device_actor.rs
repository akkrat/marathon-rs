//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/device/DeviceActorTest.kt
//! and the DeviceActor.kt rules docs/wp/3.6.md lists, each test named after its rule.
#[path = "support/device_actor.rs"]
mod device_actor;
#[path = "support/device_actor_scenarios.rs"]
mod device_actor_scenarios;
mod support;
use device_actor::*;
use device_actor_scenarios::run_scenario;
use marathon_core::execution::FromDevice;

fn returned(outcome: &Outcome, reason: &str) -> FromDevice {
    FromDevice::ReturnTestBatch(outcome.device.clone(), outcome.batch.clone(), reason.into())
}

#[test]
fn terminate_while_in_progress() {
    let outcome = run_scenario("terminate while in progress");
    let messages = outcome.received();
    assert!(messages.first() == Some(&FromDevice::IsReady(outcome.device.clone())));
    assert!(
        messages.get(1) == Some(&returned(&outcome, "Device serial-1 terminated")),
        "{:?}",
        outcome.describe()
    );
}

#[test]
fn device_lost_while_in_progress_returns_the_batch_and_notifies_the_pool() {
    let outcome = run_scenario("device lost while in progress returns the batch and notifies the pool");
    let messages = outcome.received();
    assert!(
        messages
            == [
                FromDevice::IsReady(outcome.device.clone()),
                returned(&outcome, "Device serial-1 terminated"),
                FromDevice::Terminated(outcome.device.clone()),
            ],
        "{:?}",
        outcome.describe()
    );
}

#[test]
fn batch_failure_after_reported_results_sends_the_results_only() {
    let outcome = run_scenario("batch failure after reported results sends the results only");
    let messages = outcome.received();
    assert!(messages.first() == Some(&FromDevice::IsReady(outcome.device.clone())));
    // Kotlin compares the two as a set.
    let empty = FromDevice::CompletedTestBatch(
        outcome.device.clone(),
        marathon_core::model::TestBatchResults {
            device: outcome.device.clone(),
            finished: vec![],
            failed: vec![],
            uncompleted: vec![],
        },
    );
    let rest = &messages[1..];
    assert_eq!(rest.len(), 2, "{:?}", outcome.describe());
    assert!(rest.contains(&empty) && rest.contains(&FromDevice::IsReady(outcome.device.clone())));
}

#[test]
fn batch_failure_without_results_returns_the_batch_only() {
    let outcome = run_scenario("batch failure without results returns the batch only");
    let messages = outcome.received();
    let [
        FromDevice::IsReady(_),
        FromDevice::ReturnTestBatch(_, batch, _),
        FromDevice::IsReady(_),
    ] = messages.as_slice()
    else {
        panic!("{:?}", outcome.describe());
    };
    assert!(std::sync::Arc::ptr_eq(batch, &outcome.batch));
}

#[test]
fn a_preparation_that_fails_with_a_cancellation_of_its_own_terminates_the_device_at_once_without_retries() {
    let outcome = run_scenario(
        "a preparation that fails with a cancellation of its own terminates the device at once, without retries",
    );
    assert!(
        outcome.received() == [FromDevice::Terminated(outcome.device.clone())],
        "{:?}",
        outcome.describe()
    );
    assert_eq!(outcome.calls.0, 1);
}

// ---- DeviceActor.kt rules (docs/wp/3.6.md, "Behaviours to reproduce") -------------------

use device_actor_scenarios::SCENARIOS;
use marathon_core::{
    error::*, execution::device::DeviceEvent, executor::hooks::Tape, runtime::Actor, time::ReplayHooks,
};
use std::{cell::RefCell, rc::Rc, sync::Arc};

fn texts(outcome: &Outcome) -> Vec<String> {
    outcome
        .logs()
        .iter()
        .map(|r| {
            let level = match r.level {
                marathon_logging::Level::ERROR => "Error",
                marathon_logging::Level::WARN => "Warn",
                marathon_logging::Level::INFO => "Info",
                marathon_logging::Level::DEBUG => "Debug",
                marathon_logging::Level::TRACE => "Trace",
            };
            format!("{level} {}", r.message.to_owned())
        })
        .collect()
}
fn awaiting_cancellation() -> ExecuteScript {
    execute_script(|e| async move { Err(VendorError::Cancelled(e.context.cancellation.cancelled().await)) })
}

#[test]
fn invalid_transitions_log_an_error_and_have_no_side_effect() {
    let outcome = scenario(PoolMode::Open, |run| {
        Box::pin(async move {
            run.start(ScriptedDevice::new(
                Stub::timed(1000, 0),
                None,
                Some(awaiting_cancellation()),
            ));
            let execute = || DeviceEvent::Execute(run.batch.clone());
            run.send(DeviceEvent::Complete);
            run.send(execute());
            run.run_current().await;
            run.send(DeviceEvent::Initialize);
            run.run_current().await;
            run.send(DeviceEvent::Initialize);
            run.send(execute());
            run.run_current().await;
            run.advance_by(1000).await;
            run.send(DeviceEvent::Initialize);
            run.send(DeviceEvent::Complete);
            run.run_current().await;
            run.send(execute());
            run.run_current().await;
            run.send(DeviceEvent::Initialize);
            run.send(execute());
            run.run_current().await;
            // Terminated before the terminate task closes the actor: Complete and
            // Terminate are dontTransition (no log), Initialize is invalid.
            run.send(DeviceEvent::Terminate);
            run.send(DeviceEvent::Complete);
            run.send(DeviceEvent::Terminate);
            run.send(DeviceEvent::Initialize);
            run.run_current().await;
        })
    });
    let id = outcome.batch.id.to_owned();
    let invalid: Vec<String> = texts(&outcome)
        .into_iter()
        .filter(|t| t.contains("Invalid transition"))
        .collect();
    let expected = [
        "connected event complete".to_owned(),
        format!("connected event execute batch={id}"),
        "initializing event initialize".to_owned(),
        format!("initializing event execute batch={id}"),
        "ready event initialize".to_owned(),
        "ready event complete".to_owned(),
        format!("running batch={id} event initialize"),
        format!("running batch={id} event execute batch={id}"),
        "terminated event initialize".to_owned(),
    ]
    .map(|cell| format!("Error Invalid transition from {cell}"));
    assert_eq!(invalid, expected);
    assert_eq!(outcome.calls, (1, 1));
    assert_eq!(
        outcome.describe(),
        [
            "IsReady",
            "ReturnTestBatch(same batch=true, reason=Device serial-1 terminated)",
            "Terminated"
        ]
    );
}

/// The own-job callback runs synchronously inside `started` when the owner is
/// already complete; the transition it makes must not find the machine borrowed.
#[test]
fn an_actor_under_a_cancelled_parent_terminates_from_started() {
    let outcome = scenario(PoolMode::Open, |run| {
        Box::pin(async move {
            run.parent.cancel(job_was_cancelled());
            run.start(ScriptedDevice::stub(Stub::timed(0, 0)));
            run.run_current().await;
        })
    });
    assert_eq!(outcome.describe(), ["Terminated"]);
    assert_eq!(texts(&outcome), ["Debug terminate serial-1"]);
    assert_eq!(outcome.calls, (0, 0));
}

/// A pool that closes the actor while receiving the outcome (inside the Terminate
/// side effect) re-enters the actor's job cascade synchronously.
#[test]
fn side_effects_tolerate_synchronous_cancellation_from_the_pool() {
    let outcome = scenario(PoolMode::Open, |run| {
        Box::pin(async move {
            let actor = run.start(ScriptedDevice::new(
                Stub::timed(0, 0),
                None,
                Some(awaiting_cancellation()),
            ));
            *run.pool.on_message.borrow_mut() = Some(Box::new(move |message| {
                if matches!(message, FromDevice::ReturnTestBatch(..)) {
                    actor.actor.close();
                }
            }));
            run.send(DeviceEvent::Initialize);
            run.run_current().await;
            run.send(DeviceEvent::Execute(run.batch.clone()));
            run.run_current().await;
            run.send(DeviceEvent::Terminate);
            run.run_current().await;
            *run.pool.on_message.borrow_mut() = None;
        })
    });
    assert_eq!(
        outcome.describe(),
        [
            "IsReady",
            "ReturnTestBatch(same batch=true, reason=Device serial-1 terminated)",
            "Terminated"
        ]
    );
    assert!(outcome.job_completed());
}

#[test]
fn readiness_is_a_sibling_under_the_owner_not_the_execute_task() {
    let seen = Arc::new(std::sync::Mutex::new(vec![]));
    let record = seen.clone();
    let outcome = scenario(PoolMode::Open, move |run| {
        Box::pin(async move {
            let actor = run.start(ScriptedDevice::scripted(execute_script(|e| async move {
                e.result.complete(Ok(passed(&e.device, &e.batch)));
                Ok(())
            })));
            let states = Rc::new(RefCell::new(vec![]));
            let into = states.clone();
            *run.pool.on_message.borrow_mut() = Some(Box::new(move |message| {
                if matches!(message, FromDevice::IsReady(_)) {
                    into.borrow_mut().push(actor.execute_job().map(|job| job.state()));
                }
            }));
            run.send(DeviceEvent::Initialize);
            run.run_current().await;
            run.send(DeviceEvent::Execute(run.batch.clone()));
            run.run_current().await;
            *run.pool.on_message.borrow_mut() = None;
            let states = states.borrow().clone();
            // Under the execute task, the second IsReady would keep it Completing.
            *record.lock().unwrap() = states.iter().map(|s| format!("{s:?}")).collect();
        })
    });
    assert_eq!(*seen.lock().unwrap(), ["None", "Some(Completed(Success))"]);
    assert_eq!(outcome.describe()[2], "IsReady");
}

#[test]
fn readiness_is_lost_when_the_owner_is_cancelled_before_it_runs() {
    let outcome = run_scenario("results of a completed batch reach the pool even if the actor stops right after");
    assert_eq!(
        outcome.describe(),
        [
            "IsReady",
            "CompletedTestBatch(finished=1, failed=0, uncompleted=0)",
            "Terminated"
        ]
    );
}

/// `cancelAndJoin` is cancel + a cancellable join: a terminate task whose owner is
/// cancelled stops waiting for the vendor's cleanup, and the own-job callback
/// sends Terminated once the cleanup ends.
#[test]
fn terminate_joins_are_cancellable() {
    let children = Arc::new(std::sync::Mutex::new(0));
    let count = children.clone();
    let outcome = scenario(PoolMode::Open, move |run| {
        Box::pin(async move {
            run.start_batch(ScriptedDevice::scripted(execute_script(|e| async move {
                let reason = e.context.cancellation.cancelled().await;
                // Cleanup that ignores cancellation, on the scheduler clock.
                let never = marathon_core::executor::Signal::new();
                e.context.clock.delay(5000, never).await?;
                Err(VendorError::Cancelled(reason))
            })))
            .await;
            run.send(DeviceEvent::Terminate);
            run.run_current().await;
            run.advance_by(1000).await;
            let owner = run.actor().actor.job();
            let before = owner.children_snapshot().len();
            run.parent.cancel(job_was_cancelled());
            run.run_current().await;
            // The terminate task and the receiver are gone; the execute task waits.
            *count.lock().unwrap() = before * 10 + owner.children_snapshot().len();
            run.advance_until_idle().await;
        })
    });
    // Receiver, execute and terminate tasks, then the execute task alone.
    assert_eq!(*children.lock().unwrap(), 31);
    assert_eq!(
        outcome.describe(),
        [
            "IsReady",
            "ReturnTestBatch(same batch=true, reason=Device serial-1 terminated)",
            "Terminated"
        ]
    );
    let terminated = outcome.entries.iter().find_map(|e| match e {
        Entry::Pool {
            at,
            message: FromDevice::Terminated(_),
            ..
        } => Some(*at),
        _ => None,
    });
    assert_eq!(terminated, Some(5000));
}

#[test]
fn thirty_failing_preparations_retry_every_ten_seconds_and_log_each_attempt() {
    let outcome = run_scenario("thirty failing preparations");
    let retries: Vec<i64> = outcome
        .logs()
        .iter()
        .filter(|r| r.message.to_owned().ends_with("initialization failed. Retrying"))
        .map(|r| r.at_millis)
        .collect();
    // K2: the 30th failure is logged as "Retrying" too.
    assert_eq!(retries, (0..30).map(|i| i * 10_000).collect::<Vec<_>>());
    assert_eq!(outcome.calls.0, 30);
    assert_eq!(outcome.describe(), ["Terminated"]);
    assert_eq!(outcome.now, 290_000);
}

#[test]
fn a_failed_preparation_is_retried_until_it_succeeds() {
    let outcome = scenario(PoolMode::Open, |run| {
        Box::pin(async move {
            let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let prepare = prepare_script(move |_| {
                let attempt = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    if attempt < 2 {
                        Err(VendorError::DeviceSetup(ise("adb offline")))
                    } else {
                        Ok(())
                    }
                }
            });
            run.start(ScriptedDevice::new(Stub::default(), Some(prepare), None));
            run.send(DeviceEvent::Initialize);
            run.run_current().await;
            run.advance_until_idle().await;
        })
    });
    assert_eq!(outcome.calls.0, 3);
    assert_eq!(outcome.describe(), ["IsReady"]);
    let ready_at = outcome.entries.iter().find_map(|e| match e {
        Entry::Pool { at, .. } => Some(*at),
        _ => None,
    });
    assert_eq!(ready_at, Some(20_000));
}

/// Kotlin's catch table over what the vendor threw (DeviceActor.kt:190–207).
#[test]
fn every_vendor_outcome_maps_to_its_log_and_transition() {
    let unknown = "Error Unknown vendor exception caught. Considering this a recoverable error";
    let ise_trace = "Unknown vendor exception caught:\ninvalid state: boom\n";
    let returned = |reason: &str| format!("ReturnTestBatch(same batch=true, reason={reason})");
    let terminated = |log: &str| {
        (
            Some(log.to_owned()),
            vec![returned("Device serial-1 terminated"), "Terminated".to_owned()],
        )
    };
    let completed =
        |log: Option<&str>, reason: &str| (log.map(str::to_owned), vec![returned(reason), "IsReady".to_owned()]);
    let cancelled = "Warn Device execution has been cancelled";
    type Expected = (Option<String>, Vec<String>);
    let cases: Vec<(&str, Option<VendorError>, Expected)> = vec![
        ("ok", None, completed(None, "Device serial-1 reported no results")),
        (
            "cancelled",
            Some(vendor_cancellation("timed out")),
            terminated(cancelled),
        ),
        (
            "leave pool",
            Some(VendorError::LeavePool {
                reason: LeavePoolReason::NotAdmitted,
                detail: ise("not admitted"),
            }),
            terminated(cancelled),
        ),
        (
            "device lost",
            Some(device_lost(ise("gone"))),
            terminated("Error Critical error during execution"),
        ),
        (
            "batch execution",
            Some(batch_failure("boom")),
            completed(
                Some("Warn Test batch failed execution"),
                "Test batch failed execution:\ntest batch execution failure: boom\n",
            ),
        ),
        (
            "device setup",
            Some(VendorError::DeviceSetup(ise("boom"))),
            completed(Some(unknown), ise_trace),
        ),
        (
            "incompatible device",
            Some(VendorError::IncompatibleDevice(ise("boom"))),
            completed(Some(unknown), ise_trace),
        ),
        (
            "test parsing",
            Some(VendorError::TestParsing(ise("boom"))),
            completed(Some(unknown), ise_trace),
        ),
        (
            "no devices",
            Some(VendorError::NoDevices(ise("boom"))),
            completed(Some(unknown), ise_trace),
        ),
        (
            "transfer",
            Some(VendorError::Transfer(ise("boom"))),
            completed(Some(unknown), ise_trace),
        ),
        (
            "other",
            Some(VendorError::Other(ise("boom"))),
            completed(Some(unknown), ise_trace),
        ),
    ];
    for (name, error, (log, messages)) in cases {
        let outcome = scenario(PoolMode::Open, move |run| {
            Box::pin(async move {
                run.start_batch(ScriptedDevice::scripted(execute_script(move |_| {
                    let error = error.clone();
                    async move { error.map_or(Ok(()), Err) }
                })))
                .await;
            })
        });
        let logs = texts(&outcome);
        assert_eq!(
            logs[..2],
            ["Debug initialize serial-1", "Debug executeBatch serial-1"],
            "{name}"
        );
        let classified = logs.get(2).filter(|l| !l.starts_with("Debug"));
        assert_eq!(classified, log.as_ref(), "{name}: {logs:?}");
        assert_eq!(outcome.describe()[1..], messages, "{name}");
    }
    // A panicking vendor is an unknown, recoverable exception.
    let outcome = scenario(PoolMode::Open, |run| {
        Box::pin(async move {
            run.start_batch(ScriptedDevice::scripted(execute_script(|_| async {
                panic!("boom");
            })))
            .await;
        })
    });
    assert!(texts(&outcome).contains(&unknown.to_owned()));
    assert_eq!(
        outcome.describe()[1..],
        [
            returned("Unknown vendor exception caught:\nruntime failure: boom\n"),
            "IsReady".to_owned()
        ]
    );
}

/// What the vendor returned decides, not why the core cancelled it.
#[test]
fn the_vendors_own_outcome_wins_over_the_core_cancellation() {
    let outcome = scenario(PoolMode::Open, |run| {
        Box::pin(async move {
            run.start_batch(ScriptedDevice::scripted(execute_script(|e| async move {
                e.context.cancellation.cancelled().await;
                Ok(())
            })))
            .await;
            run.send(DeviceEvent::Terminate);
            run.run_current().await;
        })
    });
    assert!(
        !texts(&outcome).iter().any(|t| t.contains("cancelled")),
        "{:?}",
        texts(&outcome)
    );
    assert_eq!(
        outcome.describe(),
        [
            "IsReady",
            "ReturnTestBatch(same batch=true, reason=Device serial-1 terminated)",
            "Terminated"
        ]
    );
}

#[test]
fn terminated_is_sent_at_most_once() {
    for name in SCENARIOS {
        let outcome = run_scenario(name);
        let terminated = outcome
            .entries
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Entry::Pool {
                        message: FromDevice::Terminated(_),
                        ..
                    }
                )
            })
            .count();
        assert!(terminated <= 1, "{name}: {terminated}");
    }
}

#[test]
fn a_closed_pool_drops_the_outcome_with_a_warning_and_terminated_silently() {
    let outcome = scenario(PoolMode::Open, |run| {
        Box::pin(async move {
            run.start_batch(ScriptedDevice::scripted(awaiting_cancellation())).await;
            run.pool.close();
            run.send(DeviceEvent::Terminate);
            run.run_current().await;
        })
    });
    let id = outcome.batch.id.to_owned();
    let warnings: Vec<String> = texts(&outcome).into_iter().filter(|t| t.starts_with("Warn")).collect();
    assert_eq!(
        warnings,
        [
            format!("Warn Pool is closed, outcome of batch {id} is dropped"),
            "Warn Device execution has been cancelled".to_owned()
        ]
    );
    let failed: Vec<&str> = outcome
        .entries
        .iter()
        .filter_map(|e| match e {
            Entry::Pool { via, .. } if via != "send" => Some(via.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(failed, ["trySend-failed", "trySend-failed"]);
}

fn tape(barriers: &[&str]) -> Option<Tape> {
    Some(Tape {
        barriers: barriers.iter().map(|b| (*b).to_owned()).collect(),
        ..Tape::default()
    })
}

#[test]
fn prepare_and_execute_are_admitted_through_their_barriers() {
    let verified = Arc::new(std::sync::Mutex::new(None));
    let result = verified.clone();
    scenario_with(
        PoolMode::Open,
        tape(&["core.DeviceActor.prepare:serial-1", "core.DeviceActor.execute:serial-1"]),
        move |run| {
            Box::pin(async move {
                run.start_batch(ScriptedDevice::scripted(execute_script(|_| async { Ok(()) })))
                    .await;
                *result.lock().unwrap() = Some(run.hooks.verify_consumed());
            })
        },
    );
    assert_eq!(*verified.lock().unwrap(), Some(Ok(())));
}

#[test]
fn each_prepare_attempt_passes_its_own_barrier() {
    let verified = Arc::new(std::sync::Mutex::new(None));
    let result = verified.clone();
    let token = "core.DeviceActor.prepare:serial-1";
    let outcome = scenario_with(PoolMode::Open, tape(&[token, token, token]), move |run| {
        Box::pin(async move {
            let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let prepare = prepare_script(move |_| {
                let attempt = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    if attempt < 2 {
                        Err(VendorError::Other(ise("retry")))
                    } else {
                        Ok(())
                    }
                }
            });
            run.start(ScriptedDevice::new(Stub::default(), Some(prepare), None));
            run.send(DeviceEvent::Initialize);
            run.run_current().await;
            run.advance_until_idle().await;
            *result.lock().unwrap() = Some(run.hooks.verify_consumed());
        })
    });
    assert_eq!(*verified.lock().unwrap(), Some(Ok(())));
    assert_eq!(outcome.calls.0, 3);
}

/// Cancelled while waiting for admission: the vendor is never called and the token
/// stays unconsumed; the task's own cancellation is logged as K1 has it.
#[test]
fn cancellation_before_barrier_admission_leaves_the_token_unconsumed() {
    let verified = Arc::new(std::sync::Mutex::new(None));
    let result = verified.clone();
    let outcome = scenario_with(
        PoolMode::Open,
        tape(&["core.DeviceActor.prepare:other", "core.DeviceActor.prepare:serial-1"]),
        move |run| {
            Box::pin(async move {
                run.start(ScriptedDevice::stub(Stub::timed(0, 0)));
                run.send(DeviceEvent::Initialize);
                run.run_current().await;
                run.send(DeviceEvent::Terminate);
                run.run_current().await;
                *result.lock().unwrap() = Some(run.hooks.verify_consumed());
            })
        },
    );
    assert_eq!(outcome.calls.0, 0);
    assert_eq!(
        texts(&outcome),
        [
            "Debug initialize serial-1",
            "Debug terminate serial-1",
            "Error Error task was cancelled"
        ]
    );
    assert_eq!(outcome.describe(), ["Terminated"]);
    let verified = verified.lock().unwrap().clone().unwrap();
    assert_eq!(
        verified.map_err(|e| e.message.to_owned()),
        Err("unused barrier entries".to_owned())
    );
}

#[test]
fn terminate_during_preparation_logs_the_task_cancellation_as_an_error() {
    // K1: catch (e: Exception) also catches the initialize task's own cancellation.
    let outcome = run_scenario("terminate during preparation");
    assert_eq!(
        texts(&outcome),
        [
            "Debug initialize serial-1",
            "Debug terminate serial-1",
            "Error Error task was cancelled"
        ]
    );
}

// R1-claude-5: Kotlin's `Notify(retryWorkers: Set<String>)` is a data class over a Set.
#[test]
fn notify_compares_retry_workers_as_a_set() {
    use marathon_core::execution::FromQueue;
    let notify = |workers: &[&str]| FromQueue::Notify {
        retry_workers: workers.iter().map(|w| (*w).into()).collect(),
    };
    assert_eq!(notify(&["a", "b"]), notify(&["b", "a"]));
    assert_ne!(notify(&["a", "b"]), notify(&["a"]));
    assert_ne!(notify(&["a"]), FromQueue::Terminated);
}

// Review round 1, decision 4: the rendered time is the wall time of the call, not of the drain.
#[test]
fn log_records_carry_wall_time_from_creation() {
    use marathon_core::logging::LogQueue;
    use marathon_logging::Level;
    let queue = LogQueue::default();
    let now = Rc::new(std::cell::Cell::new(45_296_789_i64)); // 12:34:56.789
    let clock = now.clone();
    queue.set_wall_clock(Rc::new(move || clock.get()));
    queue.log("DevicePool[p]_DeviceActor[s]", 0, Level::WARN, "called".into(), None);
    now.set(0);
    let record = queue.take().remove(0);
    assert_eq!(record.wall_millis, 45_296_789);
    assert_eq!(record.message, "called");
    assert_eq!(record.target(), "marathon_core::execution::device::actor");
}

#[test]
fn returning_a_batch_projects_its_diagnostic_but_preserves_reason_data_x56_r1() {
    let mut failure = ise("outer");
    failure.stacktrace = Some("\tSuppressed: job cancelled: LazyDeferredCoroutine was cancelled\n\tCaused by: timed out: Timeout expired: 30000 MILLISECONDS\n".into());
    for error in [
        VendorError::BatchExecution(failure.clone()),
        VendorError::Other(failure),
    ] {
        let prefix = if matches!(error, VendorError::BatchExecution(_)) {
            "Test batch failed execution:\n"
        } else {
            "Unknown vendor exception caught:\n"
        };
        let original_reason = format!("{prefix}{}", error.failure().stack_trace());
        let outcome = scenario(PoolMode::Open, move |run| {
            Box::pin(async move {
                run.start_batch(ScriptedDevice::scripted(execute_script(move |_| {
                    let error = error.clone();
                    async move { Err(error) }
                })))
                .await;
            })
        });
        let messages = outcome.received();
        let reason = messages
            .iter()
            .find_map(|message| match message {
                FromDevice::ReturnTestBatch(_, _, reason) => Some(reason),
                _ => None,
            })
            .unwrap();
        assert_eq!(reason, &original_reason);
        let logs = outcome.logs();
        let message = &logs
            .iter()
            .find(|log| log.message.starts_with("Returning batch "))
            .unwrap()
            .message;
        assert!(message.contains("task was cancelled") && message.contains("no response after 30 s"));
        assert!(!message.contains("LazyDeferredCoroutine") && !message.contains("MILLISECONDS"));
    }
}
