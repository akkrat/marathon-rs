//! Port of tools/oracle/queue-probes/QueueProbes.kt: replays fixtures/oracle/queue/queue.jsonl, the JVM run of the
//! real QueueActor, message by message. D60 gives the recorded inputs explicit native key,
//! batch and queue-order expectations; accounting, replies, logs and final states remain exact.
//! D69 supersedes the three recordings of C45–C47; `queue_behaviour` pins their native contract.
//! D77 moves the final flag of tracked runs in the `fast: false` recordings (`d77_finals`).
#[path = "support/d77.rs"]
mod d77;
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
    let text = include_str!("../../../fixtures/oracle/queue/queue.jsonl");
    let mut lines = text.lines();
    let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let rows: Vec<Value> = lines.map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(header["rows"].as_u64(), Some(rows.len() as u64));
    rows
}

/// The old comparator/lookups are replaced by explicit insertion events below; every other event stays exact.
fn non_key_events(events: &Value) -> Vec<Value> {
    events
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| !matches!(event[0].as_str(), Some("compare" | "executionTime" | "draw")))
        .cloned()
        .collect()
}

fn reply(message: &FromQueue, last: &mut HashMap<String, BatchRef>) -> Value {
    match message {
        FromQueue::Notify { retry_workers } => {
            json!(["Notify", retry_workers.iter().map(|w| w.to_owned()).collect::<Vec<_>>()])
        }
        FromQueue::NoBatch(device) => json!(["NoBatch", device.serial_number.to_owned()]),
        FromQueue::Terminated => json!(["Terminated"]),
        FromQueue::ExecuteBatch(device, batch) => {
            let serial = device.serial_number.to_owned();
            last.insert(serial.clone(), batch.clone());
            json!([
                "ExecuteBatch",
                serial,
                batch.id.to_owned(),
                batch.tests.iter().map(t).collect::<Vec<_>>()
            ])
        }
        FromQueue::OutcomeProcessed(device) => json!(["OutcomeProcessed", device.serial_number.to_owned()]),
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

const D50_ROW: &str = "execution-time comparisons with ConfirmOutcome and IsEmpty";

// D60 variants of the immutable JVM inputs. Insertions are [non-key event offset, distinct test index].
// Only two full protocol scenarios change identities/accounting: insertion-ordered random draws and D50's
// previously truncated size-3 scenario, which now runs all eleven steps. Other patches only reorder snapshots.
const NATIVE_VARIANTS: &str = r#"[
  {
    "case": "placement: a retry from another worker is left to its own worker only if nothing else is queued",
    "insertions": [[],[],[],[[4,0]],[[4,0]],[[5,1]],[],[[2,0]],[],[],[],[]],
    "patches": [
    ]
  },
  {
    "case": "placement: devices that ask together leave a retry to a device of another worker",
    "insertions": [[],[],[[5,0]],[[5,1]],[[2,1]],[],[]],
    "patches": [
    ]
  },
  {
    "case": "placement: a rejected retry keeps its worker",
    "insertions": [[],[[5,0]],[],[[1,0]],[[2,0]],[],[],[]],
    "patches": [
    ]
  },
  {
    "case": "placement: a device gets a retry from its own worker when nothing else is queued",
    "insertions": [[],[[5,0]],[],[]],
    "patches": [
    ]
  },
  {
    "case": "K1: a retried attempt that completes the test still queues it again",
    "insertions": [[],[[5,0]],[],[[2,0]],[],[],[]],
    "patches": [
    ]
  },
  {
    "case": "K2: a second failed copy of a retried test is never ended",
    "insertions": [[],[[7,0]],[],[]],
    "patches": [
    ]
  },
  {
    "case": "K3: two retry copies of one test in a class-name batch",
    "insertions": [[],[[4,0],[6,0]],[],[[1,0],[1,0]],[],[]],
    "patches": [
    ]
  },
  {
    "case": "K3: copies of a retried test beside an original copy",
    "insertions": [[],[[6,0],[8,1]],[[2,0],[2,1]],[],[[1,0],[1,1]],[],[],[]],
    "patches": [
      ["/steps/1/state/queue",[["pkg","A","m1"],["pkg","A","m2"],["pkg","C0","m0"]]]
    ]
  },
  {
    "case": "K4: a rejected batch queues a test that completed meanwhile",
    "insertions": [[],[],[],[[1,0]],[],[]],
    "patches": [
    ]
  },
  {
    "case": "reject of a batch that is no longer active",
    "insertions": [[],[],[],[],[],[],[],[]],
    "patches": [
    ]
  },
  {
    "case": "ReturnBatch at Long.MAX_VALUE",
    "insertions": [[],[[5,0],[7,1]],[],[]],
    "patches": [
    ]
  },
  {
    "case": "uncompleted quota exceeded with duplicate copies",
    "insertions": [[],[[4,0],[6,0]],[],[]],
    "patches": [
    ]
  },
  {
    "case": "class-name batching with flaky copies and mixed results",
    "insertions": [[],[],[[7,0],[9,1]],[[6,2]],[[2,2]],[],[],[],[]],
    "patches": [
    ]
  },
  {
    "case": "RequestBatches when every request could only get its own retries",
    "insertions": [[],[[5,0]],[],[]],
    "patches": [
    ]
  },
  {
    "case": "RequestBatches stops once the queue terminates",
    "insertions": [[],[]],
    "patches": [
    ]
  },
  {
    "case": "a batching exception restores the copies set aside",
    "insertions": [[],[[5,0]],[[3,0]],[]],
    "patches": [
      ["/steps/1/state/queue",[["pkg","C0","m0"],["pkg","C1","m1"],["pkg","C2","m2"]]]
    ]
  },
  {
    "case": "C5: a phantom AddRetry through the queue",
    "insertions": [[],[[5,0]],[],[]],
    "patches": [
    ]
  },
  {
    "case": "random-order comparisons",
    "insertions": [[],[],[[7,1]],[[1,0],[1,4]],[[2,1]],[[4,0],[7,4]],[],[],[],[],[],[],[],[]],
    "patches": [
      ["/steps/0/replies",[["ExecuteBatch","serial-A1","batch-1",[["pkg","C1","m1"],["pkg","C3","m3"]]]]],
      ["/steps/0/state/queue",[["pkg","C0","m0"],["pkg","C4","m4"],["pkg","C2","m2"]]],
      ["/steps/1/replies",[["ExecuteBatch","serial-B1","batch-2",[["pkg","C0","m0"],["pkg","C4","m4"]]]]],
      ["/steps/2/events",[["log","DEBUG","handle test results serial-A1",null],["observe",[[["pkg","C3","m3"],"PASSED"],[["pkg","C1","m1"],"FAILURE"]]],["stdout","00% | [pool]-[serial-A1] pkg.C3#m3 passed"],["track","pool","serial-A1",["pkg","C3","m3"],"PASSED",0,1,"batch",true],["log","DEBUG","handle failed tests serial-A1",null],["stdout","17% | [pool]-[serial-A1] pkg.C1#m1 failed"],["track","pool","serial-A1",["pkg","C1","m1"],"FAILURE",0,1,"batch",false]]],
      ["/steps/2/state/queue",[["pkg","C1","m1"],["pkg","C2","m2"]]],
      ["/steps/2/state/retryWorkers",[[["pkg","C1","m1"],["worker-A"]]]],
      ["/steps/3/state/queue",[["pkg","C1","m1"],["pkg","C0","m0"],["pkg","C4","m4"],["pkg","C2","m2"]]],
      ["/steps/3/state/retryWorkers",[[["pkg","C1","m1"],["worker-A"]]]],
      ["/steps/4/replies",[["ExecuteBatch","serial-A2","batch-3",[["pkg","C0","m0"],["pkg","C4","m4"]]]]],
      ["/steps/4/state/queue",[["pkg","C1","m1"],["pkg","C2","m2"]]],
      ["/steps/4/state/retryWorkers",[[["pkg","C1","m1"],["worker-A"]]]],
      ["/steps/5/events",[["log","DEBUG","handle test results serial-A2",null],["observe",[[["pkg","C4","m4"],"FAILURE"]]],["stdout","33% | [pool]-[serial-A2] pkg.C0#m0 incomplete"],["track","pool","serial-A2",["pkg","C0","m0"],"INCOMPLETE",0,1,"batch",false],["log","DEBUG","handle failed tests serial-A2",null],["stdout","29% | [pool]-[serial-A2] pkg.C4#m4 failed"],["track","pool","serial-A2",["pkg","C4","m4"],"FAILURE",0,1,"batch",false]]],
      ["/steps/5/state/queue",[["pkg","C1","m1"],["pkg","C0","m0"],["pkg","C4","m4"],["pkg","C2","m2"]]],
      ["/steps/5/state/retryWorkers",[[["pkg","C1","m1"],["worker-A"]],[["pkg","C0","m0"],["worker-A"]],[["pkg","C4","m4"],["worker-A"]]]],
      ["/steps/6/replies",[["ExecuteBatch","serial-B2","batch-4",[["pkg","C1","m1"],["pkg","C0","m0"]]]]],
      ["/steps/8/events",[["log","DEBUG","handle test results serial-B2",null],["observe",[[["pkg","C1","m1"],"PASSED"],[["pkg","C0","m0"],"PASSED"]]],["stdout","43% | [pool]-[serial-B2] pkg.C1#m1 passed"],["track","pool","serial-B2",["pkg","C1","m1"],"PASSED",0,1,"batch",true],["stdout","57% | [pool]-[serial-B2] pkg.C0#m0 passed"],["track","pool","serial-B2",["pkg","C0","m0"],"PASSED",0,1,"batch",true]]],
      ["/progress",[["pkg.C0#m0","Passed",1],["pkg.C1#m1","Passed",2],["pkg.C2#m2","Passed",1],["pkg.C3#m3","Passed",1],["pkg.C4#m4","Passed",2]]]
    ]
  },
  {
    "case": "execution-time comparisons with ConfirmOutcome and IsEmpty",
    "insertions": [[[2,1]],[],[[6,2],[9,1]],[],[[2,1],[2,2]],[[5,1],[7,0]],[[2,1]],[],[],[],[]],
    "patches": [
      ["/steps/0/replies",[["ExecuteBatch","serial-A1","batch-1",[["pkg","C1","m1"],["pkg","C2","m2"],["pkg","C3","m3"]]]]],
      ["/steps/0/state/queue",[["pkg","C1","m1"],["pkg","C0","m0"]]],
      ["/steps/1/state/queue",[["pkg","C1","m1"],["pkg","C0","m0"]]],
      ["/steps/2/events",[["log","DEBUG","handle test results serial-A1",null],["observe",[[["pkg","C3","m3"],"PASSED"],[["pkg","C1","m1"],"FAILURE"]]],["stdout","00% | [pool]-[serial-A1] pkg.C3#m3 passed"],["track","pool","serial-A1",["pkg","C3","m3"],"PASSED",0,1,"batch",true],["stdout","20% | [pool]-[serial-A1] pkg.C2#m2 incomplete"],["track","pool","serial-A1",["pkg","C2","m2"],"INCOMPLETE",0,1,"batch",false],["log","DEBUG","handle failed tests serial-A1",null],["stdout","17% | [pool]-[serial-A1] pkg.C1#m1 failed"],["track","pool","serial-A1",["pkg","C1","m1"],"FAILURE",0,1,"batch",false]]],
      ["/steps/2/state/queue",[["pkg","C1","m1"],["pkg","C2","m2"],["pkg","C1","m1"],["pkg","C0","m0"]]],
      ["/steps/3/state/queue",[["pkg","C1","m1"],["pkg","C2","m2"],["pkg","C1","m1"],["pkg","C0","m0"]]],
      ["/steps/4/replies",[["ExecuteBatch","serial-A1","batch-2",[["pkg","C1","m1"],["pkg","C0","m0"]]]]],
      ["/steps/4/state/queue",[["pkg","C1","m1"],["pkg","C2","m2"]]],
      ["/steps/5/events",[["log","DEBUG","onReturnBatch serial-A1. reason=lost",null],["time",100],["time",101],["stdout","33% | [pool]-[serial-A1] pkg.C1#m1 incomplete"],["track","pool","serial-A1",["pkg","C1","m1"],"INCOMPLETE",100,101,"batch-2",false],["stdout","33% | [pool]-[serial-A1] pkg.C0#m0 incomplete"],["track","pool","serial-A1",["pkg","C0","m0"],"INCOMPLETE",101,102,"batch-2",false]]],
      ["/steps/5/state/queue",[["pkg","C1","m1"],["pkg","C2","m2"],["pkg","C1","m1"],["pkg","C0","m0"]]],
      ["/steps/5/state/retryWorkers",[[["pkg","C2","m2"],["worker-A"]],[["pkg","C1","m1"],["worker-A","worker-A"]],[["pkg","C0","m0"],["worker-A"]]]],
      ["/steps/6/replies",[["ExecuteBatch","serial-B1","batch-3",[["pkg","C1","m1"],["pkg","C2","m2"],["pkg","C0","m0"]]]]],
      ["/steps/6/state/queue",[["pkg","C1","m1"]]],
      ["/steps/6/state/retryWorkers",[[["pkg","C1","m1"],["worker-A"]]]],
      ["/steps/7/events",[["log","DEBUG","handle test results serial-B1",null],["observe",[[["pkg","C1","m1"],"PASSED"],[["pkg","C2","m2"],"PASSED"],[["pkg","C0","m0"],"PASSED"]]],["stdout","33% | [pool]-[serial-B1] pkg.C1#m1 passed"],["track","pool","serial-B1",["pkg","C1","m1"],"PASSED",0,1,"batch",false],["stdout","50% | [pool]-[serial-B1] pkg.C2#m2 passed"],["track","pool","serial-B1",["pkg","C2","m2"],"PASSED",0,1,"batch",true],["stdout","67% | [pool]-[serial-B1] pkg.C0#m0 passed"],["track","pool","serial-B1",["pkg","C0","m0"],"PASSED",0,1,"batch",true]]],
      ["/steps/7/state/queue",[["pkg","C1","m1"]]],
      ["/steps/7/state/retryWorkers",[[["pkg","C1","m1"],["worker-A"]]]],
      ["/steps/8/replies",[["ExecuteBatch","serial-B1","batch-4",[["pkg","C1","m1"]]]]],
      ["/steps/9/events",[["log","DEBUG","handle test results serial-B1",null],["observe",[[["pkg","C1","m1"],"PASSED"]]],["stdout","83% | [pool]-[serial-B1] pkg.C1#m1 passed"],["track","pool","serial-B1",["pkg","C1","m1"],"PASSED",0,1,"batch",true]]],
      ["/progress",[["pkg.C0#m0","Passed",1],["pkg.C1#m1","Passed",3],["pkg.C2#m2","Passed",1],["pkg.C3#m3","Passed",1]]]
    ]
  }
]"#;

fn native_expected(row: Value) -> Value {
    let mut row = native_order(row);
    d77_finals(&mut row);
    row
}

fn native_order(mut row: Value) -> Value {
    // x56 changes exactly the handler's diagnostic name/message, not queue data.
    for step in row["steps"].as_array_mut().unwrap() {
        for event in step["events"].as_array_mut().unwrap() {
            if *event
                == json!([
                    "handler",
                    "ERROR",
                    "QueueActor[DevicePoolId(name=pool)]",
                    "CoroutineContext closing due to unrecoverable error"
                ])
            {
                *event = json!([
                    "handler",
                    "ERROR",
                    "QueueActor[pool]",
                    "Task stopped due to unrecoverable error"
                ]);
            }
        }
    }

    let variants: Value = serde_json::from_str(NATIVE_VARIANTS).unwrap();
    let variant = variants
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["case"] == row["case"])
        .unwrap();
    let mut tests = Vec::new();
    for test in row["tests"]
        .as_array()
        .unwrap()
        .iter()
        .chain(row["flaky"].as_array().unwrap())
    {
        if !tests.contains(test) {
            tests.push(test.clone());
        }
    }
    for step in row["steps"].as_array_mut().unwrap() {
        step["events"] = json!(non_key_events(&step["events"]));
    }
    for patch in variant["patches"].as_array().unwrap() {
        *row.pointer_mut(patch[0].as_str().unwrap()).unwrap() = patch[1].clone();
    }
    let random = row["sorting"] == "random-order";
    let draws = row["draws"].as_array().unwrap().clone();
    let mut drawn = Vec::new();
    let mut key_events = |test: &Value| -> Vec<Value> {
        let mut events = vec![json!(["key", test])];
        if random && !drawn.contains(test) {
            events.push(json!(["draw", draws[drawn.len()]]));
            drawn.push(test.clone());
        }
        events
    };
    row["construction"] = json!(
        row["tests"]
            .as_array()
            .unwrap()
            .iter()
            .chain(row["flaky"].as_array().unwrap())
            .flat_map(&mut key_events)
            .collect::<Vec<_>>()
    );
    let plans = variant["insertions"].as_array().unwrap();
    assert_eq!(row["steps"].as_array().unwrap().len(), plans.len());
    for (step, plan) in row["steps"].as_array_mut().unwrap().iter_mut().zip(plans) {
        let base = step["events"].as_array().unwrap();
        let mut events = Vec::new();
        let mut inserted = 0;
        for offset in 0..=base.len() {
            for insertion in plan
                .as_array()
                .unwrap()
                .iter()
                .filter(|p| p[0].as_u64() == Some(offset as u64))
            {
                events.extend(key_events(&tests[insertion[1].as_u64().unwrap() as usize]));
                inserted += 1;
            }
            if let Some(event) = base.get(offset) {
                events.push(event.clone());
            }
        }
        assert_eq!(inserted, plan.as_array().unwrap().len());
        step["events"] = json!(events);
    }
    row
}

