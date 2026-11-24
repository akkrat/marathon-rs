//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/strategy/impl/retry/fixedquota/FixedQuotaRetryStrategyTest.kt
//! and impl/retry/fixedquota/{PoolTestFailureAccumulator,RetryWatchdog}Test.kt, impl/retry/NoRetryStrategyTest.kt,
//! plus the replay of fixtures/oracle/strategies/retry.jsonl (tools/oracle/strategy-probes).
//!
//! The Kotlin tests pass a fresh PoolProgressAccumulator; on a fresh Added state
//! AddRetry has no side effect, so NoActionProgress stands in (WP 3.2 decision 1).
//! "should return 0 tests if test reached terminal state" needs the real FSM (WP 3.7): it runs
//! the WP 3.5 accumulator on the core executor.

#[path = "support/progress.rs"]
mod progress_support;
mod strategy_support;

use marathon_config::schema::RetryStrategyConfiguration as Cnf;
use marathon_core::{
    model::{DevicePoolId, Test, TestAction, TestShard, TestStatus},
    strategy::{PoolTestFailureAccumulator, RetryProgress, RetryStrategy, RetryWatchdog},
};
use serde_json::{Value, json};
use std::collections::HashMap;
use strategy_support::*;

fn pool(name: &str) -> DevicePoolId {
    DevicePoolId { name: name.into() }
}
fn shard(tests: &[Test]) -> TestShard {
    TestShard {
        tests: tests.to_vec(),
        flaky_tests: Vec::new(),
    }
}
fn fixed_quota(total: i32) -> RetryStrategy {
    RetryStrategy::from_config(&Cnf::FixedQuota {
        total_allowed_retry_quota: total,
        retry_per_test_quota: 3,
    })
}
/// FixedQuotaRetryStrategyConfiguration() defaults: 200 total, 3 per test.
fn default_quota() -> RetryStrategy {
    fixed_quota(200)
}
fn retried(strategy: &mut RetryStrategy, tests: &[Test], shard: &TestShard) -> usize {
    let pool = pool("DevicePoolId-1");
    strategy
        .process(&pool, &generate_test_results(tests), shard, &mut NoActionProgress)
        .len()
}

#[test]
fn should_return_empty_list() {
    let tests = generate_tests(50);
    let mut strategy = RetryStrategy::from_config(&Cnf::NoRetry);
    let result = strategy.process(
        &pool("devicePoolId"),
        &generate_test_results(&tests),
        &shard(&tests),
        &mut NoActionProgress,
    );
    assert!(result.is_empty());
}

#[test]
fn total_quota_tests_total_quota_is_1() {
    let tests = generate_tests(10);
    assert_eq!(retried(&mut fixed_quota(1), &tests, &shard(&tests)), 1);
}

#[test]
fn total_quota_tests_total_quota_more_than_size_of_the_input_list() {
    let tests = generate_tests(10);
    assert_eq!(retried(&mut fixed_quota(10 + 1), &tests, &shard(&tests)), 10);
}

#[test]
fn flakiness_tests_should_return_all_tests_if_flakytests_size_0() {
    let tests = generate_tests(50);
    assert_eq!(retried(&mut default_quota(), &tests, &shard(&tests)), 50);
}

#[test]
fn flakiness_tests_should_return_0_tests_if_flakiness_strategy_added_3_flaky_tests_per_test() {
    let tests = generate_tests(50);
    let shard = TestShard {
        tests: tests.clone(),
        flaky_tests: [tests.clone(), tests.clone(), tests.clone()].concat(),
    };
    assert_eq!(retried(&mut default_quota(), &tests, &shard), 0);
}

#[test]
fn same_device_pool_id_default_retry_count_value_0() {
    let accumulator = PoolTestFailureAccumulator::default();
    assert_eq!(accumulator.count(&pool("DevicePoolId"), &default_test()), 0);
}

