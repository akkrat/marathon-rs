//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/strategy/impl/sorting/ExecutionTimeSortingStrategyTest.kt
//! and impl/sorting/SuccessRateSortingStrategyTest.kt; D60 insertion-time keys.

mod strategy_support;

use marathon_config::schema::SortingStrategyConfiguration as Cnf;
use marathon_core::{
    model::Test,
    strategy::{MetricsKey, MetricsLookup, SortingStrategy, TestKey, TestQueue},
    time::ReplayHooks,
};
use serde_json::{Value, json};
use std::{cmp::Ordering, sync::Arc};
use strategy_support::*;

fn hooks() -> Arc<ScriptedHooks> {
    ScriptedHooks::with_draws([], None)
}
fn key(strategy: &SortingStrategy, metrics: Arc<dyn MetricsLookup>) -> TestKey {
    strategy.process(metrics, hooks())
}
fn execution_time() -> SortingStrategy {
    SortingStrategy::from_config(&Cnf::ExecutionTime {
        percentile: 0.8,
        time_limit: limit(),
    })
}
fn success_rate(ascending: bool) -> SortingStrategy {
    SortingStrategy::from_config(&Cnf::SuccessRate {
        time_limit: limit(),
        ascending,
    })
}
fn sorted_by_success_rate(strategy: &SortingStrategy) -> (Vec<Test>, Vec<Test>) {
    let tests = generate_tests(3);
    let metrics =
        MetricsProviderStub::with_success_rates(tests.iter().enumerate().map(|(i, t)| (t.clone(), 0.1 * i as f64)));
    let mut result = tests.clone();
    let mut queue = TestQueue::new(key(strategy, Arc::new(metrics)));
    queue.add_all(result);
    result = std::iter::from_fn(|| queue.poll()).collect();
    (tests, result)
}

#[test]
fn strategy_with_min_success_rate_0_8_single_test_shard_should_return_3_tests_sorted_by_execution_time() {
    let tests = generate_tests(3);
    let metrics = MetricsProviderStub::with_execution_times(
        tests
            .iter()
            .enumerate()
            .map(|(i, t)| (t.clone(), 1000.0 + i as f64 * 1000.0)),
    );
    let mut result = tests.clone();
    let mut queue = TestQueue::new(key(&execution_time(), Arc::new(metrics)));
    queue.add_all(result);
    result = std::iter::from_fn(|| queue.poll()).collect();
    assert_eq!(result.len(), 3);
    assert_eq!(result, vec![tests[2].clone(), tests[1].clone(), tests[0].clone()]);
}

#[test]
fn queued_tests_keep_their_order_if_their_expected_durations_change() {
    let tests = generate_tests(3);
    let metrics = Arc::new(MetricsProviderStub::with_execution_times([
        (tests[0].clone(), 1000.0),
        (tests[1].clone(), 2000.0),
        (tests[2].clone(), 3000.0),
    ]));
    let mut queue = TestQueue::new(key(&execution_time(), metrics.clone()));
    queue.add_all(tests.clone());
    metrics.set_execution_time(&tests[0], 5000.0);
    let polled: Vec<Test> = std::iter::from_fn(|| queue.poll()).collect();
    assert_eq!(polled, vec![tests[2].clone(), tests[1].clone(), tests[0].clone()]);
}

#[test]
fn strategy_with_default_ordering_single_test_shard_should_return_3_tests_sorted_by_descending_success_rate() {
    let strategy = SortingStrategy::from_config(&marathon_config::schema::SortingStrategyConfiguration::SuccessRate {
        time_limit: limit(),
        ascending: false,
    });
    let (tests, result) = sorted_by_success_rate(&strategy);
    assert_eq!(result, vec![tests[2].clone(), tests[1].clone(), tests[0].clone()]);
}

// Kotlin's name says "descending" for the ascending case; kept as is.
#[test]
fn strategy_with_ascending_ordering_single_test_shard_should_return_3_tests_sorted_by_descending_success_rate() {
    let (tests, result) = sorted_by_success_rate(&success_rate(true));
    assert_eq!(result, tests);
}

#[test]
fn strategy_with_descending_ordering_single_test_shard_should_return_3_tests_sorted_by_descending_success_rate() {
    let (tests, result) = sorted_by_success_rate(&success_rate(false));
    assert_eq!(result, vec![tests[2].clone(), tests[1].clone(), tests[0].clone()]);
}

// ---- D60 keys -----------------------------------------------------------------------------------------

fn defaults() -> Value {
    json!({"successRates": [], "executionTimes": [], "successRate": "0.5", "executionTime": "1500.0"})
}

