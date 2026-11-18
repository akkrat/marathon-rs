//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/strategy/impl/flakiness/ProbabilityBasedFlakinessStrategyTest.kt
//! and impl/flakiness/IgnoreFlakinessStrategyTest.kt, plus the replay of
//! fixtures/oracle/strategies/flakiness.jsonl (tools/oracle/strategy-probes).

mod strategy_support;

use marathon_config::schema::FlakinessStrategyConfiguration as Cnf;
use marathon_core::time::ReplayHooks;
use marathon_core::{
    model::{Test, TestShard},
    strategy::{FlakinessStrategy, MetricsKey, flakiness::queue_order},
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
    // C8: a 0 from working history. Unavailable history makes no copies at all (D71,
    // analytics_factory::d71_*).
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

// ---- D72: fixed ---------------------------------------------------------------------------------------

fn fixed(count: i32) -> FlakinessStrategy {
    FlakinessStrategy::from_config(&Cnf::Fixed { count })
}
/// A lookup no fixed pass may read: its answers would be ignored anyway.
struct Unread;
impl marathon_core::strategy::MetricsLookup for Unread {
    fn success_rate(&self, _: &Test, _: jcompat::jtime::Instant) -> f64 {
        panic!("fixed read a success rate")
    }
    fn execution_time(&self, _: &Test, _: f64, _: jcompat::jtime::Instant) -> f64 {
        panic!("fixed read an execution time")
    }
    fn view(&self) -> Option<std::sync::Arc<dyn marathon_core::strategy::MetricsLookup>> {
        panic!("fixed took a view")
    }
    fn success_rate_available(&self, _: jcompat::jtime::Instant, _: &str) -> bool {
        panic!("fixed asked for history")
    }
}

#[test]
fn d72_fixed_gives_every_test_count_copies_grouped_as_probability_does() {
    let tests = generate_tests(3);
    let input = TestShard {
        tests: tests.clone(),
        flaky_tests: vec![tests[0].clone()],
    };
    let result = fixed(3).process(input, &Unread);
    assert_eq!(result.tests, tests);
    assert_eq!(
        result.flaky_tests,
        tests
            .iter()
            .flat_map(|t| std::iter::repeat_n(t.clone(), 3))
            .collect::<Vec<_>>()
    );
    // The same grouping probability gives when every test is below the minimum.
    assert_eq!(
        result.flaky_tests,
        probability(0.8, 3)
            .process(shard(tests), &MetricsProviderStub::with_success_rate(0.0))
            .flaky_tests
    );
    assert!(fixed(3).metrics_keys().is_empty());
}

#[test]
fn d72_fixed_count_0_makes_no_copies() {
    let tests = generate_tests(2);
    let input = TestShard {
        tests: tests.clone(),
        flaky_tests: vec![tests[1].clone()],
    };
    assert_eq!(fixed(0).process(input, &Unread), shard(tests));
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

// ---- D83: copies enter the queue round by round -------------------------------------------------------

/// The shard through a queue keyed by `strategy`, as a pool queues it, polled to the end.
fn polled(
    shard: &TestShard,
    strategy: marathon_core::strategy::SortingStrategy,
    metrics: MetricsProviderStub,
    hooks: std::sync::Arc<ScriptedHooks>,
) -> Vec<Test> {
    let mut queue = marathon_core::strategy::TestQueue::new(strategy.process(std::sync::Arc::new(metrics), hooks));
    queue.add_all(queue_order(shard));
    std::iter::from_fn(|| queue.poll()).collect()
}
fn no_sorting(shard: &TestShard) -> Vec<Test> {
    polled(
        shard,
        marathon_core::strategy::SortingStrategy::NoSorting,
        MetricsProviderStub::default(),
        ScriptedHooks::with_draws([], None),
    )
}

#[test]
fn d83_equal_priority_copies_of_different_tests_interleave() {
    let t = generate_tests(3);
    let [a, b, c] = [&t[0], &t[1], &t[2]].map(Test::clone);
    // fixed: every test, then every test's first copy, and so on.
    let shard = fixed(2).process(shard(t.clone()), &Unread);
    assert_eq!(no_sorting(&shard), [t.clone(), t.clone(), t.clone()].concat());
    // Uneven probability counts: a round holds the tests that still have a copy, in shard order.
    let shard = TestShard {
        tests: t.clone(),
        flaky_tests: vec![a.clone(), a.clone(), c.clone(), c.clone(), c.clone()],
    };
    assert_eq!(queue_order(&shard), [&a, &b, &c, &a, &c, &a, &c, &c].map(Test::clone));
    assert_eq!(no_sorting(&shard), queue_order(&shard));
    // Count sharding's copies are in `tests`; they interleave the same way.
    let shard = TestShard {
        tests: vec![a.clone(), a.clone(), b.clone(), b.clone()],
        flaky_tests: vec![b.clone()],
    };
    assert_eq!(queue_order(&shard), [&a, &b, &a, &b, &b].map(Test::clone));
    assert_eq!(
        queue_order(&TestShard {
            tests: vec![],
            flaky_tests: vec![]
        }),
        []
    );
}

#[test]
fn d83_different_sort_keys_still_order_the_queue() {
    let t = generate_tests(3);
    let shard = fixed(2).process(shard(t.clone()), &Unread);
    let execution_time = || {
        marathon_core::strategy::SortingStrategy::from_config(
            &marathon_config::schema::SortingStrategyConfiguration::ExecutionTime {
                percentile: 0.8,
                time_limit: limit(),
            },
        )
    };
    // Distinct durations: longest first, so each test's runs stay together, as in the Kotlin heap.
    let times = MetricsProviderStub::with_execution_times([
        (t[0].clone(), 2000.0),
        (t[1].clone(), 1000.0),
        (t[2].clone(), 3000.0),
    ]);
    let grouped: Vec<Test> = [&t[2], &t[0], &t[1]]
        .into_iter()
        .flat_map(|test| std::iter::repeat_n(test.clone(), 3))
        .collect();
    assert_eq!(
        polled(&shard, execution_time(), times, ScriptedHooks::with_draws([], None)),
        grouped
    );
    // Two tests tie: they interleave with each other, ahead of the shorter one.
    let times = MetricsProviderStub::with_execution_times([
        (t[0].clone(), 2000.0),
        (t[1].clone(), 2000.0),
        (t[2].clone(), 1000.0),
    ]);
    assert_eq!(
        polled(&shard, execution_time(), times, ScriptedHooks::with_draws([], None)),
        [&t[0], &t[1], &t[0], &t[1], &t[0], &t[1], &t[2], &t[2], &t[2]].map(Test::clone)
    );
}

#[test]
fn d83_a_seeded_random_order_draws_as_before_and_ties_interleave() {
    let t = generate_tests(3);
    let shard = fixed(2).process(shard(t.clone()), &Unread);
    let hooks = ScriptedHooks::with_draws([5, 1, 5], None);
    let random = marathon_core::strategy::SortingStrategy::from_config(
        &marathon_config::schema::SortingStrategyConfiguration::RandomOrder,
    );
    let order = polled(&shard, random, MetricsProviderStub::default(), hooks.clone());
    // One draw per test, at its first insertion, in shard order: the same draws as before D83.
    assert_eq!(*lock(&hooks.drawn), [5, 1, 5]);
    assert!(hooks.verify_consumed().is_ok());
    assert_eq!(
        order,
        [&t[1], &t[1], &t[1], &t[0], &t[2], &t[0], &t[2], &t[0], &t[2]].map(Test::clone)
    );
}

#[test]
fn d83_batching_rules_hold_for_copies_in_turns() {
    use marathon_config::schema::BatchingStrategyConfiguration as Batching;
    use marathon_core::strategy::{BatchingStrategy, SortingStrategy, TestQueue};
    let batches = |batching: Batching, shard: &TestShard| {
        let hooks = ScriptedHooks::with_draws([], None);
        let mut queue = TestQueue::new(
            SortingStrategy::NoSorting.process(std::sync::Arc::new(MetricsProviderStub::default()), hooks.clone()),
        );
        queue.add_all(queue_order(shard));
        let strategy = BatchingStrategy::from_config(&batching);
        let mut out = Vec::new();
        while !queue.is_empty() {
            let batch = strategy.process(&mut queue, &MetricsProviderStub::default(), None, hooks.as_ref());
            out.push(batch.unwrap().tests);
        }
        out
    };
    // fixed-size never puts two copies of a test in one batch.
    let t = generate_tests(3);
    let fixed_size = Batching::FixedSize {
        size: 3,
        duration_millis: None,
        percentile: None,
        time_limit: None,
        last_mile_length: 0,
    };
    // With 4 copies each, the grouped order filled batch 2 as [t0, t1] (D50's 6 rejected polls ran out on t0's and
    // t1's other copies); in turns, every batch is full.
    assert_eq!(
        batches(fixed_size, &fixed(4).process(shard(t.clone()), &Unread)),
        vec![t; 5]
    );
    // class-name still takes every copy of its class, now in turns.
    let m = generate_class_group_tests(1, 2);
    let copies = fixed(2).process(shard(m.clone()), &Unread);
    assert_eq!(
        batches(Batching::ClassName, &copies),
        [[m.clone(), m.clone(), m].concat()]
    );
}