#[test]
fn same_device_pool_id_record_should_increment_retry_counter() {
    let mut accumulator = PoolTestFailureAccumulator::default();
    let (pool, test) = (pool("DevicePoolId"), default_test());
    assert_eq!(accumulator.count(&pool, &test), 0);
    accumulator.record(&pool, &test);
    assert_eq!(accumulator.count(&pool, &test), 1);
    accumulator.record(&pool, &test);
    assert_eq!(accumulator.count(&pool, &test), 2);
}

#[test]
fn same_device_pool_id_record_should_increment_only_associated_counter() {
    let mut accumulator = PoolTestFailureAccumulator::default();
    let pool = pool("DevicePoolId");
    let test = default_test();
    let test2 = generate_test("pkg", "clazz", "testMethod2");
    let counts = |a: &PoolTestFailureAccumulator| (a.count(&pool, &test), a.count(&pool, &test2));
    assert_eq!(counts(&accumulator), (0, 0));
    accumulator.record(&pool, &test);
    assert_eq!(counts(&accumulator), (1, 0));
    accumulator.record(&pool, &test2);
    assert_eq!(counts(&accumulator), (1, 1));
    accumulator.record(&pool, &test2);
    assert_eq!(counts(&accumulator), (1, 2));
}

#[test]
fn different_device_pool_ids_record_should_increment_counter_only_for_specified_pool_id() {
    let mut accumulator = PoolTestFailureAccumulator::default();
    let (pool1, pool2, test) = (pool("DevicePoolId-1"), pool("DevicePoolId-2"), default_test());
    assert_eq!(
        (accumulator.count(&pool1, &test), accumulator.count(&pool2, &test)),
        (0, 0)
    );
    accumulator.record(&pool1, &test);
    accumulator.record(&pool2, &test);
    assert_eq!(
        (accumulator.count(&pool1, &test), accumulator.count(&pool2, &test)),
        (1, 1)
    );
}

/// RetryWatchdogTest's `requestRetry(n)`: D99 decides and charges in `grant`, around AddRetry.
fn grants(mut watchdog: RetryWatchdog, failures: i32) -> bool {
    watchdog.grant(failures, || true)
}

#[test]
fn total_allowed_quota_tests_should_return_false_if_total_allowed_quota_is_0() {
    assert!(!grants(RetryWatchdog::new(0, 3), 1));
}

#[test]
fn total_allowed_quota_tests_should_return_true_if_total_allowed_quota_is_not_0() {
    assert!(grants(RetryWatchdog::new(100, 3), 1));
}

#[test]
fn max_retry_per_test_quota_should_return_false_if_max_retry_per_test_quota_is_0_and_input_is_1() {
    assert!(!grants(RetryWatchdog::new(100, 0), 1));
}

#[test]
fn max_retry_per_test_quota_should_return_false_if_max_retry_per_test_quota_is_2_and_input_is_3() {
    assert!(!grants(RetryWatchdog::new(100, 2), 3));
}

#[test]
fn max_retry_per_test_quota_should_return_true_if_max_retry_per_test_quota_is_3_and_input_is_1() {
    assert!(grants(RetryWatchdog::new(100, 3), 1));
}

#[test]
fn max_retry_per_test_quota_should_return_true_if_max_retry_per_test_quota_is_2_and_input_is_1() {
    assert!(grants(RetryWatchdog::new(100, 2), 1));
}

// ---- rule pins -----------------------------------------------------------------------------------------

/// Records retryTest calls and answers from a script keyed by test.
#[derive(Default)]
struct ScriptedProgress {
    complete: Vec<Test>,
    calls: Vec<Test>,
}
impl RetryProgress for ScriptedProgress {
    fn retry_test(&mut self, test: &Test) -> Option<TestAction> {
        self.calls.push(test.clone());
        self.complete.contains(test).then_some(TestAction::Complete)
    }
}

