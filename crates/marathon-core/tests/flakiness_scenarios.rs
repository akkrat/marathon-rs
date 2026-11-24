//! Native-only runs of the production run graph with the Stub vendor (no Kotlin counterpart):
//! D71 (unavailable history adds no probability copies), D72 (`flakinessStrategy: fixed`) and D83 (copies
//! of different tests take turns in the queue), D99 (a failure that meets no total retry quota left ends its test).
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
    run_with(
        |configuration| {
            configuration.flakiness_strategy = flakiness;
            configuration.execution_strategy.fast = fast;
            configuration.execution_strategy.mode = mode;
        },
        statuses,
    )
}
fn run_with(configure: impl FnOnce(&mut marathon_config::schema::Configuration), statuses: &[TestStatus]) -> Outcome {
    let (mut factory, mut script) = scenario::load("success.json");
    for (at, event, ..) in &mut script.events {
        if event == "end" {
            *at = 60_000;
        }
    }
    configure(&mut factory.configuration.configuration);
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

/// Without `fast` every run executes and the run fails. D77 (was C87): the first failure, the run `fast`
/// would have ended the test on, is the one tracked as final, so the summary's first line, the per-test
/// result file and JUnit say failed, as the exit code does. The `Raw:` line and its failed-tests list
/// count every run.
#[test]
fn d72_fixed_under_all_success_without_fast_runs_every_copy_and_fails() {
    use TestStatus::*;
    let outcome = all_success(false, &[Passed, Failure, Passed, Passed]);
    assert_eq!(statuses(&outcome), ["PASSED", "FAILURE", "PASSED"]);
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

/// D77 under ANY_SUCCESS without `fast` (C87 predates D72 and showed with probability's copies too): the
/// first pass is the final result of a test that passes, whatever the runs after it, so the summary's
/// first line, the per-test result file and JUnit say passed, as the exit code does.
#[test]
fn d77_any_success_without_fast_tracks_the_first_pass_as_final() {
    use TestStatus::*;
    let outcome = run(
        FlakinessStrategyConfiguration::Fixed { count: 2 },
        false,
        &[Failure, Passed, Failure, Passed],
    );
    assert_eq!(statuses(&outcome), ["FAILURE", "PASSED", "FAILURE"]);
    assert!(matches!(outcome.value, Some(Ok(true))));
    assert_eq!(final_status(&outcome), "PASSED");
    assert!(
        !outcome
            .read("tests/omni/marathon_junit_report.xml")
            .contains("<failure")
    );
    let text = summary(&outcome);
    assert!(
        text.starts_with("Marathon run finished:\nDevice pool omni:\n\t1 passed, 0 failed, 0 ignored tests\n"),
        "{text}"
    );
    assert!(
        text.contains(
            "\tRaw: 1 passed, 2 failed, 0 ignored, 0 incomplete tests\n\tFailed tests:\n\t\ttest.SimpleTest#test failed 2 time(s)\n"
        ),
        "{text}"
    );
    // A test whose every run fails keeps its last run as final, as with `fast`.
    let outcome = run(
        FlakinessStrategyConfiguration::Fixed { count: 2 },
        false,
        &[Failure, Failure, Failure, Passed],
    );
    assert!(matches!(outcome.value, Some(Ok(false))));
    assert_eq!(final_status(&outcome), "FAILURE");
    assert!(summary(&outcome).contains("\t0 passed, 1 failed, 0 ignored tests\n"));
}

/// D77 takes the final record from what `fast: true` tracks, which is unchanged: there the run that
/// decides the verdict ends the test. For the same answers both settings write the same per-test result
/// file and summary first line; only the number of runs differs.
#[test]
fn d77_fast_and_not_fast_track_the_same_final_record() {
    use TestStatus::*;
    let fixed = FlakinessStrategyConfiguration::Fixed { count: 2 };
    for (mode, answers, runs) in [
        (ExecutionMode::AnySuccess, [Failure, Passed, Failure, Passed], 2),
        (ExecutionMode::AllSuccess, [Passed, Failure, Passed, Passed], 2),
        (ExecutionMode::AnySuccess, [Failure, Failure, Failure, Passed], 3),
        (ExecutionMode::AllSuccess, [Passed, Passed, Passed, Passed], 3),
    ] {
        let fast = run_in(mode, fixed.clone(), true, &answers);
        let slow = run_in(mode, fixed.clone(), false, &answers);
        assert_eq!(statuses(&fast).len(), runs, "{mode:?} {answers:?}");
        assert_eq!(statuses(&slow).len(), 3, "{mode:?} {answers:?}");
        // Batch ids are random per run; which run is final is pinned in `pool_progress`.
        let record = |outcome: &Outcome| {
            let mut record: Value =
                serde_json::from_str(&outcome.read("test_result/omni/serial-1/test.SimpleTest#test.json")).unwrap();
            record["testBatchId"] = Value::Null;
            record
        };
        assert_eq!(record(&fast), record(&slow), "{mode:?} {answers:?}");
        // The first block up to the flakiness overhead, which counts the runs and so differs by design; the failed-tests
        // list is part of it.
        let first = |outcome: &Outcome| {
            summary(outcome)
                .lines()
                .take_while(|line| !line.contains("Flakiness overhead") && !line.trim_start().starts_with("Raw:"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(first(&fast), first(&slow), "{mode:?} {answers:?}");
        if matches!(mode, ExecutionMode::AllSuccess) && answers[..3].contains(&Failure) {
            assert!(
                first(&slow).contains("SimpleTest"),
                "failed-tests list compared: {}",
                first(&slow)
            );
        }
        assert_eq!(
            format!("{:?}", fast.value),
            format!("{:?}", slow.value),
            "{mode:?} {answers:?}"
        );
    }
}

fn errors(outcome: &Outcome) -> Vec<String> {
    outcome
        .lines
        .iter()
        .filter_map(|line| match line {
            Line::Log { level, message, .. } if *level == Level::ERROR => Some(message.clone()),
            _ => None,
        })
        .collect()
}

/// D99 (C5): with fixed-quota retries, the failure that meets 0 total retries left ends the test as failed. Kotlin
/// added a phantom attempt there and refused the retry, so the test was never completed: the summary counted it
/// neither passed nor failed, left it out of `Failed tests:`, and the run logged `Expected to run … more, but no
/// terminal events received`. The run's result was a failure either way.
#[test]
fn d99_a_failure_that_meets_no_retries_left_is_counted_failed() {
    use TestStatus::*;
    for (total, answers) in [(1, &[Failure, Failure, Passed][..]), (0, &[Failure, Passed][..])] {
        let outcome = run_with(
            |configuration| {
                configuration.retry_strategy = marathon_config::schema::RetryStrategyConfiguration::FixedQuota {
                    total_allowed_retry_quota: total,
                    retry_per_test_quota: 3,
                };
            },
            answers,
        );
        let runs = total as usize + 1;
        assert_eq!(statuses(&outcome), vec!["FAILURE"; runs], "quota {total}");
        assert!(matches!(outcome.value, Some(Ok(false))), "quota {total}");
        assert_eq!(final_status(&outcome), "FAILURE", "quota {total}");
        let text = summary(&outcome);
        assert!(
            text.starts_with(
                "Marathon run finished:\nDevice pool omni:\n\t0 passed, 1 failed, 0 ignored tests\n\tFailed tests:\n\t\ttest.SimpleTest#test\n"
            ),
            "quota {total}: {text}"
        );
        assert!(
            text.contains(&format!(
                "\tRaw: 0 passed, {runs} failed, 0 ignored, 0 incomplete tests\n"
            )),
            "quota {total}: {text}"
        );
        assert_eq!(errors(&outcome), Vec::<String>::new(), "quota {total}");
    }
}

/// Three tests on 8 devices that connect 1 ms apart, every run 5 s except on `slow` (30 s). `configure` sets up
/// the copies and the execution strategy; `statuses` gives each test's answer on every device. Returns the tests
/// of each `execute` call in order and raw.json's rows.
fn on_eight_devices(
    configure: impl FnOnce(&mut marathon_config::schema::Configuration),
    statuses: impl Fn(&str) -> TestStatus,
    slow: Option<usize>,
) -> (Vec<Vec<String>>, Vec<Value>) {
    use marathon_test_support::stub_device::{StubDevice, StubDeviceOptions};
    let (mut factory, mut script) = scenario::load("success.json");
    let tests: Vec<_> = (1..=3)
        .map(|i| scenario::test("test", "SimpleTest", &format!("step{i}")))
        .collect();
    factory.tests = tests.clone();
    configure(&mut factory.configuration.configuration);
    script.devices.clear();
    script.events.retain(|(_, event, ..)| event == "end");
    script.events[0].0 = 120_000;
    for n in 1..=8 {
        let serial = format!("serial-{n}");
        let device = StubDevice::new(StubDeviceOptions {
            serial_number: serial.as_str().into(),
            test_time_millis: if slow == Some(n) { 30_000 } else { 5_000 },
            ..StubDeviceOptions::default()
        });
        device.set_execution_results(
            tests
                .iter()
                .map(|t| (t.clone(), vec![statuses(&t.method); 5]))
                .collect(),
        );
        script.devices.insert(serial.clone(), device);
        script
            .events
            .push((1_000 + n as i64, "connect".into(), Some(serial), Value::Null));
    }
    script.events.sort_by_key(|(at, ..)| *at);
    let outcome = scenario::drive(factory, script, None, None);
    assert!(outcome.completed, "the run did not complete");
    let executed = outcome
        .log
        .iter()
        .filter(|entry| entry["call"] == "execute")
        .map(|entry| {
            entry["tests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap().trim_start_matches("test.SimpleTest#").to_owned())
                .collect()
        })
        .collect();
    let raw: Value = serde_json::from_str(&outcome.read("test_result/raw.json")).unwrap();
    let passed = raw.as_array().unwrap().iter().all(|row| row["status"] == "PASSED");
    // The run completed normally either way: Ok(true) when every run passed, Ok(false) otherwise, never an error.
    assert!(matches!(outcome.value, Some(Ok(_))), "{:?}", outcome.value);
    assert_eq!(matches!(outcome.value, Some(Ok(true))), passed, "{:?}", outcome.value);
    (executed, raw.as_array().unwrap().clone())
}

/// Three tests, each with 4 fixed copies (15 runs), under ANY_SUCCESS, every run passing. serial-3, which takes the
/// third test's first run, is slow.
fn copies_on_eight_devices(fast: bool) -> (Vec<Vec<String>>, usize) {
    let (executed, raw) = on_eight_devices(
        |configuration| {
            configuration.flakiness_strategy = FlakinessStrategyConfiguration::Fixed { count: 4 };
            configuration.execution_strategy.mode = ExecutionMode::AnySuccess;
            configuration.execution_strategy.fast = fast;
        },
        |_| TestStatus::Passed,
        Some(3),
    );
    assert!(raw.iter().all(|row| row["status"] == "PASSED"));
    (executed, raw.len())
}

/// D83: equal-priority copies are queued round by round (every test, then each test's first copy, ...), so the
/// first wave of devices spreads over the tests and `fast` drops the copies of a test that has passed meanwhile.
/// Before D83 the copies of one test were queued together: the first wave was step1 ×5, step2 ×2, step3 ×1, and
/// with step3's one run on the slow device, all four of its copies ran (13 runs).
#[test]
fn d83_copies_of_different_tests_interleave_so_fast_drops_more_of_them() {
    let (executed, results) = copies_on_eight_devices(true);
    let wave: Vec<&str> = executed[..8].iter().map(|batch| batch[0].as_str()).collect();
    assert!(executed.iter().all(|batch| batch.len() == 1));
    assert_eq!(
        wave,
        ["step1", "step2", "step3", "step1", "step2", "step3", "step1", "step2"]
    );
    // Only step3's copies taken before its fast run on serial-6 passed still run.
    assert_eq!(&executed[8..], [["step3"], ["step3"], ["step3"]]);
    assert_eq!(results, 11);
    // Without `fast` every copy runs: which copies exist did not change.
    let (executed, results) = copies_on_eight_devices(false);
    assert_eq!((executed.len(), results), (15, 15));
    for step in ["step1", "step2", "step3"] {
        assert_eq!(executed.iter().filter(|batch| batch[0] == step).count(), 5);
    }
}

/// D83 for `shardingStrategy: count` (the loader allows `count: 5` only with ALL_SUCCESS and flakiness `ignore`): 5 runs of
/// each of three tests, step1 failing everywhere. `fast` ends step1 at its first failure and drops its runs that are
/// still queued. In turns, the first wave is `step1 step2 step3 step1 step2 step3 step1 step2`, so step1's last two
/// runs are still queued and 13 runs happen. Before D83 the wave was step1 ×5, step2 ×3: every run of step1 had
/// started before the first failed, and all 15 ran.
#[test]
fn d83_count_repeats_interleave_so_fast_all_success_drops_queued_runs() {
    let run = |fast: bool| {
        on_eight_devices(
            |configuration| {
                configuration.sharding_strategy =
                    marathon_config::schema::ShardingStrategyConfiguration::Count { count: 5 };
                configuration.execution_strategy.mode = ExecutionMode::AllSuccess;
                configuration.execution_strategy.fast = fast;
            },
            |method| {
                if method == "step1" {
                    TestStatus::Failure
                } else {
                    TestStatus::Passed
                }
            },
            None,
        )
    };
    let (executed, raw) = run(true);
    assert!(executed.iter().all(|batch| batch.len() == 1));
    let runs = |executed: &[Vec<String>], step: &str| executed.iter().filter(|batch| batch[0] == step).count();
    assert_eq!(["step1", "step2", "step3"].map(|step| runs(&executed, step)), [3, 5, 5]);
    assert_eq!(raw.len(), 13);
    let wave: Vec<&str> = executed[..8].iter().map(|batch| batch[0].as_str()).collect();
    assert_eq!(
        wave,
        ["step1", "step2", "step3", "step1", "step2", "step3", "step1", "step2"]
    );
    // Without `fast` every run executes: which runs exist did not change.
    let (executed, raw) = run(false);
    assert_eq!(["step1", "step2", "step3"].map(|step| runs(&executed, step)), [5, 5, 5]);
    assert_eq!(raw.len(), 15);
}
