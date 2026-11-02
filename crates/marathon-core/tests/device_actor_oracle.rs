//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/device/DeviceActor.kt,
//! compared with tools/oracle/module DeviceActorProbe (fixtures/oracle/device-actor).
//!
//! Each probe scenario runs on the Rust actor and its row is compared with the JVM
//! row, coroutine exception handler records included (logger and position). The
//! few accepted differences are applied to the expected row, each with its
//! reason, and never silently.
#[path = "support/device_actor.rs"]
mod device_actor;
#[path = "support/device_actor_scenarios.rs"]
mod device_actor_scenarios;
mod support;
use device_actor_scenarios::{SCENARIOS, run_scenario};
use serde_json::Value;

const HANDLER: &str = "CoroutineContext closing due to unrecoverable error";

fn corpus() -> Vec<Value> {
    include_str!("../../../fixtures/oracle/device-actor/device-actor.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Applies the accepted differences to a JVM row.
fn accepted(row: &mut Value) {
    let name = row["scenario"].as_str().unwrap().to_owned();
    let trace = row["trace"].as_array_mut().unwrap();
    match name.as_str() {
        // S1: the data classes' toString holds the whole test list; the batch id stands in.
        "execute at an unclosed terminated actor" => {
            for entry in trace.iter_mut() {
                if entry["log"]["level"] == "ERROR" {
                    entry["log"]["message"] =
                        "Invalid transition from DeviceState.Terminated event DeviceEvent.Execute(batch=<batch>)"
                            .into();
                }
            }
        }
        // The probe wraps StubDevice to count calls, so StubDevice reports itself
        // and not the wrapper; the Kotlin test (no wrapper) sees the same device.
        "batch failure after reported results sends the results only" => {
            for entry in trace.iter_mut() {
                if entry["pool"]["type"] == "CompletedTestBatch" {
                    entry["pool"]["sameDevice"] = true.into();
                }
            }
        }
        // Decision 6, a property of the test vehicle and not of the actor: Kotlin's
        // vendor launches its failing child into the prepare `async`, which absorbs
        // the failure, so no handler logs. The vehicle's test-side child fails the
        // actor job directly, so the actor's handler logs it once, at Kotlin's
        // handler position: before the terminate cascade.
        "actor failing before a dispatched batch starts executing returns the batch"
        | "actor job failure from a child launched in prepare" => {
            let at = trace
                .iter()
                .position(|entry| {
                    let message = entry["log"]["message"].as_str().unwrap_or("");
                    message.starts_with("Returning batch") || message.starts_with("terminate ")
                })
                .unwrap();
            let logger = trace[at]["log"]["logger"].clone();
            trace.insert(
                at,
                serde_json::json!({
                    "t": trace[at]["t"],
                    "log": {
                        "logger": logger,
                        "level": "ERROR",
                        "message": HANDLER,
                        "throwable": [{"class": "java.lang.IllegalStateException", "message": "log streaming crashed"}],
                    },
                }),
            );
        }
        _ => {}
    }
}

#[test]
fn device_actor_corpus_matches_the_actor() {
    let corpus = corpus();
    assert_eq!(
        corpus
            .iter()
            .map(|row| row["scenario"].as_str().unwrap())
            .collect::<Vec<_>>(),
        SCENARIOS,
        "probe scenarios and the Rust list diverge"
    );
    let rows = corpus.len();
    let mut failures = vec![];
    for mut expected in corpus {
        let name = SCENARIOS
            .iter()
            .copied()
            .find(|name| expected["scenario"] == **name)
            .unwrap();
        accepted(&mut expected);
        let actual = run_scenario(name).row(name);
        if actual != expected {
            failures.push(format!(
                "{name}\n  expected {}\n  actual   {}",
                serde_json::to_string(&expected).unwrap(),
                serde_json::to_string(&actual).unwrap()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {rows} rows differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Vendor work runs on worker threads, so drains race vendor completions; every
/// scenario must give the same row on each of 50 runs.
#[test]
fn device_actor_rows_are_stable_over_repeated_runs() {
    const RUNS: usize = 50;
    let mut compared = 0;
    for name in SCENARIOS {
        let first = run_scenario(name).row(name);
        for _ in 1..RUNS {
            assert_eq!(run_scenario(name).row(name), first, "{name}");
            compared += 1;
        }
    }
    println!(
        "{} scenarios x {RUNS} runs, {compared} repeated rows equal",
        SCENARIOS.len()
    );
}

/// Review round 1 (R1-sol-1): the vendor starts on another runtime after the
/// terminate task has cancelled the execution, and must still see an active
/// job at entry as the inline Kotlin call does. Raced 2,000 times.
#[test]
fn entry_cancellation_matches_the_probe_on_every_run() {
    const RUNS: usize = 2000;
    let mut expected = corpus()
        .into_iter()
        .find(|row| row["scenario"] == "entry cancellation")
        .unwrap();
    accepted(&mut expected);
    let divergent: Vec<Value> = (0..RUNS)
        .map(|_| run_scenario("entry cancellation").row("entry cancellation"))
        .filter(|row| *row != expected)
        .collect();
    assert!(
        divergent.is_empty(),
        "{} of {RUNS} runs diverge from the probe row\n  expected {}\n  first    {}",
        divergent.len(),
        serde_json::to_string(&expected).unwrap(),
        serde_json::to_string(&divergent[0]).unwrap()
    );
}
