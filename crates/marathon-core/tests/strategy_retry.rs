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

#[test]
fn total_allowed_quota_tests_should_return_false_if_total_allowed_quota_is_0() {
    assert!(!RetryWatchdog::new(0, 3).request_retry(1));
}

#[test]
fn total_allowed_quota_tests_should_return_true_if_total_allowed_quota_is_not_0() {
    assert!(RetryWatchdog::new(100, 3).request_retry(1));
}

#[test]
fn max_retry_per_test_quota_should_return_false_if_max_retry_per_test_quota_is_0_and_input_is_1() {
    assert!(!RetryWatchdog::new(100, 0).request_retry(1));
}

#[test]
fn max_retry_per_test_quota_should_return_false_if_max_retry_per_test_quota_is_2_and_input_is_3() {
    assert!(!RetryWatchdog::new(100, 2).request_retry(3));
}

#[test]
fn max_retry_per_test_quota_should_return_true_if_max_retry_per_test_quota_is_3_and_input_is_1() {
    assert!(RetryWatchdog::new(100, 3).request_retry(1));
}

#[test]
fn max_retry_per_test_quota_should_return_true_if_max_retry_per_test_quota_is_2_and_input_is_1() {
    assert!(RetryWatchdog::new(100, 2).request_retry(1));
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
fn at_zero_quota_left_add_retry_still_runs_and_the_decrement_refuses() {
    // C5: t0 takes the only retry; t1 passes retryPossible at left == 0, its
    // AddRetry runs (a phantom total+1 in the accumulator) and requestRetry refuses.
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
    // t2 then fails retryPossible (left -1) and never reaches the accumulator.
    assert_eq!(progress.calls, vec![tests[0].clone(), tests[1].clone()]);
    let RetryStrategy::FixedQuota(fixed) = &strategy else {
        unreachable!()
    };
    assert_eq!(fixed.watchdog().total_allowed_retry_left(), -1);
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
fn watchdog_counters_wrap_like_java_ints() {
    let mut watchdog = RetryWatchdog::new(i32::MIN, i32::MAX);
    assert!(!watchdog.retry_possible(0));
    assert!(watchdog.request_retry(0), "MIN - 1 wraps to MAX");
    assert_eq!(watchdog.total_allowed_retry_left(), i32::MAX);
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
        let name = test.to_test_name('.', '#').to_string_lossy();
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

#[test]
fn retry_oracle_table_replays() {
    for row in rows(include_str!("../../../fixtures/oracle/strategies/retry.jsonl"), "retry") {
        let case = row["case"].as_str().unwrap_or_default();
        let trace = Trace::default();
        let shard_tests = tests_of(&row["shard"]);
        let flaky = tests_of(&row["flaky"]);
        let mut states = HashMap::new();
        for test in shard_tests.iter().chain(&flaky) {
            states
                .entry(test.to_test_name('.', '#').to_string_lossy())
                .or_insert(("Added", 0))
                .1 += 1;
        }
        for pre in row["pre"].as_array().into_iter().flatten() {
            let name = test_of(&pre[0]).to_test_name('.', '#').to_string_lossy();
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
                .map(|r| r.test)
                .collect();
            assert_eq!(tsj(&returned), round["returned"], "{at}");
            assert_eq!(Value::Array(trace.drain()), round["trace"], "{at}");
            let RetryStrategy::FixedQuota(fixed) = &strategy else {
                unreachable!()
            };
            assert_eq!(
                json!(fixed.watchdog().total_allowed_retry_left()),
                round["left"],
                "{at}"
            );
            for entry in round["counts"].as_array().into_iter().flatten() {
                assert_eq!(
                    json!(fixed.accumulator().count(&pool, &test_of(&entry[0]))),
                    entry[1],
                    "{at}"
                );
            }
            // The accumulator totals (C5's phantom total+1) are 3.7's assertion; the
            // stand-in only has to agree with them where it models AddRetry.
            for entry in round["totals"].as_array().into_iter().flatten() {
                let name = entry[0].as_str().unwrap_or_default();
                let (state, total) = progress.states[name];
                assert_eq!(
                    (json!(state), json!(total)),
                    (entry[1].clone(), entry[2].clone()),
                    "{at}: {name}"
                );
            }
        }
    }
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
