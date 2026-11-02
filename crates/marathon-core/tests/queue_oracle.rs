//! Port of tools/oracle/queue-probes/QueueProbes.kt: replays fixtures/oracle/queue/queue.jsonl, the JVM run of the
//! real QueueActor, message by message. Replies, Notify order, batches, comparisons, random draws, bundle and timer
//! calls, observeExecutionTimes arguments, log and progress lines, Track calls, the queue state after each message
//! and the final accumulator states must all match.
#[path = "support/queue.rs"]
mod support;
use marathon_core::{
    execution::{
        FromQueue,
        progress::TestState,
        queue::{QueueMessage, RequestBatch},
    },
    model::*,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use support::*;

fn rows() -> Vec<Value> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/oracle/queue/queue.jsonl");
    let text = std::fs::read_to_string(path).unwrap();
    let mut lines = text.lines();
    let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let rows: Vec<Value> = lines.map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(header["rows"].as_u64(), Some(rows.len() as u64));
    rows
}

/// The probe records the test a draw is for; the Rust hooks only see the draw, whose
/// test follows from the comparison before it.
fn normalized(events: &Value) -> Vec<Value> {
    events
        .as_array()
        .unwrap()
        .iter()
        .map(|event| match event[0].as_str() {
            Some("draw") => json!(["draw", event[2]]),
            _ => event.clone(),
        })
        .collect()
}

fn reply(message: &FromQueue, last: &mut HashMap<String, BatchRef>) -> Value {
    match message {
        FromQueue::Notify { retry_workers } => json!([
            "Notify",
            retry_workers.iter().map(|w| w.to_string_lossy()).collect::<Vec<_>>()
        ]),
        FromQueue::NoBatch(device) => json!(["NoBatch", device.serial_number.to_string_lossy()]),
        FromQueue::Terminated => json!(["Terminated"]),
        FromQueue::ExecuteBatch(device, batch) => {
            let serial = device.serial_number.to_string_lossy();
            last.insert(serial.clone(), batch.clone());
            json!([
                "ExecuteBatch",
                serial,
                batch.id.to_string_lossy(),
                batch.tests.iter().map(t).collect::<Vec<_>>()
            ])
        }
        FromQueue::OutcomeProcessed(device) => json!(["OutcomeProcessed", device.serial_number.to_string_lossy()]),
    }
}

fn status(name: &str) -> TestStatus {
    match name {
        "PASSED" => TestStatus::Passed,
        "FAILURE" => TestStatus::Failure,
        "IGNORED" => TestStatus::Ignored,
        "INCOMPLETE" => TestStatus::Incomplete,
        other => panic!("status {other}"),
    }
}

fn message(
    step: &Value,
    last: &HashMap<String, BatchRef>,
) -> (QueueMessage, Option<tokio::sync::oneshot::Receiver<bool>>) {
    let serial = || step["device"].as_str().unwrap().to_owned();
    let request = |serial: &str, worker: &Value| RequestBatch {
        device: device(serial),
        worker_id: worker.as_str().map(Into::into),
    };
    let message = match step["op"].as_str().unwrap() {
        "request" => QueueMessage::RequestBatch(request(&serial(), &step["worker"])),
        "requests" => QueueMessage::RequestBatches(
            step["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| request(r[0].as_str().unwrap(), &r[1]))
                .collect(),
        ),
        "completed" => {
            let d = device(&serial());
            let make = |test: &Test, s: TestStatus| result(test, &d, "batch", s, 0, 1);
            let results = if let Some(statuses) = step["statuses"].as_array() {
                let all: Vec<TestResult> = last[&serial()]
                    .tests
                    .iter()
                    .zip(statuses)
                    .map(|(test, s)| make(test, status(s.as_str().unwrap())))
                    .collect();
                let of = |wanted: &[TestStatus]| all.iter().filter(|r| wanted.contains(&r.status)).cloned().collect();
                batch_results(
                    of(&[TestStatus::Passed, TestStatus::Ignored]),
                    of(&[TestStatus::Failure]),
                    of(&[TestStatus::Incomplete]),
                )
            } else {
                let list = |key: &str| {
                    step[key]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|r| make(&test_of(&r[0]), status(r[1].as_str().unwrap())))
                        .collect()
                };
                batch_results(list("finished"), list("failed"), list("uncompleted"))
            };
            QueueMessage::Completed(d, results)
        }
        "return" => QueueMessage::ReturnBatch(
            device(&serial()),
            last[&serial()].clone(),
            step["reason"].as_str().unwrap().into(),
        ),
        "reject" => QueueMessage::RejectBatch(device(&serial()), last[step["batchOf"].as_str().unwrap()].clone()),
        "confirm" => QueueMessage::ConfirmOutcome(device(&serial())),
        "isEmpty" => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            return (QueueMessage::IsEmpty(tx), Some(rx));
        }
        _ => QueueMessage::Terminate,
    };
    (message, None)
}

fn state_name(state: TestState) -> &'static str {
    match state {
        TestState::Added { .. } => "Added",
        TestState::Passing { .. } => "Passing",
        TestState::Failing { .. } => "Failing",
        TestState::Failed { .. } => "Failed",
        TestState::Passed { .. } => "Passed",
    }
}

