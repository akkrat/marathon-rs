//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/strategy/impl/flakiness/ProbabilityBasedFlakinessStrategyTest.kt
//! and impl/flakiness/IgnoreFlakinessStrategyTest.kt, plus the replay of
//! fixtures/oracle/strategies/flakiness.jsonl (tools/oracle/strategy-probes).

mod strategy_support;

use marathon_config::schema::FlakinessStrategyConfiguration as Cnf;
use marathon_core::{
    model::{Test, TestShard},
    strategy::{FlakinessStrategy, MetricsKey},
};
use serde_json::Value;
use strategy_support::*;

fn shard(tests: Vec<Test>) -> TestShard {
    TestShard {
        tests,
        flaky_tests: Vec::new(),
    }
}
fn probability(min_success_rate: f64, max_count: i32) -> FlakinessStrategy {
    FlakinessStrategy::from_config(&Cnf::Probability {
        min_success_rate,
        max_count,
        time_limit: limit(),
    })
}
/// The Kotlin test class's shared strategy: minSuccessRate 0.8, maxCount 5.
fn strategy() -> FlakinessStrategy {
    probability(0.8, 5)
}
fn run(tests: usize, success_rate: f64) -> TestShard {
    strategy().process(
        shard(generate_tests(tests)),
        &MetricsProviderStub::with_success_rate(success_rate),
    )
}

#[test]
fn ignore_flakiness_strategy_test_should_return_same_test_shard() {
    let shard = shard(generate_tests(10));
    let strategy = FlakinessStrategy::from_config(&Cnf::Ignore);
    assert_eq!(strategy.process(shard.clone(), &MetricsProviderStub::default()), shard);
}

#[test]
fn strategy_with_min_success_rate_0_8_should_return_2_flaky_tests_for_one_with_success_rate_0_5() {
    let result = run(1, 0.5);
    assert_eq!((result.tests.len(), result.flaky_tests.len()), (1, 2));
}

#[test]
fn strategy_with_min_success_rate_0_8_should_return_zero_flaky_tests_for_one_test_with_success_rate_0_8() {
    let result = run(1, 0.8);
    assert_eq!((result.tests.len(), result.flaky_tests.len()), (1, 0));
}

#[test]
fn strategy_with_min_success_rate_0_8_should_return_zero_flaky_tests_for_one_test_with_success_rate_1() {
    let result = run(1, 1.0);
    assert_eq!((result.tests.len(), result.flaky_tests.len()), (1, 0));
}

#[test]
fn strategy_with_min_success_rate_0_8_should_return_one_flaky_test_for_one_test_with_success_rate_0_7() {
    let result = run(1, 0.7);
    assert_eq!((result.tests.len(), result.flaky_tests.len()), (1, 1));
}

// Kotlin's name says maxCount = 3 and 0_01; the test uses maxCount 5 and 0.001 and expects 5.
#[test]
fn strategy_with_min_success_rate_0_8_should_return_3_flaky_tests_if_max_count_3_and_success_rate_0_01() {
    let result = run(1, 0.001);
    assert_eq!((result.tests.len(), result.flaky_tests.len()), (1, 5));
}

#[test]
fn strategy_with_min_success_rate_0_8_should_return_three_flaky_tests_for_three_tests_in_a_shard_with_success_rate_0_7()
{
    let result = run(3, 0.7);
    assert_eq!((result.tests.len(), result.flaky_tests.len()), (3, 3));
}

// ---- rule pins -----------------------------------------------------------------------------------------

#[test]
fn a_success_rate_of_0_gives_every_test_max_count_copies() {
    // C8: the NoOp provider (WP 3.4a) answers 0 for every test.
    let tests = generate_tests(3);
    let result = probability(0.8, 4).process(shard(tests.clone()), &MetricsProviderStub::with_success_rate(0.0));
    let expected: Vec<Test> = tests.iter().flat_map(|t| std::iter::repeat_n(t.clone(), 4)).collect();
    assert_eq!(result.flaky_tests, expected);
}

#[test]
fn the_fail_rate_is_squared_each_step() {
    // C29: success 0.1 under minSuccessRate 0.8: 0.9, 0.81, 0.6561, 0.430..., then
    // 0.185 <= 0.2 stops at 4 copies; multiplying by the base rate would give 15.
    assert_eq!(
        probability(0.8, 100)
            .process(shard(generate_tests(1)), &MetricsProviderStub::with_success_rate(0.1))
            .flaky_tests
            .len(),
        4
    );
    let mut multiplied = 0;
    let mut fail = 0.9f64;
    while fail > 1.0 - 0.8 && multiplied < 100 {
        multiplied += 1;
        fail *= 0.9;
    }
    assert_eq!(multiplied, 15);
}

#[test]
fn copies_follow_shard_order_and_replace_existing_flaky_tests() {
    let tests = generate_tests(2);
    let metrics = MetricsProviderStub::with_success_rates([(tests[0].clone(), 0.7), (tests[1].clone(), 0.5)]);
    let input = TestShard {
        tests: tests.clone(),
        flaky_tests: vec![tests[1].clone()],
    };
    let result = strategy().process(input, &metrics);
    assert_eq!(
        result.flaky_tests,
        vec![tests[0].clone(), tests[1].clone(), tests[1].clone()]
    );
    assert_eq!(
        strategy().metrics_keys(),
        vec![MetricsKey::SuccessRate { limit: limit() }]
    );
    assert!(FlakinessStrategy::from_config(&Cnf::Ignore).metrics_keys().is_empty());
}

// ---- oracle (c) ----------------------------------------------------------------------------------------

#[test]
fn flakiness_oracle_table_replays() {
    for row in rows(
        include_str!("../../../fixtures/oracle/strategies/flakiness.jsonl"),
        "flakiness",
    ) {
        let trace = Trace::default();
        let metrics = RecordingMetrics::from_json(&row["metrics"], trace.clone());
        let min = double_of(&row["minSuccessRate"]);
        let max = row["maxCount"].as_i64().unwrap_or_default() as i32;
        let result = probability(min, max).process(shard(tests_of(&row["tests"])), &metrics);
        let at = format!("min {min}, max {max}");
        assert_eq!(tsj(&result.tests), row["result"], "{at}");
        assert_eq!(tsj(&result.flaky_tests), row["flaky"], "{at}");
        assert_eq!(Value::Array(trace.drain()), row["trace"], "{at}");
    }
}
