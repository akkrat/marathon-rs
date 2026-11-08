//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/MetricsProviderImpl.kt
//! (replay of the real-class JVM probes in tools/oracle/metrics-probes).
mod metrics_support;
mod strategy_support;
use jcompat::{
    jnum::{double_to_string, parse_double},
    jtime::parse_instant,
};
use marathon_core::{
    error::OperationError, executor::shared::failure, model::TestStatus, strategy::MetricsKey, wiring::MetricsProvider,
};
use metrics_support::*;
use serde_json::{Value, json};
use std::sync::Arc;
use strategy_support::*;
fn status(value: &str) -> TestStatus {
    match value {
        "PASSED" => TestStatus::Passed,
        "FAILURE" => TestStatus::Failure,
        "IGNORED" => TestStatus::Ignored,
        "INCOMPLETE" => TestStatus::Incomplete,
        "ASSUMPTION_FAILURE" => TestStatus::AssumptionFailure,
        _ => panic!("unknown status"),
    }
}
#[test]
fn provider_sequences_match_jvm_bits_calls_and_ordered_logs() {
    run(async {
        for line in include_str!("../../../fixtures/oracle/metrics/provider.jsonl").lines() {
            let row: Value = serde_json::from_str(line).unwrap();
            let h = Harness::new();
            let mode = row["mode"].as_str().unwrap();
            let source = Arc::new(Source {
                failure: mode == "fail",
                ..Source::default()
            });
            let t = generate_tests(7);
            if mode == "retry" {
                *source.errors.lock().unwrap() = vec![
                    Some(OperationError::Failed(failure(
                        "java.lang.RuntimeException",
                        "unreadable",
                    ))),
                    Some(OperationError::Failed(failure(
                        "java.lang.RuntimeException",
                        "unreadable",
                    ))),
                ]
                .into();
            }
            if mode == "duplicate" {
                *source.times.lock().unwrap() = vec![time_row(&t[0], 100.5), time_row(&t[0], 7000.0)];
                *source.rates.lock().unwrap() = vec![rate_row(&t[0], 0.1), rate_row(&t[0], 0.9)];
            } else if mode == "history" {
                *source.times.lock().unwrap() = vec![time_row(&t[0], 7000.0)];
            }
            let p = h.provider(source.clone(), 300000, row["min"].as_i64().map(|v| v as i32));
            for step in row["steps"].as_array().unwrap() {
                let op = &step["input"];
                let test = &t[op["test"].as_u64().unwrap() as usize];
                let percentile = parse_double(op["p"].as_str().unwrap()).unwrap();
                let limit = parse_instant(op["limit"].as_str().unwrap()).unwrap();
                let value = match op["op"].as_str().unwrap() {
                    "time" => Some(p.execution_time(test, percentile, limit).await.unwrap()),
                    "rate" => Some(p.success_rate(test, limit).await.unwrap()),
                    "observe" => {
                        p.observe_execution_times(&[result(
                            test,
                            status(op["status"].as_str().unwrap()),
                            op["duration"].as_i64().unwrap(),
                        )])
                        .await
                        .unwrap();
                        None
                    }
                    "coverage" => {
                        let tests = op["tests"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|i| t[i.as_u64().unwrap() as usize].clone())
                            .collect::<Vec<_>>();
                        p.report_execution_time_coverage(&tests, percentile, limit, "sorting")
                            .await
                            .unwrap();
                        None
                    }
                    _ => panic!("unknown op"),
                };
                let bits = value.map(|v| format!("{:016x}", if v.is_nan() { f64::NAN.to_bits() } else { v.to_bits() }));
                assert_eq!(json!(bits), step["bits"], "{} {op}", row["name"]);
                let events = h
                    .logs
                    .take()
                    .iter()
                    .map(|e| {
                        let mut message = e.message.to_owned();
                        if e.logger == "Timings" {
                            let start = message.rfind(" in ").unwrap();
                            message.truncate(start);
                            message.push_str(" in <ms> ms");
                        }
                        json!([e.logger, e.level.as_str(), message])
                    })
                    .collect::<Vec<_>>();
                assert_eq!(json!(events), step["logs"], "{} {op}", row["name"]);
                let calls = std::mem::take(&mut *source.calls.lock().unwrap())
                    .iter()
                    .map(|key| match key {
                        MetricsKey::SuccessRate { limit } => json!(["rate", "", limit.to_string()]),
                        MetricsKey::ExecutionTime { percentile, limit } => {
                            json!(["time", double_to_string(*percentile), limit.to_string()])
                        }
                    })
                    .collect::<Vec<_>>();
                assert_eq!(json!(calls), step["calls"], "{} {op}", row["name"]);
            }
        }
    });
}
