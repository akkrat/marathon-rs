//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/strategy/impl/sorting/ExecutionTimeSortingStrategyTest.kt
//! and impl/sorting/SuccessRateSortingStrategyTest.kt, plus the replay of
//! fixtures/oracle/strategies/sorting.jsonl (tools/oracle/strategy-probes).

mod strategy_support;

use jcompat::JavaPriorityQueue;
use marathon_config::schema::SortingStrategyConfiguration as Cnf;
use marathon_core::{
    model::Test,
    strategy::{MetricsKey, MetricsLookup, SortingStrategy, TestComparator, TestQueue},
    time::ReplayHooks,
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use strategy_support::*;

fn hooks() -> Arc<ScriptedHooks> {
    ScriptedHooks::with_draws([], None)
}
fn comparator(strategy: &SortingStrategy, metrics: Arc<dyn MetricsLookup>) -> TestComparator {
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
    // The Kotlin test uses sortedWith; a slice sort stands in for it here only.
    result.sort_by(comparator(strategy, Arc::new(metrics)));
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
    result.sort_by(comparator(&execution_time(), Arc::new(metrics)));
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
    let mut queue = TestQueue::new(comparator(&execution_time(), metrics.clone()));
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

// ---- rule pins -----------------------------------------------------------------------------------------

fn traced(row_metrics: Value) -> (Trace, Arc<RecordingMetrics>) {
    let trace = Trace::default();
    let metrics = Arc::new(RecordingMetrics::from_json(&row_metrics, trace.clone()));
    (trace, metrics)
}
fn defaults() -> Value {
    json!({"successRates": [], "executionTimes": [], "successRate": "0.5", "executionTime": "1500.0"})
}

#[test]
fn reversed_comparators_extract_the_second_arguments_key_first() {
    let [a, b] = [generate_test("a", "A", "m"), generate_test("b", "B", "m")];
    let (trace, metrics) = traced(defaults());
    comparator(&execution_time(), metrics.clone())(&a, &b);
    assert_eq!(
        trace.drain(),
        vec![json!(["executionTime", tj(&b)]), json!(["executionTime", tj(&a)])]
    );
    comparator(&success_rate(false), metrics.clone())(&a, &b);
    assert_eq!(
        trace.drain(),
        vec![json!(["successRate", tj(&b)]), json!(["successRate", tj(&a)])]
    );
    // Ascending is not reversed, and success rates are not memoised.
    let mut ascending = comparator(&success_rate(true), metrics);
    ascending(&a, &b);
    ascending(&a, &b);
    let ab = vec![json!(["successRate", tj(&a)]), json!(["successRate", tj(&b)])];
    assert_eq!(trace.drain(), [ab.clone(), ab].concat());
}

#[test]
fn execution_time_memo_is_per_process_call_and_survives_re_queue() {
    let tests = generate_tests(2);
    let (trace, metrics) = traced(defaults());
    let strategy = execution_time();
    let mut first = comparator(&strategy, metrics.clone());
    first(&tests[0], &tests[1]);
    first(&tests[1], &tests[0]);
    assert_eq!(trace.drain().len(), 2, "one lookup per distinct test");
    let mut second = comparator(&strategy, metrics);
    second(&tests[0], &tests[1]);
    assert_eq!(trace.drain().len(), 2, "a fresh memo per process call");
}

#[test]
fn random_order_draws_the_first_argument_first_and_shares_its_cache() {
    let tests = generate_tests(3);
    let trace = Trace::default();
    let hooks = ScriptedHooks::with_draws([7, 3, 5], Some(trace.clone()));
    let strategy = SortingStrategy::from_config(&Cnf::RandomOrder);
    let metrics: Arc<dyn MetricsLookup> = Arc::new(MetricsProviderStub::default());
    let mut first = strategy.process(metrics.clone(), hooks.clone());
    assert_eq!(first(&tests[0], &tests[1]), std::cmp::Ordering::Greater);
    // A second comparator from the same strategy reuses the keys already drawn.
    let mut second = strategy.process(metrics, hooks.clone());
    assert_eq!(second(&tests[1], &tests[2]), std::cmp::Ordering::Less);
    assert_eq!(*lock(&hooks.drawn), vec![7, 3, 5]);
    let SortingStrategy::RandomOrder { cache } = &strategy else {
        unreachable!()
    };
    let cache = lock(cache);
    assert_eq!((cache[&tests[0]], cache[&tests[1]], cache[&tests[2]]), (7, 3, 5));
}

#[test]
fn random_order_keys_a_failed_draw_0_and_the_replay_reports_it() {
    let tests = generate_tests(2);
    let hooks = ScriptedHooks::with_draws([-1], None);
    let strategy = SortingStrategy::from_config(&Cnf::RandomOrder);
    let mut compare = strategy.process(Arc::new(MetricsProviderStub::default()), hooks.clone());
    assert_eq!(compare(&tests[0], &tests[1]), std::cmp::Ordering::Less);
    assert_eq!(compare(&tests[1], &tests[0]), std::cmp::Ordering::Greater);
    assert!(hooks.verify_consumed().is_err());
}

#[test]
fn no_sorting_compares_equal_and_its_heap_order_is_not_fifo() {
    let tests = generate_tests(3);
    let strategy = SortingStrategy::from_config(&Cnf::NoSorting);
    let mut queue = TestQueue::new(comparator(&strategy, Arc::new(MetricsProviderStub::default())));
    queue.add_all(tests.clone());
    assert_eq!(queue.poll(), Some(tests[0].clone()));
    // swap_remove moves the last element to the root and equal keys keep it there.
    assert_eq!(queue.poll(), Some(tests[2].clone()));
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
    assert!(SortingStrategy::from_config(&Cnf::NoSorting).metrics_keys().is_empty());
    assert!(
        SortingStrategy::from_config(&Cnf::RandomOrder)
            .metrics_keys()
            .is_empty()
    );
}

// ---- oracle (a) ----------------------------------------------------------------------------------------

fn sorting_strategy(spec: &Value) -> SortingStrategy {
    match spec["type"].as_str() {
        Some("ExecutionTime") => SortingStrategy::from_config(&Cnf::ExecutionTime {
            percentile: double_of(&spec["percentile"]),
            time_limit: limit(),
        }),
        Some("SuccessRate") => SortingStrategy::from_config(&Cnf::SuccessRate {
            time_limit: limit(),
            ascending: spec["ascending"].as_bool().unwrap_or(false),
        }),
        Some("RandomOrder") => SortingStrategy::from_config(&Cnf::RandomOrder),
        Some("NoSorting") => SortingStrategy::from_config(&Cnf::NoSorting),
        other => panic!("unknown sorting {other:?}"),
    }
}

#[test]
fn sorting_oracle_table_replays() {
    for row in rows(
        include_str!("../../../fixtures/oracle/strategies/sorting.jsonl"),
        "sorting",
    ) {
        let case = row["case"].as_str().unwrap_or_default();
        let trace = Trace::default();
        let metrics = Arc::new(RecordingMetrics::from_json(&row["metrics"], trace.clone()));
        let draws: Vec<i32> = row["draws"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|d| d.as_i64())
            .map(|d| d as i32)
            .collect();
        let hooks = ScriptedHooks::with_draws(draws, Some(trace.clone()));
        let strategy = sorting_strategy(&row["strategy"]);
        let build = || {
            TestQueue::new(recording(
                strategy.process(metrics.clone(), hooks.clone()),
                trace.clone(),
            ))
        };
        let mut queue = build();
        let mut kotlin_draws = HashMap::new();
        let ops = row["ops"].as_array().cloned().unwrap_or_default();
        let steps = row["steps"].as_array().cloned().unwrap_or_default();
        assert_eq!(ops.len(), steps.len(), "{case}");
        for (i, (op, step)) in ops.iter().zip(&steps).enumerate() {
            let result = match op["op"].as_str() {
                Some("offer") => json!(queue.offer(test_of(&op["test"]))),
                Some("addAll") => json!(queue.add_all(tests_of(&op["tests"]))),
                Some("poll") => queue.poll().map_or(Value::Null, |t| tj(&t)),
                Some("remove") => json!(queue.remove(&test_of(&op["test"]))),
                Some("removeAll") => json!(queue.remove_all(&tests_of(&op["tests"]))),
                Some("setExecutionTime") => {
                    lock(&metrics.stub.execution_times).insert(test_of(&op["test"]), double_of(&op["value"]));
                    Value::Null
                }
                Some("setSuccessRate") => {
                    lock(&metrics.stub.success_rates).insert(test_of(&op["test"]), double_of(&op["value"]));
                    Value::Null
                }
                Some("reprocess") => {
                    queue = build();
                    Value::Null
                }
                other => panic!("unknown op {other:?}"),
            };
            let at = format!("{case}, step {i} {op}");
            assert_eq!(result, step["result"], "{at}");
            assert_eq!(tsj(&queue.iter().cloned().collect::<Vec<_>>()), step["array"], "{at}");
            // Kotlin records the drawing test too; the Rust hooks see only the value,
            // so the test's key is checked through the strategy's cache instead.
            let expected: Vec<Value> = step["trace"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| {
                    if e[0] == "draw" {
                        kotlin_draws.insert(test_of(&e[1]), e[2].as_i64().unwrap_or_default() as i32);
                        json!(["draw", e[2]])
                    } else {
                        e.clone()
                    }
                })
                .collect();
            assert_eq!(trace.drain(), expected, "{at}");
            if let SortingStrategy::RandomOrder { cache } = &strategy {
                assert_eq!(*lock(cache), kotlin_draws, "{at}");
            }
        }
        assert!(hooks.verify_consumed().is_ok(), "{case}");
    }
}

#[allow(dead_code)]
fn _queue_type_is_send(queue: TestQueue) -> impl Send {
    let _: &JavaPriorityQueue<Test, TestComparator> = &queue;
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
