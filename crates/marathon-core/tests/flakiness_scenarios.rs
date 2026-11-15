//! Native-only runs of the production run graph with the Stub vendor (no Kotlin counterpart):
//! D71 (unavailable history adds no probability copies) and D72 (`flakinessStrategy: fixed`).
//! The scenario input is tools/oracle/scenarios/success.json, adjusted before the run.
use marathon_config::schema::{ExecutionMode, FlakinessStrategyConfiguration};
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
    run_in(ExecutionMode::AnySuccess, flakiness, fast, statuses)
}
fn run_in(
    mode: ExecutionMode,
    flakiness: FlakinessStrategyConfiguration,
    fast: bool,
    statuses: &[TestStatus],
) -> Outcome {
    let (mut factory, mut script) = scenario::load("success.json");
    for (at, event, ..) in &mut script.events {
        if event == "end" {
            *at = 60_000;
        }
    }
    let configuration = &mut factory.configuration.configuration;
    configuration.flakiness_strategy = flakiness;
    configuration.execution_strategy.fast = fast;
    configuration.execution_strategy.mode = mode;
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

/// The end-of-run block, from its first line.
fn summary(outcome: &Outcome) -> String {
    outcome
        .lines
        .iter()
        .find_map(|line| match line {
            Line::Raw(text) if text.starts_with("Marathon run finished") => Some(text.clone()),
            _ => None,
        })
        .expect("a summary")
}
/// The status of the result tracked as final, as the per-test result file stores it.
fn final_status(outcome: &Outcome) -> String {
    let text = outcome.read("test_result/omni/serial-1/test.SimpleTest#test.json");
    serde_json::from_str::<Value>(&text).unwrap()["status"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn all_success(fast: bool, statuses: &[TestStatus]) -> Outcome {
    run_in(
        ExecutionMode::AllSuccess,
        FlakinessStrategyConfiguration::Fixed { count: 2 },
        fast,
        statuses,
    )
}

/// D72 under ALL_SUCCESS: the test passes only if all count + 1 runs pass. `Some(Ok(false))` is the
/// result the CLI turns into `Marathon execution failed` and exit 1.
#[test]
fn d72_fixed_under_all_success_passes_only_when_every_run_passes() {
    use TestStatus::*;
    for fast in [true, false] {
        let outcome = all_success(fast, &[Passed, Passed, Passed, Passed]);
        assert_eq!(statuses(&outcome), ["PASSED", "PASSED", "PASSED"], "fast {fast}");
        assert!(matches!(outcome.value, Some(Ok(true))), "fast {fast}");
        assert_eq!(final_status(&outcome), "PASSED");
        assert!(
            summary(&outcome)
                .starts_with("Marathon run finished:\nDevice pool omni:\n\t1 passed, 0 failed, 0 ignored tests\n"),
            "{}",
            summary(&outcome)
        );
    }
}

/// With `fast` the first failure ends the test: its remaining copy is dropped, and that failure is
/// the final result, so the summary, the result files and the exit code agree.
#[test]
fn d72_fixed_under_fast_all_success_stops_at_the_first_failure() {
    use TestStatus::*;
    let outcome = all_success(true, &[Passed, Failure, Passed, Passed]);
    assert_eq!(statuses(&outcome), ["PASSED", "FAILURE"]);
    assert!(matches!(outcome.value, Some(Ok(false))));
    assert_eq!(final_status(&outcome), "FAILURE");
    assert!(
        outcome
            .read("tests/omni/marathon_junit_report.xml")
            .contains("<failure")
    );
    let summary = summary(&outcome);
    assert!(
        summary.starts_with(
            "Marathon run finished:\nDevice pool omni:\n\t0 passed, 1 failed, 0 ignored tests\n\tFailed tests:\n\t\ttest.SimpleTest#test\n"
        ),
        "{summary}"
    );
    assert!(
        summary.contains("\tRaw: 1 passed, 1 failed, 0 ignored, 0 incomplete tests\n"),
        "{summary}"
    );
}

/// Without `fast` every run executes and the run fails. C86 (kept, Kotlin's `testEnded`): the run that
/// completes the test, here the last, is the one tracked as final with its own status, so the summary's
/// first line and the per-test result file show it as passed while the exit code says failed. The
/// `Raw:` line and its failed-tests list count every run.
#[test]
fn d72_fixed_under_all_success_without_fast_runs_every_copy_and_fails() {
    use TestStatus::*;
    let outcome = all_success(false, &[Passed, Failure, Passed, Passed]);
    assert_eq!(statuses(&outcome), ["PASSED", "FAILURE", "PASSED"]);
    assert!(matches!(outcome.value, Some(Ok(false))));
    // C86: the verdict is failed, the final tracked result is the last run.
    assert_eq!(final_status(&outcome), "PASSED");
    assert!(
        !outcome
            .read("tests/omni/marathon_junit_report.xml")
            .contains("<failure")
    );
    let summary = summary(&outcome);
    assert!(
        summary.starts_with("Marathon run finished:\nDevice pool omni:\n\t1 passed, 0 failed, 0 ignored tests\n"),
        "{summary}"
    );
    assert!(
        summary.contains(
            "\tRaw: 2 passed, 1 failed, 0 ignored, 0 incomplete tests\n\tFailed tests:\n\t\ttest.SimpleTest#test failed 1 time(s)\n"
        ),
        "{summary}"
    );
    // A failure that is the last run is consistent everywhere.
    let outcome = all_success(false, &[Passed, Passed, Failure, Passed]);
    assert!(matches!(outcome.value, Some(Ok(false))));
    assert_eq!(final_status(&outcome), "FAILURE");
}

/// C86 predates D72: ANY_SUCCESS without `fast` shows the mirror image with probability's copies too.
#[test]
fn c86_any_success_without_fast_tracks_the_last_run_as_final() {
    use TestStatus::*;
    let outcome = run(
        FlakinessStrategyConfiguration::Fixed { count: 2 },
        false,
        &[Failure, Passed, Failure, Passed],
    );
    assert!(matches!(outcome.value, Some(Ok(true))));
    assert_eq!(final_status(&outcome), "FAILURE");
    assert!(summary(&outcome).contains("\t0 passed, 1 failed, 0 ignored tests\n"));
}