#[test]
fn at_zero_quota_left_add_retry_does_not_run_d99() {
    // D99 (C5): t0 takes the only retry; t1 meets 0 left and is refused before AddRetry, so its total is
    // unchanged and the failure can end the test. Kotlin ran t1's AddRetry and refused in the decrement (-1).
    let tests = generate_tests(3);
    let mut strategy = fixed_quota(1);
    let mut progress = ScriptedProgress::default();
    let result = strategy.process(
        &pool("p"),
        &generate_test_results(&tests),
        &shard(&tests),
        &mut progress,
    );
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].test, tests[0]);
    assert_eq!(progress.calls, vec![tests[0].clone()]);
    let RetryStrategy::FixedQuota(fixed) = &strategy else {
        unreachable!()
    };
    assert_eq!(fixed.watchdog().total_allowed_retry_left(), 0);
}

#[test]
fn a_refund_after_the_quota_ran_out_grants_the_next_failure_d99() {
    // D99 with D69: nothing is charged at 0 left, so a refunded retry is one whole retry again. Kotlin's
    // phantom left -1, which the refund only brought back to 0, the C5 boundary.
    let tests = generate_tests(3);
    let mut strategy = fixed_quota(1);
    let mut progress = ScriptedProgress::default();
    let results = generate_test_results(&tests);
    let pool = pool("p");
    assert_eq!(
        strategy.retried(&pool, &results[..2], &shard(&tests), &mut progress),
        [0]
    );
    strategy.refund();
    assert_eq!(
        strategy.retried(&pool, &results[2..], &shard(&tests), &mut progress),
        [0]
    );
    assert_eq!(progress.calls, vec![tests[0].clone(), tests[2].clone()]);
}

#[test]
fn a_complete_test_is_not_retried_and_does_not_spend_quota() {
    let tests = generate_tests(2);
    let mut strategy = fixed_quota(1);
    let mut progress = ScriptedProgress {
        complete: vec![tests[0].clone()],
        ..Default::default()
    };
    let result = strategy.process(
        &pool("p"),
        &generate_test_results(&tests),
        &shard(&tests),
        &mut progress,
    );
    assert_eq!(
        result.iter().map(|r| r.test.clone()).collect::<Vec<_>>(),
        vec![tests[1].clone()]
    );
    let RetryStrategy::FixedQuota(fixed) = &strategy else {
        unreachable!()
    };
    assert_eq!(fixed.watchdog().total_allowed_retry_left(), 0);
}

#[test]
fn each_strategy_instance_has_its_own_total_quota() {
    // C30: QueueActor builds one strategy per pool, so two pools retry twice the total.
    let tests = generate_tests(2);
    let (mut a, mut b) = (fixed_quota(1), fixed_quota(1));
    assert_eq!(retried(&mut a, &tests[..1], &shard(&tests)), 1);
    assert_eq!(retried(&mut b, &tests[1..], &shard(&tests)), 1);
    assert_eq!(retried(&mut a, &tests[1..], &shard(&tests)), 0);
}

#[test]
fn a_negative_total_quota_grants_nothing_and_is_not_charged_d99() {
    // Kotlin's decrement wrapped Int.MIN_VALUE to MAX_VALUE; its retryPossible refused first, and D99's single
    // check refuses before anything is charged, so no AddRetry runs and the counter never wraps.
    let mut watchdog = RetryWatchdog::new(i32::MIN, i32::MAX);
    assert!(!watchdog.grant(0, || unreachable!("AddRetry ran for a refused retry")));
    assert_eq!(watchdog.total_allowed_retry_left(), i32::MIN);
}

#[test]
fn a_grant_refused_by_the_per_test_quota_runs_no_add_retry() {
    let mut watchdog = RetryWatchdog::new(5, 2);
    assert!(!watchdog.grant(3, || unreachable!("AddRetry ran for a refused retry")));
    assert!(watchdog.grant(2, || true));
    assert_eq!(watchdog.total_allowed_retry_left(), 4);
}

