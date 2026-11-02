//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/DevicePoolActor.kt (edge-case characterizations).
#[path = "support/pool.rs"]
mod support;
use marathon_core::{
    execution::{DevicePoolMessage, FromDevice, FromQueue, FromScheduler, queue::QueueMessage},
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
#[test]
fn stale_disconnect_removes_the_replacement() {
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
                pool.remove(&old);
                pool.add(&new);
                h.run_current().await;
                let replacement = pool.actor_of(&new).unwrap();
                assert!(!matches!(
                    Actor::job(replacement.actor.as_ref()).state(),
                    JobState::Completed(_)
                ));
                pool.remove(&old);
                h.run_current().await;
                assert!(matches!(
                    Actor::job(replacement.actor.as_ref()).state(),
                    JobState::Completed(_)
                ));
                assert!(!pool.terminated());
                let expected: serde_json::Value = include_str!("../../../fixtures/oracle/pool/scheduler.jsonl")
                    .lines()
                    .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                    .find(|r| r["name"] == "C27")
                    .unwrap();
                assert_eq!(expected["devices"], serde_json::json!(["keeper"]));
                pool.send(terminate());
                h.await_pool_termination().await;
            })
        });
    }
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
                    let s = l.message.to_string_lossy();
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
                .filter(|l| l.logger == "DevicePoolActor[B4]" && l.level == jcompat::logging::Level::Error)
                .map(|l| l.message.to_string_lossy())
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
            let fail = Gate::default();
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
                        observed.borrow_mut().push(info.serial_number.to_string_lossy())
                    })),
                    ..Default::default()
                },
            );
            for i in 0..4 {
                pool.add(&TestDevice::new(
                    &format!("failed-{i}"),
                    Some("A"),
                    None,
                    fail_on(&fail),
                ));
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
            fail.open();
            h.run_current().await;
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
            assert!(h.logs().iter().any(|l| l.level == jcompat::logging::Level::Warn
                && l.message.to_string_lossy() == "device same already present in pool scheduling"));
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
                    .any(|l| l.message.to_string_lossy().ends_with(": noActiveDevices"))
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