/// D77: with `fast: false` the JVM tracks the run that completes a test as final (C87); the native
/// accounting tracks the run `fast: true` would complete it on. Only the final flag of `track` events moves.
fn d77_finals(row: &mut Value) {
    if row["fast"] == true {
        return;
    }
    let mut d77 = d77::D77::new(row["mode"] != "ALL_SUCCESS");
    for step in row["steps"].as_array_mut().unwrap() {
        for event in step["events"].as_array_mut().unwrap() {
            if event[0] == "track" {
                let flag = d77.final_flag(
                    event[3].to_string(),
                    event[4].as_str().unwrap(),
                    event[8].as_bool().unwrap(),
                );
                event[8] = json!(flag);
            }
        }
    }
}

/// D69: the recordings of C45 (K1), C46 (K2) and C47 (K4), which the native queue no longer reproduces. They stay in
/// the fixture unedited and are left out of the replay; `d69_supersedes_exactly_the_c45_to_c47_recordings` checks
/// that they differ exactly where D69 says.
const SUPERSEDED_D69: [&str; 3] = ["K1:", "K2:", "K4:"];
fn superseded(case: &str) -> bool {
    SUPERSEDED_D69.iter().any(|prefix| case.starts_with(prefix))
}

/// Replays the rows whose case starts with one of `prefixes` (all rows for none), except those D69 supersedes.
fn replay(prefixes: &'static [&'static str]) {
    let rows: Vec<Value> = rows()
        .into_iter()
        .filter(|row| {
            let case = row["case"].as_str().unwrap();
            !superseded(case) && (prefixes.is_empty() || prefixes.iter().any(|prefix| case.starts_with(prefix)))
        })
        .map(native_expected)
        .collect();
    let mismatches = mismatches_of(rows);
    assert!(
        mismatches.is_empty(),
        "{} mismatches:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// What differs from the recording in `rows`, in order.
fn mismatches_of(rows: Vec<Value>) -> Vec<String> {
    assert!(!rows.is_empty());
    let found = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let out = found.clone();
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
                setup.record_keys = true;
                let queue = Queue::spawn(&core, setup);
                let construction = queue.trace.drain();
                if construction != *row["construction"].as_array().unwrap() {
                    mismatches.push(format!(
                        "{case}: construction\n  rust {construction:?}\n  expected  {}",
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
                            t(test), workers.iter().map(|w| w.to_owned()).collect::<Vec<_>>()
                        ])).collect::<Vec<_>>(),
                        "activeBatches": snapshot.active_batches.iter().map(|s| s.to_owned()).collect::<Vec<_>>(),
                        "closed": marathon_core::runtime::Actor::is_closed_for_send(queue.handle.actor().as_ref()),
                    });
                    let actual = json!({
                        "sent": sent, "replies": replies, "isEmpty": is_empty,
                        "events": events, "state": state,
                    });
                    let wanted = json!({
                        "sent": expected["sent"], "replies": expected["replies"], "isEmpty": expected["isEmpty"],
                        "events": expected["events"], "state": expected["state"],
                    });
                    if actual != wanted {
                        mismatches.push(format!(
                            "{case}: step {index} {step}\n  rust {actual}\n  expected  {wanted}"
                        ));
                    }
                }
                let progress: Vec<Value> = queue
                    .states()
                    .await
                    .into_iter()
                    .map(|(name, state)| json!([name, state_name(state), state.counters().0]))
                    .collect();
                if !row["progress"].is_null() && json!(progress) != row["progress"] {
                    mismatches.push(format!(
                        "{case}: progress\n  rust {progress:?}\n  expected  {}",
                        row["progress"]
                    ));
                }
            }
            *out.lock().unwrap() = mismatches;
            Ok(())
        })
    });
    found.lock().unwrap().clone()
}