#[test]
fn metrics_keys_are_extracted_once_per_entry_in_insertion_order() {
    let tests = generate_tests(3);
    for strategy in [execution_time(), success_rate(true), success_rate(false)] {
        let trace = Trace::default();
        let metrics = Arc::new(RecordingMetrics::from_json(&defaults(), trace.clone()));
        let mut queue = TestQueue::new(key(&strategy, metrics));
        queue.offer(tests[0].clone());
        assert_eq!(trace.drain().len(), 1, "even a singleton extracts its key");
        queue.add_all([tests[2].clone(), tests[1].clone(), tests[2].clone()]);
        let operation = if matches!(strategy, SortingStrategy::ExecutionTime { .. }) {
            "executionTime"
        } else {
            "successRate"
        };
        assert_eq!(
            trace.drain(),
            [
                json!([operation, tj(&tests[2])]),
                json!([operation, tj(&tests[1])]),
                json!([operation, tj(&tests[2])])
            ]
        );
        queue.peek();
        queue.iter().for_each(|_| {});
        queue.poll();
        queue.remove_all(&tests[1..]);
        assert!(trace.drain().is_empty(), "only insertion reads metrics");
    }
}

#[test]
fn random_order_draws_in_first_insertion_order_and_shares_its_cache() {
    let tests = generate_tests(3);
    let hooks = ScriptedHooks::with_draws([7, 3, 5], None);
    let strategy = SortingStrategy::from_config(&Cnf::RandomOrder);
    let metrics: Arc<dyn MetricsLookup> = Arc::new(MetricsProviderStub::default());
    let mut first = TestQueue::new(strategy.process(metrics.clone(), hooks.clone()));
    first.add_all([tests[0].clone(), tests[1].clone(), tests[0].clone()]);
    assert_eq!(*lock(&hooks.drawn), [7, 3]);
    assert_eq!(first.poll(), Some(tests[1].clone()));
    first.offer(tests[1].clone());
    let mut second = TestQueue::new(strategy.process(metrics, hooks.clone()));
    second.add_all([tests[1].clone(), tests[2].clone(), tests[0].clone()]);
    assert_eq!(*lock(&hooks.drawn), [7, 3, 5]);
    assert_eq!(
        std::iter::from_fn(|| second.poll()).collect::<Vec<_>>(),
        [tests[1].clone(), tests[2].clone(), tests[0].clone()]
    );
    assert!(hooks.verify_consumed().is_ok());
}

#[test]
fn random_collisions_are_fifo_and_a_singleton_draws_immediately() {
    let tests = generate_tests(3);
    let hooks = ScriptedHooks::with_draws([4, 4, 4], None);
    let mut queue = TestQueue::new(
        SortingStrategy::from_config(&Cnf::RandomOrder)
            .process(Arc::new(MetricsProviderStub::default()), hooks.clone()),
    );
    queue.offer(tests[0].clone());
    assert_eq!(*lock(&hooks.drawn), [4]);
    queue.add_all(tests[1..].iter().cloned());
    assert_eq!(std::iter::from_fn(|| queue.poll()).collect::<Vec<_>>(), tests);
    assert!(hooks.verify_consumed().is_ok());
}

#[test]
fn random_order_keys_a_failed_draw_0_and_the_replay_reports_it() {
    let tests = generate_tests(2);
    let hooks = ScriptedHooks::with_draws([-1], None);
    let strategy = SortingStrategy::from_config(&Cnf::RandomOrder);
    let mut queue = TestQueue::new(strategy.process(Arc::new(MetricsProviderStub::default()), hooks.clone()));
    queue.add_all(tests.clone());
    assert_eq!(queue.poll(), Some(tests[0].clone()));
    queue.offer(tests[1].clone());
    assert_eq!(*lock(&hooks.drawn), [-1]);
    assert!(hooks.verify_consumed().is_err());
}

#[test]
fn no_sorting_is_fifo_and_reinserted_entries_go_after_survivors() {
    let tests = generate_tests(4);
    let mut queue = TestQueue::new(key(
        &SortingStrategy::NoSorting,
        Arc::new(MetricsProviderStub::default()),
    ));
    queue.add_all(tests.clone());
    assert_eq!(queue.poll(), Some(tests[0].clone()));
    queue.offer(tests[0].clone());
    assert_eq!(
        std::iter::from_fn(|| queue.poll()).collect::<Vec<_>>(),
        [tests[1].clone(), tests[2].clone(), tests[3].clone(), tests[0].clone()]
    );
}

