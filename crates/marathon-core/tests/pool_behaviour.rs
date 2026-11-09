//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/DevicePoolActor.kt (edge-case characterizations).
#[path = "support/pool.rs"]
mod support;
use marathon_core::{
    execution::{DevicePoolMessage, FromDevice, FromQueue, FromScheduler, queue::QueueMessage},
    logging::LogRecord,
    model::*,
    runtime::*,
    vendor::Device,
};
use std::sync::Arc;
use support::*;
fn batch() -> BatchRef {
    Arc::new(TestBatch {
        tests: tests(1, "Behaviour"),
        id: "batch".into(),
    })
}
fn terminate() -> DevicePoolMessage {
    DevicePoolMessage::FromScheduler(FromScheduler::Terminate)
}
fn message(device: &Arc<TestDevice>, event: fn(marathon_core::vendor::DeviceRef) -> FromDevice) -> DevicePoolMessage {
    DevicePoolMessage::FromDevice(event(device.reference()))
}
/// C27's JVM row stays the witness of the old behaviour: a second RemoveDevice of the old device
/// took the replacement with it, so only `keeper` was left.
fn c27_witness() -> serde_json::Value {
    include_str!("../../../fixtures/oracle/pool/scheduler.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|r| r["name"] == "C27")
        .unwrap()
}
fn completed(actor: &marathon_core::execution::device::DeviceActorRef) -> bool {
    matches!(Actor::job(actor.actor.as_ref()).state(), JobState::Completed(_))
}
#[test]
fn stale_disconnect_leaves_the_replacement_d40() {
    assert_eq!(c27_witness()["devices"], serde_json::json!(["keeper"]));
    for _ in 0..50 {
        scheduling_test(|h| {
            Box::pin(async move {
                let gate = Gate::default();
                let old = TestDevice::new("same", None, Some(prepare_on(&gate)), report_passed());
                let new = TestDevice::new("same", None, Some(prepare_on(&gate)), report_passed());
                let keeper = TestDevice::new("keeper", None, Some(prepare_on(&gate)), report_passed());
                let pool = h.start_pool(
                    tests(1, "Behaviour"),
                    PoolOptions {
                        pool: "C27".into(),
                        ..Default::default()
                    },
                );
                pool.add(&old);
                pool.add(&keeper);
                h.run_current().await;
                let original = pool.actor_of(&old).unwrap();
                pool.remove(&old);
                pool.add(&new);
                h.run_current().await;
                assert!(completed(&original));
                let replacement = pool.actor_of(&new).unwrap();
                assert!(!completed(&replacement));
                pool.remove(&old);
                h.run_current().await;
                assert!(!completed(&replacement));
                assert!(!pool.terminated());
                let mismatch = |logs: Vec<LogRecord>| {
                    logs.iter()
                        .filter(|r| r.message == "device same: another instance is in this pool, nothing to remove")
                        .count()
                };
                assert_eq!(mismatch(h.logs()), 1);
                pool.remove(&new);
                h.run_current().await;
                assert!(completed(&replacement));
                assert!(!pool.terminated());
                // A device that was the current one, or is gone altogether, is no mismatch.
                assert_eq!(mismatch(h.logs()), 1);
                pool.send(terminate());
                h.await_pool_termination().await;
            })
        });
    }
}
#[test]
fn stale_disconnect_returns_the_old_batch_and_keeps_the_replacements_d40() {
    for _ in 0..50 {
        scheduling_test(|h| {
            Box::pin(async move {
                let old = TestDevice::new("same", None, None, finish_on_signal());
                let new = TestDevice::new("same", None, None, finish_on_signal());
                let pool = h.start_pool(tests(1, "Behaviour"), Default::default());
                pool.add(&old);
                h.run_current().await;
                assert_eq!(old.batches().len(), 1);
                pool.remove(&old);
                pool.add(&new);
                h.run_current().await;
                // The old actor gave its batch back, and the replacement runs it.
                assert_eq!(new.batches().len(), 1);
                let replacement = pool.actor_of(&new).unwrap();
                pool.remove(&old);
                h.run_current().await;
                assert!(!completed(&replacement));
                assert!(!pool.terminated());
                new.finish.send();
                h.await_pool_termination().await;
                assert!(pool.aggregate_result().await.unwrap());
            })
        });
    }
}
/// The last device's termination, old device first: the replacement holds the pool open whatever
/// stale or duplicate events arrive, and ends it when it goes.
#[test]
fn the_last_device_ends_the_pool_with_the_old_device_removed_first_d40() {
    for _ in 0..50 {
        scheduling_test(|h| {
            Box::pin(async move {
                let gate = Gate::default();
                let old = TestDevice::new("same", None, Some(prepare_on(&gate)), report_passed());
                let new = TestDevice::new("same", None, Some(prepare_on(&gate)), report_passed());
                let pool = h.start_pool(tests(1, "Behaviour"), Default::default());
                pool.add(&old);
                h.run_current().await;
                pool.remove(&old);
                pool.add(&new);
                pool.remove(&old);
                h.run_current().await;
                let replacement = pool.actor_of(&new).unwrap();
                assert!(!completed(&replacement));
                assert!(!pool.terminated());
                pool.remove(&old);
                h.run_current().await;
                assert!(!pool.terminated());
                pool.remove(&new);
                pool.remove(&new);
                h.await_pool_termination().await;
                assert!(completed(&replacement));
            })
        });
    }
}
/// New device first: the pool still holds the old one and rejects the new one, as in Kotlin. The
/// rejected device's disconnect no longer takes the old one with it, and the old one's does.
#[test]
fn the_last_device_ends_the_pool_with_the_new_device_seen_first_d40() {
    for _ in 0..50 {
        scheduling_test(|h| {
            Box::pin(async move {
                let gate = Gate::default();
                let old = TestDevice::new("same", None, Some(prepare_on(&gate)), report_passed());
                let new = TestDevice::new("same", None, Some(prepare_on(&gate)), report_passed());
                let pool = h.start_pool(tests(1, "Behaviour"), Default::default());
                pool.add(&old);
                pool.add(&new);
                h.run_current().await;
                assert!(pool.actor_of(&new).is_none());
                let original = pool.actor_of(&old).unwrap();
                pool.remove(&new);
                h.run_current().await;
                assert!(!completed(&original));
                assert!(!pool.terminated());
                pool.remove(&old);
                h.await_pool_termination().await;
                assert!(completed(&original));
            })
        });
    }
}
/// Duplicates with no replacement: the pool waits for the terminating device, once.
#[test]
fn a_duplicate_disconnect_of_the_last_device_ends_the_pool_once_d40() {
    for _ in 0..50 {
        scheduling_test(|h| {
            Box::pin(async move {
                let gate = Gate::default();
                let d = TestDevice::new("same", None, Some(prepare_on(&gate)), report_passed());
                let pool = h.start_pool(tests(1, "Behaviour"), Default::default());
                pool.add(&d);
                h.run_current().await;
                pool.remove(&d);
                pool.remove(&d);
                h.await_pool_termination().await;
                assert!(pool.terminated());
            })
        });
    }
}
/// RemoveDevice reaches every pool (B3). A device this pool never held changes nothing here;
/// `unknown_disconnect_still_terminates_an_empty_pool` covers the pool with no device.
#[test]
fn a_disconnect_of_another_pools_device_changes_nothing_here_d40() {
    scheduling_test(|h| {
        Box::pin(async move {
            let gate = Gate::default();
            let here = TestDevice::new("here", None, Some(prepare_on(&gate)), report_passed());
            let other = TestDevice::new("other", None, Some(prepare_on(&gate)), report_passed());
            let pool = h.start_pool(tests(1, "Behaviour"), Default::default());
            pool.add(&here);
            h.run_current().await;
            pool.remove(&other);
            h.run_current().await;
            assert!(!pool.terminated());
            pool.send(terminate());
            h.await_pool_termination().await;
        })
    });
}
#[test]
fn stale_termination_does_not_remove_the_replacement_or_its_assignment() {
    scheduling_test(|h| {
        Box::pin(async move {
            let gate = Gate::default();
            let old = TestDevice::new("same", None, Some(prepare_on(&gate)), report_passed());
            let new = TestDevice::new("same", None, None, finish_on_signal());
            let keeper = TestDevice::new("keeper", None, Some(prepare_on(&gate)), report_passed());
            let pool = h.start_pool(tests(1, "Behaviour"), Default::default());
            pool.add(&old);
            pool.add(&keeper);
            h.run_current().await;
            pool.remove(&old);
            pool.add(&new);
            h.run_current().await;
            assert_eq!(new.batches().len(), 1);
            pool.send(message(&old, FromDevice::Terminated));
            pool.send(is_ready(&old));
            pool.send(is_ready(&new));
            h.run_current().await;
            assert_eq!(new.batches().len(), 1);
            assert!(!pool.terminated());
            new.finish.send();
            h.await_pool_termination().await;
            assert!(pool.aggregate_result().await.unwrap());
        })
    });
}
#[test]
fn closed_queue_drops_outcomes_and_rejects_without_stranding_termination() {
    scheduling_test(|h| {
        Box::pin(async move {
            let gate = Gate::default();
            let d = TestDevice::new("serial-A", None, Some(prepare_on(&gate)), report_passed());
            let pool = h.start_pool(tests(1, "Behaviour"), Default::default());
            pool.add(&d);
            h.run_current().await;
            pool.pool.queue.close();
            gate.open();
            h.run_current().await;
            pool.send(notify());
            let batch = batch();
            let failed = d.report(&batch, TestStatus::Failure);
            pool.send(DevicePoolMessage::FromDevice(FromDevice::CompletedTestBatch(
                d.reference(),
                failed,
            )));
            pool.send(DevicePoolMessage::FromDevice(FromDevice::ReturnTestBatch(
                d.reference(),
                batch.clone(),
                "test return".into(),
            )));
            pool.send(DevicePoolMessage::FromQueue(FromQueue::ExecuteBatch(d.info(), batch)));
            h.run_current().await;
            assert!(d.batches().is_empty());
            pool.send(terminate());
            h.await_pool_termination().await;
            let logs = h.logs();
            let drops = logs
                .iter()
                .filter_map(|l| {
                    let s = l.message.to_owned();
                    s.contains("queue is closed").then_some(s)
                })
                .collect::<Vec<_>>();
            for name in [
                "RequestBatch",
                "Completed",
                "ConfirmOutcome",
                "ReturnBatch",
                "RejectBatch",
            ] {
                assert!(
                    drops.iter().any(|l| l.contains(&format!(", {name} is dropped"))),
                    "{name}: {drops:?}"
                );
            }
        })
    });
}
#[test]
fn await_queue_ignores_false_and_discards_later_pool_messages() {
    scheduling_test(|h| {
        Box::pin(async move {
            let pool = h.start_pool(tests(1, "Behaviour"), Default::default());
            pool.send(terminate());
            pool.add(&TestDevice::plain("never-added"));
            h.await_pool_termination().await;
            assert!(pool.actors.borrow().is_empty());
            assert!(pool.pool.actor.state().closed_for_send);
            assert!(matches!(pool.pool.queue.job().state(), JobState::Completed(_)));
            assert!(!pool.try_send(notify()));
        })
    });
}
#[test]
fn await_queue_also_finishes_when_queue_job_completes_without_answering() {
    scheduling_test(|h| {
        Box::pin(async move {
            let pool = h.start_pool(tests(1, "Behaviour"), Default::default());
            let d = TestDevice::plain("serial-A");
            let held = pool.progress.lock().await;
            assert!(pool.pool.queue.try_send(QueueMessage::Completed(
                d.info(),
                d.report(&batch(), TestStatus::Passed)
            )));
            pool.send(terminate());
            h.run_current().await;
            assert!(!pool.terminated());
            assert_eq!(
                pool.pool.queue.actor().state().queued,
                1,
                "IsEmpty was accepted behind the suspended outcome"
            );
            pool.pool.queue.job().cancel(job_was_cancelled());
            drop(held);
            h.await_pool_termination().await;
            assert!(pool.terminated());
        })
    });
}
#[test]
fn all_devices_terminated_error_is_logged_even_after_the_last_result_passed() {
    scheduling_test(|h| {
        Box::pin(async move {
            let d = TestDevice::new("serial-A", None, None, await_cancellation());
            let pool = h.start_pool(
                tests(1, "Behaviour"),
                PoolOptions {
                    pool: "B4".into(),
                    ..Default::default()
                },
            );
            pool.add(&d);
            h.run_current().await;
            pool.send(DevicePoolMessage::FromDevice(FromDevice::CompletedTestBatch(
                d.reference(),
                d.report(&batch(), TestStatus::Passed),
            )));
            pool.send(message(&d, FromDevice::Terminated));
            h.await_pool_termination().await;
            assert!(pool.aggregate_result().await.unwrap());
            let actual = h
                .logs()
                .iter()
                .filter(|l| l.logger == "DevicePoolActor[B4]" && l.level == marathon_logging::Level::ERROR)
                .map(|l| l.message.to_owned())
                .collect::<Vec<_>>();
            let expected: serde_json::Value = include_str!("../../../fixtures/oracle/pool/scheduler.jsonl")
                .lines()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                .find(|r| r["name"] == "B4")
                .unwrap();
            let errors = expected["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["level"] == "ERROR")
                .map(|e| e["message"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            assert_eq!(actual, errors);
            assert_eq!(actual.len(), 1);
        })
    });
}

#[test]
fn parked_devices_keep_insertion_order_with_null_workers_before_retry_workers() {
    scheduling_test(|h| {
        Box::pin(async move {
            // One gate per failing device: released together, a device whose failure was processed
            // early asks again (after its outcome) and can reach the queue just after a later failure
            // added a retry but before the parked devices were told, taking that retry. Kotlin's
            // single-threaded dispatcher processes all four failures first.
            let fail = (0..4).map(|_| Gate::default()).collect::<Vec<_>>();
            // The four failures resume on vendor workers in any order and at any speed; a retry
            // that finished before the last failure arrived would free its device for a second pick.
            let release = Gate::default();
            let pass_on_release = {
                let release = release.clone();
                on_execute(move |e| {
                    let release = release.clone();
                    async move {
                        release.wait_in(&e.context).await?;
                        e.result.complete(Ok(e.device.report(&e.batch, TestStatus::Passed)));
                        Ok(())
                    }
                })
            };
            let ready = (0..4).map(|_| Gate::default()).collect::<Vec<_>>();
            let picked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let observed = picked.clone();
            let pool = h.start_pool(
                tests(4, "Behaviour"),
                PoolOptions {
                    configure: Box::new(|c| c.retry_strategy = fixed_quota(4, 1)),
                    before_execute: Some(std::rc::Rc::new(move |info, _| {
                        observed.borrow_mut().push(info.serial_number.to_owned())
                    })),
                    ..Default::default()
                },
            );
            for (i, gate) in fail.iter().enumerate() {
                pool.add(&TestDevice::new(&format!("failed-{i}"), Some("A"), None, fail_on(gate)));
            }
            let idle = [
                TestDevice::new(
                    "A-first",
                    Some("A"),
                    Some(prepare_on(&ready[0])),
                    pass_on_release.clone(),
                ),
                TestDevice::new("null", None, Some(prepare_on(&ready[1])), pass_on_release.clone()),
                TestDevice::new("B", Some("B"), Some(prepare_on(&ready[2])), pass_on_release.clone()),
                TestDevice::new("A-last", Some("A"), Some(prepare_on(&ready[3])), pass_on_release),
            ];
            for device in &idle {
                pool.add(device);
            }
            h.run_current().await;
            for gate in &ready {
                gate.open();
                h.run_current().await;
            }
            for gate in &fail {
                gate.open();
                h.run_current().await;
            }
            assert_eq!(&picked.borrow()[4..], &["null", "B", "A-first", "A-last"]);
            release.open();
            h.await_pool_termination().await;
            assert_eq!(&picked.borrow()[4..], &["null", "B", "A-first", "A-last"]);
            assert!(pool.aggregate_result().await.unwrap());
        })
    });
}

#[test]
fn duplicate_serial_add_keeps_the_original_device() {
    scheduling_test(|h| {
        Box::pin(async move {
            let prepared = Gate::default();
            let first = TestDevice::new("same", None, Some(prepare_on(&prepared)), report_passed());
            let duplicate = TestDevice::plain("same");
            let pool = h.start_pool(tests(1, "Behaviour"), Default::default());
            pool.add(&first);
            pool.add(&duplicate);
            h.run_current().await;
            assert_eq!(pool.actors.borrow().len(), 1);
            assert!(h.logs().iter().any(|l| l.level == marathon_logging::Level::WARN
                && l.message == "device same already present in pool scheduling"));
            prepared.open();
            h.await_pool_termination().await;
            assert_eq!(first.batches().len(), 1);
            assert!(duplicate.batches().is_empty());
        })
    });
}

#[test]
fn unknown_disconnect_still_terminates_an_empty_pool() {
    scheduling_test(|h| {
        Box::pin(async move {
            let pool = h.start_pool(tests(1, "Behaviour"), Default::default());
            pool.remove(&TestDevice::plain("unknown"));
            h.await_pool_termination().await;
            assert!(pool.terminated());
            assert!(
                h.logs()
                    .iter()
                    .any(|l| l.message.to_owned().ends_with(": noActiveDevices"))
            );
        })
    });
}

#[test]
fn cancelled_pool_finishes_while_queue_is_blocked_without_cancelling_supervisor() {
    scheduling_test(|h| {
        Box::pin(async move {
            let pool = h.start_pool(tests(1, "Behaviour"), Default::default());
            let held = pool.progress.lock().await;
            let d = TestDevice::plain("serial-A");
            assert!(pool.pool.queue.try_send(QueueMessage::Completed(
                d.info(),
                d.report(&batch(), TestStatus::Passed)
            )));
            pool.send(terminate());
            h.run_current().await;
            assert_eq!(pool.pool.queue.actor().state().queued, 1);
            let owner = Actor::job(pool.pool.actor.as_ref());
            let reason = job_was_cancelled();
            owner.cancel(reason.clone());
            h.run_current().await;
            assert_eq!(owner.state(), JobState::Completed(Completion::Cancelled(reason)));
            assert!(pool.pool.actor.state().closed_for_send);
            assert_eq!(pool.pool.supervisor.state(), JobState::Active);
            assert_eq!(pool.pool.queue.job().state(), JobState::Active);
            assert_eq!(pool.pool.queue.actor().state().queued, 1);
            drop(held);
            pool.pool.supervisor.cancel(job_was_cancelled());
            h.run_current().await;
        })
    });
}