#[test]
fn queue_actor_replays_recorded_inputs_with_native_order_d60() {
    replay(&[]);
}

fn recorded_row(case: &str) -> Value {
    rows().into_iter().find(|row| row["case"] == case).unwrap()
}

#[test]
fn d50_fills_the_slot_kotlin_gave_to_a_flaky_copy() {
    let row = recorded_row(D50_ROW);
    assert_eq!(
        row["steps"][0]["replies"][0][3],
        json!([["pkg", "C1", "m1"], ["pkg", "C2", "m2"]])
    );
    let native = native_expected(row);
    assert_eq!(
        native["steps"][0]["replies"][0][3],
        json!([["pkg", "C1", "m1"], ["pkg", "C2", "m2"], ["pkg", "C3", "m3"]])
    );
    assert_eq!(native["steps"].as_array().unwrap().len(), 11);
    assert!(mismatches_of(vec![native]).is_empty());
}

/// D77 moves the final flag of these tracked runs, all in `fast: false` (ANY_SUCCESS) recordings: the first pass
/// of a test with several runs becomes final, and the later run that completes the test does not. A flag moved
/// back to the JVM's is a mismatch.
#[test]
fn d77_moves_only_final_flags_of_non_fast_recordings() {
    let mut moved = Vec::new();
    for row in rows() {
        let case = row["case"].as_str().unwrap().to_owned();
        let fast = row["fast"] == true;
        let before = native_order(row);
        let mut after = before.clone();
        d77_finals(&mut after);
        for (index, (old, new)) in before["steps"]
            .as_array()
            .unwrap()
            .iter()
            .zip(after["steps"].as_array().unwrap())
            .enumerate()
        {
            for (old, new) in old["events"]
                .as_array()
                .unwrap()
                .iter()
                .zip(new["events"].as_array().unwrap())
            {
                if old != new {
                    assert!(!fast, "{case}");
                    let mut old = old.clone();
                    old[8] = new[8].clone();
                    assert_eq!(&old, new);
                    let case = case.split(':').next().unwrap();
                    let test = new[3][1].as_str().unwrap().to_owned() + "#" + new[3][2].as_str().unwrap();
                    moved.push(format!("{case} step {} {test} {}", index + 1, new[8]));
                }
            }
        }
    }
    assert_eq!(
        moved,
        [
            "placement step 10 Placement#slow true",
            "placement step 12 Placement#slow false",
            // C46's recording (superseded by D69): the JVM never completes the test, D77 tracks its pass.
            "K2 step 4 A#m1 true",
            "K3 step 6 A#m1 true",
            "K3 step 6 A#m1 false",
            "K3 step 2 A#m1 true",
            "K3 step 8 A#m1 false",
            "class-name batching with flaky copies and mixed results step 3 A#m1 true",
            "class-name batching with flaky copies and mixed results step 4 C0#m0 true",
            "class-name batching with flaky copies and mixed results step 7 A#m1 false",
            "class-name batching with flaky copies and mixed results step 8 C0#m0 false",
            "execution-time comparisons with ConfirmOutcome and IsEmpty step 8 C1#m1 true",
            "execution-time comparisons with ConfirmOutcome and IsEmpty step 10 C1#m1 false",
        ]
    );
    // A flag left as the JVM recorded it is a mismatch.
    let case = rows()
        .into_iter()
        .find(|row| row["case"].as_str().unwrap().starts_with("execution-time comparisons"))
        .unwrap();
    let jvm = native_order(case);
    assert_eq!(mismatches_of(vec![jvm.clone()]).len(), 2);
    let mut native = jvm;
    d77_finals(&mut native);
    assert!(mismatches_of(vec![native]).is_empty());
}