#[test]
fn float_keys_preserve_the_java_comparator_order_and_all_nan_ties() {
    use jcompat::jnum::double_compare;
    let values = [
        f64::from_bits(0xfff8_0000_0000_0001),
        -0.0,
        f64::INFINITY,
        f64::NAN,
        0.0,
        f64::NEG_INFINITY,
        1.0,
        f64::from_bits(0x7ff0_0000_0000_0001),
    ];
    let tests = generate_tests(values.len());
    for (strategy, descending) in [
        (execution_time(), true),
        (success_rate(true), false),
        (success_rate(false), true),
    ] {
        let metrics = Arc::new(MetricsProviderStub::with_execution_times(
            tests.iter().cloned().zip(values),
        ));
        *lock(&metrics.success_rates) = tests.iter().cloned().zip(values).collect();
        let mut queue = TestQueue::new(key(&strategy, metrics));
        queue.add_all(tests.clone());
        let mut expected: Vec<usize> = (0..values.len()).collect();
        expected.sort_by(|a, b| {
            if descending {
                double_compare(values[*b], values[*a])
            } else {
                double_compare(values[*a], values[*b])
            }
        });
        assert_eq!(
            expected,
            if descending {
                vec![0, 3, 7, 2, 6, 4, 1, 5]
            } else {
                vec![5, 1, 4, 6, 2, 0, 3, 7]
            }
        );
        let expected: Vec<_> = expected.into_iter().map(|i| tests[i].clone()).collect();
        assert_eq!(std::iter::from_fn(|| queue.poll()).collect::<Vec<_>>(), expected);
        let metrics = Arc::new(MetricsProviderStub::with_execution_times(
            tests.iter().cloned().map(|t| (t, -f64::NAN)),
        ));
        *lock(&metrics.success_rates) = tests.iter().cloned().map(|t| (t, f64::NAN)).collect();
        let mut queue = TestQueue::new(key(&strategy, metrics));
        queue.add_all(tests.clone());
        assert_eq!(
            std::iter::from_fn(|| queue.poll()).collect::<Vec<_>>(),
            tests,
            "all NaNs tie FIFO"
        );
    }
    assert_eq!(double_compare(-0.0, 0.0), Ordering::Less);
}

#[test]
fn remove_all_removes_equal_tests_with_different_keys_and_metadata() {
    use marathon_core::{
        model::{MetaMap, MetaProperty},
        strategy::queue::OrderedQueue,
    };
    let tests = generate_tests(2);
    let mut duplicate = tests[0].clone();
    duplicate.meta_properties.push(MetaProperty {
        name: "different metadata".into(),
        values: MetaMap::default(),
    });
    let mut priorities = [2, 1, 0].into_iter();
    let mut queue = OrderedQueue::new(Box::new(move |_: &Test| priorities.next().unwrap()));
    queue.add_all([tests[0].clone(), tests[1].clone(), duplicate]);
    assert_eq!(queue.len(), 3);
    assert!(queue.remove_all(&[tests[0].clone(), tests[0].clone()]));
    assert_eq!(queue.iter().cloned().collect::<Vec<_>>(), [tests[1].clone()]);
    assert!(!queue.remove_all(&tests[..1]));
    assert!(!queue.remove_all(&[]));
    assert_eq!(queue.poll(), Some(tests[1].clone()));
}

#[test]
fn sorting_declares_the_metrics_keys_it_can_touch() {
    assert_eq!(
        execution_time().metrics_keys(),
        vec![MetricsKey::ExecutionTime {
            percentile: 0.8,
            limit: limit()
        }]
    );
    assert_eq!(
        success_rate(true).metrics_keys(),
        vec![MetricsKey::SuccessRate { limit: limit() }]
    );
    assert!(SortingStrategy::NoSorting.metrics_keys().is_empty());
    assert!(
        SortingStrategy::from_config(&Cnf::RandomOrder)
            .metrics_keys()
            .is_empty()
    );
}

#[allow(dead_code)]
fn _queue_type_is_send(queue: TestQueue) -> impl Send {
    queue
}

#[test]
fn metrics_keys_keep_java_double_equality_for_prefetch() {
    let key = |percentile| MetricsKey::ExecutionTime {
        percentile,
        limit: limit(),
    };
    assert_eq!(key(f64::NAN), key(f64::from_bits(0xfff8_0000_0000_0001)));
    assert_ne!(key(-0.0), key(0.0));
    assert_eq!(key(90.0), key(90.0));
    assert_ne!(key(90.0), MetricsKey::SuccessRate { limit: limit() });
}