// ---- oracle (d) ----------------------------------------------------------------------------------------

/// The slice of the PoolProgressAccumulator FSM that AddRetry touches, seeded
/// from the row: Added gains a total and has no side effect; Passed and Failed
/// are terminal and return Complete; a test outside the shard has no FSM.
struct OracleProgress {
    states: HashMap<String, (&'static str, i32)>,
    trace: Trace,
}
impl RetryProgress for OracleProgress {
    fn retry_test(&mut self, test: &Test) -> Option<TestAction> {
        let name = test.to_test_name('.', '#').to_owned();
        let Some((state, total)) = self.states.get_mut(&name) else {
            self.trace.push(json!(["retryTest", name, "none"]));
            return None;
        };
        self.trace.push(json!(["retryTest", name, state]));
        match *state {
            "Passed" | "Failed" => Some(TestAction::Complete),
            _ => {
                *total += 1;
                None
            }
        }
    }
}

fn retry_rows() -> Vec<Value> {
    rows(include_str!("../../../fixtures/oracle/strategies/retry.jsonl"), "retry")
}

/// D99: the JVM recording of a C5 phantom, rewritten to what the native strategy does. A round whose total quota
/// goes from >= 0 to -1 ended with a refused retry whose AddRetry still ran; that is always the round's last
/// `retryTest`, since nothing reaches AddRetry at -1. D99 refuses it before AddRetry: the call leaves the trace,
/// its total gains nothing, and the quota stays at 0 in this and later rounds. Rows without a phantom are
/// returned unchanged.
fn d99_expected(mut row: Value) -> Value {
    let mut left = row["total"].as_i64().unwrap();
    let mut phantom: Option<String> = None;
    for round in row["rounds"].as_array_mut().unwrap() {
        let jvm_left = round["left"].as_i64().unwrap();
        if phantom.is_none() && left >= 0 && jvm_left == -1 {
            let call = round["trace"].as_array_mut().unwrap().pop().unwrap();
            assert_eq!(call[0], "retryTest");
            assert!(
                call[2] != "Passed" && call[2] != "Failed",
                "a terminal test is no phantom: {call}"
            );
            phantom = Some(call[1].as_str().unwrap().to_owned());
        }
        left = jvm_left;
        if let Some(name) = &phantom {
            round["left"] = json!(0);
            for entry in round["totals"].as_array_mut().unwrap() {
                if entry[0] == name.as_str() {
                    entry[2] = json!(entry[2].as_i64().unwrap() - 1);
                }
            }
        }
    }
    row
}

#[test]
fn retry_oracle_table_replays_with_d99() {
    for row in retry_rows() {
        let mismatches = retry_mismatches(&d99_expected(row));
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }
}

/// The D99 allowance rewrites exactly the five recordings that reach a phantom, and the native strategy no longer
/// reproduces any of them as the JVM recorded it.
#[test]
fn the_d99_allowance_covers_only_the_c5_phantoms() {
    let mut rewritten = Vec::new();
    for row in retry_rows() {
        let case = row["case"].as_str().unwrap().to_owned();
        let native = d99_expected(row.clone());
        if native == row {
            continue;
        }
        assert!(
            !retry_mismatches(&row).is_empty(),
            "{case}: the JVM's phantom still replays"
        );
        rewritten.push(case);
    }
    assert_eq!(
        rewritten,
        [
            "total quota 1, two failures",
            "total quota 2 across rounds",
            "total quota 0",
            "pool A of two, total quota 1",
            "pool B of two, total quota 1",
        ]
    );
    // A quota that starts below 0 never charges, so -1 there is no phantom.
    let negative = retry_rows()
        .into_iter()
        .find(|row| row["case"] == "negative quotas")
        .unwrap();
    assert_eq!(d99_expected(negative.clone()), negative);
}

/// What differs from a recorded row, round by round.
fn retry_mismatches(row: &Value) -> Vec<String> {
    let mut mismatches = Vec::new();
    let mut check = |at: String, actual: Value, expected: &Value| {
        if actual != *expected {
            mismatches.push(format!("{at}: rust {actual} expected {expected}"));
        }
    };
    let case = row["case"].as_str().unwrap_or_default();
    let trace = Trace::default();
    let shard_tests = tests_of(&row["shard"]);
    let flaky = tests_of(&row["flaky"]);
    let mut states = HashMap::new();
    for test in shard_tests.iter().chain(&flaky) {
        states
            .entry(test.to_test_name('.', '#').to_owned())
            .or_insert(("Added", 0))
            .1 += 1;
    }
    for pre in row["pre"].as_array().into_iter().flatten() {
        let name = test_of(&pre[0]).to_test_name('.', '#').to_owned();
        let terminal = if pre[1] == "PASSED" { "Passed" } else { "Failed" };
        if let Some(state) = states.get_mut(&name) {
            state.0 = terminal;
        }
    }
    let mut progress = OracleProgress {
        states,
        trace: trace.clone(),
    };
    let shard = TestShard {
        tests: shard_tests.clone(),
        flaky_tests: flaky.clone(),
    };
    let total = row["total"].as_i64().unwrap_or_default() as i32;
    let per_test = row["perTest"].as_i64().unwrap_or_default() as i32;
    let mut strategy = RetryStrategy::from_config(&Cnf::FixedQuota {
        total_allowed_retry_quota: total,
        retry_per_test_quota: per_test,
    });
    let pool = pool("pool");
    for (i, round) in row["rounds"].as_array().into_iter().flatten().enumerate() {
        let at = format!("{case}, round {i}");
        let failed: Vec<_> = tests_of(&round["failed"])
            .iter()
            .map(|t| generate_test_result(t, TestStatus::Failure))
            .collect();
        let returned: Vec<Test> = strategy
            .process(&pool, &failed, &shard, &mut progress)
            .into_iter()
            .map(|r| r.test.clone())
            .collect();
        check(format!("{at}: returned"), tsj(&returned), &round["returned"]);
        check(format!("{at}: trace"), Value::Array(trace.drain()), &round["trace"]);
        let RetryStrategy::FixedQuota(fixed) = &strategy else {
            unreachable!()
        };
        check(
            format!("{at}: left"),
            json!(fixed.watchdog().total_allowed_retry_left()),
            &round["left"],
        );
        for entry in round["counts"].as_array().into_iter().flatten() {
            check(
                format!("{at}: count"),
                json!(fixed.accumulator().count(&pool, &test_of(&entry[0]))),
                &entry[1],
            );
        }
        // The accumulator totals are 3.7's assertion; the stand-in only has to agree with
        // them where it models AddRetry.
        for entry in round["totals"].as_array().into_iter().flatten() {
            let name = entry[0].as_str().unwrap_or_default();
            let (state, total) = progress.states[name];
            check(
                format!("{at}: {name}"),
                json!([state, total]),
                &json!([entry[1], entry[2]]),
            );
        }
    }
    mismatches
}

#[test]
fn should_return_0_tests_if_test_reached_terminal_state() {
    progress_support::run(|core| {
        Box::pin(async move {
            let mut strategy = default_quota();
            let pool_id = pool("DevicePoolId-1");
            let tests = generate_tests(1);
            let test_results = generate_test_results(&tests);
            let mut accumulator = progress_support::Harness::new(
                &core,
                tests.clone(),
                marathon_config::schema::ExecutionMode::AnySuccess,
                false,
            );
            let device_info = create_device_info();
            let test = &tests[0];
            accumulator.start(&device_info, test).await;
            accumulator.end(test_results[0].clone(), false).await;

            let returned = strategy.process(&pool_id, &test_results, &shard(&tests), &mut accumulator.reporter);
            assert_eq!(returned.len(), 0);
            Ok(())
        })
    });
}