/// Replays the rows whose case starts with one of `prefixes` (all rows for none).
fn replay(prefixes: &'static [&'static str]) {
    let rows: Vec<Value> = rows()
        .into_iter()
        .filter(|row| {
            let case = row["case"].as_str().unwrap();
            prefixes.is_empty() || prefixes.iter().any(|prefix| case.starts_with(prefix))
        })
        .collect();
    assert!(!rows.is_empty());
    let configurations: Vec<_> = rows.iter().map(scenario_configuration).collect();
    run(move |core| {
        Box::pin(async move {
            let mut mismatches = Vec::new();
            for (row, configuration) in rows.iter().zip(configurations) {
                let case = row["case"].as_str().unwrap();
                let mut setup = Setup::new(configuration, tests_of(&row["tests"]));
                setup.flaky = tests_of(&row["flaky"]);
                setup.execution_times = row["executionTimes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| (test_of(&e[0]), e[1].as_f64().unwrap()))
                    .collect();
                setup.draws = row["draws"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|d| d.as_i64().unwrap() as i32)
                    .collect();
                setup.timer = row["timer"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|d| d.as_i64().unwrap())
                    .collect();
                setup.bundle_fails_on = (!row["bundleFailsOn"].is_null()).then(|| test_of(&row["bundleFailsOn"]));
                setup.pool = POOL.into();
                setup.record_comparisons = true;
                let queue = Queue::spawn(&core, setup);
                let construction = queue.trace.drain();
                if construction != normalized(&row["construction"]) {
                    mismatches.push(format!(
                        "{case}: construction\n  rust {construction:?}\n  jvm  {}",
                        row["construction"]
                    ));
                }
                let mut last = HashMap::new();
                for (index, expected) in row["steps"].as_array().unwrap().iter().enumerate() {
                    let step = &expected["step"];
                    let (message, answer) = message(step, &last);
                    let sent = queue.handle.try_send(message);
                    let mut is_empty = Value::Null;
                    if sent {
                        match answer {
                            Some(rx) => {
                                if let Some(answer) = queue.settle_on(rx).await {
                                    is_empty = json!(answer);
                                }
                            }
                            None => {
                                queue.barrier().await;
                            }
                        }
                    }
                    let replies: Vec<Value> = queue.replies().iter().map(|r| reply(r, &mut last)).collect();
                    let events = queue.trace.drain();
                    let snapshot = queue
                        .snapshot
                        .borrow()
                        .clone()
                        .unwrap_or_else(|| panic!("{case}: no state"));
                    let state = json!({
                        "queue": snapshot.queue.iter().map(t).collect::<Vec<_>>(),
                        "retryWorkers": snapshot.retry_workers.iter().map(|(test, workers)| json!([
                            t(test), workers.iter().map(|w| w.to_string_lossy()).collect::<Vec<_>>()
                        ])).collect::<Vec<_>>(),
                        "activeBatches": snapshot.active_batches.iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>(),
                        "closed": marathon_core::runtime::Actor::is_closed_for_send(queue.handle.actor().as_ref()),
                    });
                    let actual = json!({
                        "sent": sent, "replies": replies, "isEmpty": is_empty,
                        "events": events, "state": state,
                    });
                    let wanted = json!({
                        "sent": expected["sent"], "replies": expected["replies"], "isEmpty": expected["isEmpty"],
                        "events": normalized(&expected["events"]), "state": expected["state"],
                    });
                    if actual != wanted {
                        mismatches.push(format!("{case}: step {index} {step}\n  rust {actual}\n  jvm  {wanted}"));
                    }
                }
                let progress: Vec<Value> = queue
                    .states()
                    .await
                    .into_iter()
                    .map(|(name, state)| json!([name, state_name(state), state.counters().0]))
                    .collect();
                if json!(progress) != row["progress"] {
                    mismatches.push(format!(
                        "{case}: progress\n  rust {progress:?}\n  jvm  {}",
                        row["progress"]
                    ));
                }
            }
            assert!(
                mismatches.is_empty(),
                "{} mismatches:\n{}",
                mismatches.len(),
                mismatches.join("\n")
            );
            Ok(())
        })
    });
}

#[test]
fn queue_actor_replays_the_jvm_corpus() {
    replay(&[]);
}

/// C45: the attempt that completes a test under fast ALL_SUCCESS is still queued as a retry and runs again.
#[test]
fn retried_attempt_that_completes_the_test_is_queued_again() {
    replay(&["K1:"]);
}

/// C46: retried tests are matched by Test equality, so a second failed copy of one is never ended.
#[test]
fn second_failed_copy_of_a_retried_test_is_never_ended() {
    replay(&["K2:"]);
}

/// K3 refuted: ClassName takes every copy, so each copy consumes one retry worker.
#[test]
fn copies_of_one_test_in_a_class_name_batch_are_dispatched_per_copy() {
    replay(&["K3:"]);
}

/// C47: a rejected batch queues its tests again although one of them completed on another device meanwhile.
#[test]
fn rejected_batch_queues_a_test_that_completed_meanwhile() {
    replay(&["K4:"]);
}

/// C5 through the real accumulator and queue: the refused retry still adds to the test's total.
#[test]
fn phantom_add_retry_through_the_queue() {
    replay(&["C5:"]);
}

#[test]
fn return_batch_timestamps_wrap_at_long_max_value() {
    replay(&["ReturnBatch at Long.MAX_VALUE"]);
}

#[test]
fn batching_exception_restores_the_copies_and_ends_the_queue() {
    replay(&["a batching exception"]);
}