#[test]
fn native_replay_rejects_queue_and_accounting_mutations() {
    let native = native_expected(recorded_row("C5: a phantom AddRetry through the queue"));
    let mut tampered = native.clone();
    tampered["progress"][0][1] = json!("tampered");
    assert_eq!(mismatches_of(vec![tampered]).len(), 1);
    let mut tampered = native;
    tampered["steps"][0]["state"]["queue"] = json!([["tampered", "tampered", "tampered"]]);
    assert_eq!(mismatches_of(vec![tampered]).len(), 1);
}

/// D69 narrows the replay by exactly three recordings, and each still differs from the native queue only from the
/// message D69 changes on: K1 (C45) from the failure whose retry is no longer queued, K2 (C46) at the batch whose
/// second failed copy is now ended and at the pass that now completes the test, K4 (C47) from the rejection that no
/// longer queues the completed test. The native expectations are `queue_behaviour`'s `_d69` tests.
#[test]
fn d69_supersedes_exactly_the_c45_to_c47_recordings() {
    let rows: Vec<Value> = rows()
        .into_iter()
        .filter(|row| superseded(row["case"].as_str().unwrap()))
        .collect();
    assert_eq!(rows.len(), SUPERSEDED_D69.len());
    let differing: Vec<String> = mismatches_of(rows.into_iter().map(native_expected).collect())
        .iter()
        .map(|mismatch| {
            let line = mismatch.lines().next().unwrap();
            let what = match line.find(": step ") {
                Some(at) => line[at + 2..].split(' ').take(2).collect::<Vec<_>>().join(" "),
                None => line.rsplit(": ").next().unwrap().to_owned(),
            };
            format!("{} {what}", &line[..2])
        })
        .collect();
    assert_eq!(
        differing,
        [
            "K1 step 1",
            "K1 step 2",
            "K1 step 3",
            "K1 step 4",
            "K1 step 5",
            "K1 step 6",
            "K1 progress",
            "K2 step 1",
            "K2 step 3",
            "K4 step 3",
            "K4 step 4",
            "K4 step 5",
            "K4 progress",
        ]
    );
}

/// K3 refuted: ClassName takes every copy, so each copy consumes one retry worker.
#[test]
fn copies_of_one_test_in_a_class_name_batch_are_dispatched_per_copy() {
    replay(&["K3:"]);
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
