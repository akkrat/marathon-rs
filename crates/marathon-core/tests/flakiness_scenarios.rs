//! Native-only runs of the production run graph with the Stub vendor (no Kotlin counterpart):
//! D71 (unavailable history adds no probability copies) and D72 (`flakinessStrategy: fixed`).
//! The scenario input is tools/oracle/scenarios/success.json, adjusted before the run.
use marathon_config::schema::FlakinessStrategyConfiguration;
use marathon_core::model::TestStatus;
use marathon_logging::Level;
use marathon_test_support::{
    console::Line,
    scenario::{self, Outcome},
};
use serde_json::Value;
use std::collections::HashMap;

/// One test, analytics disabled (the factory's default), and `statuses` as the device's answers.
/// A run takes 5 s per batch, so the scenario's end moves from 20 s to 60 s.
fn run(flakiness: FlakinessStrategyConfiguration, fast: bool, statuses: &[TestStatus]) -> Outcome {
    let (mut factory, mut script) = scenario::load("success.json");
    for (at, event, ..) in &mut script.events {
        if event == "end" {
            *at = 60_000;
        }
    }
    let configuration = &mut factory.configuration.configuration;
    configuration.flakiness_strategy = flakiness;
    configuration.execution_strategy.fast = fast;
    let test = scenario::test("test", "SimpleTest", "test");
    script.devices["serial-1"].set_execution_results(HashMap::from([(test, statuses.to_vec())]));
    let outcome = scenario::drive(factory, script, None, None);
    assert!(outcome.completed, "the run did not complete");
    outcome
}
fn statuses(outcome: &Outcome) -> Vec<String> {
    let raw: Value = serde_json::from_str(&outcome.read("test_result/raw.json")).unwrap();
    raw.as_array()
        .unwrap()
        .iter()
        .map(|row| {
            assert_eq!(row["method"], "test");
            row["status"].as_str().unwrap().to_owned()
        })
        .collect()
}
fn warnings(outcome: &Outcome, prefix: &str) -> Vec<String> {
    outcome
        .lines
        .iter()
        .filter_map(|line| match line {
            Line::Log { level, message, .. } if *level == Level::WARN && message.starts_with(prefix) => {
                Some(message.clone())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn d72_fixed_runs_each_test_count_plus_one_times_with_analytics_disabled() {
    use TestStatus::*;
    let fixed = |count| FlakinessStrategyConfiguration::Fixed { count };
    // A stabilization job: every copy runs and is reported, the failing one included.
    let outcome = run(fixed(2), false, &[Passed, Failure, Passed, Passed]);
    assert_eq!(statuses(&outcome), ["PASSED", "FAILURE", "PASSED"]);
    assert!(matches!(outcome.value, Some(Ok(true))));
    assert!(warnings(&outcome, "Test history unavailable").is_empty());
    let outcome = run(fixed(0), false, &[Passed, Passed]);
    assert_eq!(statuses(&outcome), ["PASSED"]);
    // With `fast` (the default), the first pass completes the test and drops its remaining copies,
    // as it does probability's.
    let outcome = run(fixed(2), true, &[Failure, Passed, Passed, Passed]);
    assert_eq!(statuses(&outcome), ["FAILURE", "PASSED"]);
}

#[test]
fn d71_probability_with_analytics_disabled_runs_each_test_once() {
    use TestStatus::*;
    let outcome = run(
        FlakinessStrategyConfiguration::Probability {
            min_success_rate: 0.8,
            max_count: 3,
            time_limit: jcompat::jtime::parse_instant("2020-01-01T00:00:00Z").unwrap(),
        },
        false,
        &[Passed, Passed, Passed, Passed],
    );
    // C8 gave 3 copies here.
    assert_eq!(statuses(&outcome), ["PASSED"]);
    assert_eq!(
        warnings(&outcome, "Test history unavailable"),
        ["Test history unavailable (analytics is disabled); flakinessStrategy probability adds no copies"]
    );
}
