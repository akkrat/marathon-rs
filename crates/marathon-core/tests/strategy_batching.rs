//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/strategy/impl/batching/FixedSizeBatchingStrategyTest.kt
//! and impl/batching/{ClassName,Isolate}BatchingStrategyTest.kt, plus the replay of
//! fixtures/oracle/strategies/batching.jsonl (tools/oracle/strategy-probes).
//!
//! The Kotlin tests batch from a LinkedList; WP 3.2 P3 ports them onto a
//! JavaPriorityQueue with the no-sorting comparator, which keeps insertion
//! order for what they check. The 4 "without history" tests are WP 3.4a's.

mod strategy_support;

use jcompat::JavaPriorityQueue;
use marathon_config::schema::{BatchingStrategyConfiguration as Cnf, SortingStrategyConfiguration};
use marathon_core::{
    error::{Failure, VendorError, VendorResult},
    model::Test,
    strategy::{
        BatchingError, BatchingStrategy, MetricsKey, MetricsLookup, SortingStrategy, TestComparator, TestQueue,
    },
    vendor::{BundleRef, TestBundleIdentifier},
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use strategy_support::*;

fn fixed(size: i32) -> BatchingStrategy {
    BatchingStrategy::from_config(&Cnf::FixedSize {
        size,
        duration_millis: None,
        percentile: None,
        time_limit: None,
        last_mile_length: 0,
    })
}
fn no_sorting_queue(tests: Vec<Test>) -> TestQueue {
    let comparator = SortingStrategy::from_config(&SortingStrategyConfiguration::NoSorting).process(
        Arc::new(MetricsProviderStub::default()),
        ScriptedHooks::with_draws([], None),
    );
    let mut queue = TestQueue::new(comparator);
    queue.add_all(tests);
    queue
}
fn process(
    strategy: &BatchingStrategy,
    queue: &mut TestQueue,
    identifier: Option<&dyn TestBundleIdentifier>,
) -> Vec<Test> {
    let hooks = ScriptedHooks::with_draws([], None);
    match strategy.process(queue, &MetricsProviderStub::default(), identifier, hooks.as_ref()) {
        Ok(batch) => batch.tests,
        Err(e) => panic!("batching failed: {e:?}"),
    }
}

#[test]
fn test_class_named_strategy_should_create_10_batches_for_10_classes_with_batch_size_5() {
    let mut queue = no_sorting_queue(generate_class_group_tests(10, 5));
    assert_eq!(
        process(&BatchingStrategy::from_config(&Cnf::ClassName), &mut queue, None).len(),
        5
    );
}

#[test]
fn test_class_named_strategy_should_create_1_batch_for_1_classes_with_batch_size_5() {
    let mut queue = no_sorting_queue(generate_class_group_tests(1, 5));
    assert_eq!(
        process(&BatchingStrategy::from_config(&Cnf::ClassName), &mut queue, None).len(),
        5
    );
}

#[test]
fn test_batching_strategy_with_fixed_size_should_create_5_batches_for_50_tests_with_batch_size_10() {
    let mut queue = no_sorting_queue(generate_tests(50));
    assert_eq!(process(&fixed(10), &mut queue, None).len(), 10);
}

#[test]
fn test_batching_strategy_with_fixed_size_should_create_1_batch_for_10_tests_with_batch_size_10() {
    let mut queue = no_sorting_queue(generate_tests(10));
    assert_eq!(process(&fixed(10), &mut queue, None).len(), 10);
}

#[test]
fn test_batching_should_not_place_tests_from_different_bundles_in_the_same_batch() {
    let tests = generate_tests(2);
    let mut identifier = StubTestBundleIdentifier::default();
    identifier.put(&tests[0], bundle("1"));
    identifier.put(&tests[1], bundle("2"));
    let mut queue = no_sorting_queue(tests);
    assert_eq!(process(&fixed(2), &mut queue, Some(&identifier)).len(), 1);
}

#[test]
fn isolate_batching_strategy_test_should_return_batches_with_size_1() {
    let strategy = BatchingStrategy::from_config(&Cnf::Isolate);
    let mut queue = no_sorting_queue(generate_tests(50));
    assert_eq!(queue.len(), 50);
    for left in [49, 48, 47] {
        assert_eq!(process(&strategy, &mut queue, None).len(), 1);
        assert_eq!(queue.len(), left);
    }
}

// ---- rule pins -----------------------------------------------------------------------------------------

/// Polls in name order. An all-equal comparator would not poll in insertion order.
fn by_name(tests: Vec<Test>) -> JavaPriorityQueue<Test, fn(&Test, &Test) -> std::cmp::Ordering> {
    let mut queue =
        JavaPriorityQueue::new((|a: &Test, b: &Test| a.pkg.cmp(&b.pkg)) as fn(&Test, &Test) -> std::cmp::Ordering);
    queue.add_all(tests);
    queue
}
fn duration_limited(size: i32, millis: i64) -> BatchingStrategy {
    BatchingStrategy::from_config(&Cnf::FixedSize {
        size,
        duration_millis: Some(millis),
        percentile: Some(90.0),
        time_limit: Some(limit()),
        last_mile_length: 0,
    })
}

#[test]
fn fixed_size_counts_unbatchable_polls_against_the_size_and_re_adds_them() {
    let t = generate_tests(3);
    // A flaky copy of t0 is polled second: it takes a slot but not a place in the batch.
    let mut queue = by_name(vec![t[0].clone(), t[0].clone(), t[1].clone(), t[2].clone()]);
    let hooks = ScriptedHooks::with_draws([], None);
    let batch = fixed(3)
        .process(&mut queue, &MetricsProviderStub::default(), None, hooks.as_ref())
        .map(|b| b.tests);
    assert_eq!(batch, Ok(vec![t[0].clone(), t[1].clone()]));
    assert!(queue.iter().any(|q| *q == t[0]) && queue.len() == 2);
}

#[test]
fn fixed_size_adds_the_duration_of_unbatchable_polls_and_breaks_at_the_limit() {
    let t = generate_tests(3);
    let metrics = MetricsProviderStub::with_execution_times([(t[0].clone(), 3000.0), (t[1].clone(), 1000.0)]);
    // t0 (3000) + its copy (3000, unbatchable) reaches 6000: the batch is t0 alone.
    let mut queue = by_name(vec![t[0].clone(), t[0].clone(), t[1].clone(), t[2].clone()]);
    let hooks = ScriptedHooks::with_draws([], None);
    let batch = duration_limited(5, 6000).process(&mut queue, &metrics, None, hooks.as_ref());
    assert_eq!(batch.map(|b| b.tests), Ok(vec![t[0].clone()]));
    // At least one test is taken even when it alone exceeds the limit.
    let mut queue = by_name(vec![t[0].clone(), t[1].clone()]);
    let batch = duration_limited(5, 1).process(&mut queue, &metrics, None, hooks.as_ref());
    assert_eq!(batch.map(|b| b.tests), Ok(vec![t[0].clone()]));
}

#[test]
fn fixed_size_a_null_bundle_does_not_lock_the_batch() {
    let t = generate_tests(4);
    let mut identifier = StubTestBundleIdentifier::default();
    identifier.put(&t[1], bundle("a"));
    identifier.put(&t[2], bundle("b"));
    let mut queue = by_name(t.clone());
    let hooks = ScriptedHooks::with_draws([], None);
    let batch = fixed(4).process(
        &mut queue,
        &MetricsProviderStub::default(),
        Some(&identifier),
        hooks.as_ref(),
    );
    // t0 (null) leaves it open, t1 locks "a", t2 ("b") and t3 (null after the lock) are refused.
    assert_eq!(batch.map(|b| b.tests), Ok(vec![t[0].clone(), t[1].clone()]));
    assert_eq!(queue.len(), 2);
}

#[test]
fn fixed_size_last_mile_polls_a_single_test() {
    let strategy = BatchingStrategy::from_config(&Cnf::FixedSize {
        size: 10,
        duration_millis: None,
        percentile: None,
        time_limit: None,
        last_mile_length: 3,
    });
    let hooks = ScriptedHooks::with_draws([], None);
    let mut queue = by_name(generate_tests(3));
    let batch = strategy.process(&mut queue, &MetricsProviderStub::default(), None, hooks.as_ref());
    assert_eq!(batch.map(|b| b.tests.len()), Ok(3), "3 is not below the threshold");
    let mut queue = by_name(generate_tests(2));
    let batch = strategy.process(&mut queue, &MetricsProviderStub::default(), None, hooks.as_ref());
    assert_eq!(batch.map(|b| b.tests.len()), Ok(1));
}

#[test]
fn batching_rejects_an_empty_queue() {
    let hooks = ScriptedHooks::with_draws([], None);
    for strategy in [
        fixed(3),
        BatchingStrategy::from_config(&Cnf::ClassName),
        BatchingStrategy::from_config(&Cnf::Isolate),
    ] {
        let result = strategy.process(
            &mut by_name(Vec::new()),
            &MetricsProviderStub::default(),
            None,
            hooks.as_ref(),
        );
        assert_eq!(result, Err(BatchingError::EmptyQueue));
    }
}

#[test]
fn batch_id_is_allocated_after_the_last_queue_operation() {
    let t = generate_tests(3);
    let trace = Trace::default();
    let hooks = ScriptedHooks::with_draws([], Some(trace.clone()));
    let comparator = recording(Box::new(|_, _| std::cmp::Ordering::Equal), trace.clone());
    let mut queue = TestQueue::new(comparator);
    queue.add_all(vec![t[0].clone(), t[0].clone(), t[1].clone()]);
    trace.drain();
    let batch = fixed(3).process(&mut queue, &MetricsProviderStub::default(), None, hooks.as_ref());
    assert_eq!(batch.map(|b| b.id.to_string_lossy()), Ok("batch-1".to_owned()));
    let events = trace.drain();
    // The re-add of the unbatchable copy compares, then the id is drawn.
    assert_eq!(events.last(), Some(&json!(["allocateId"])));
    assert!(events.len() > 1);
}

struct FailingIdentifier;
impl TestBundleIdentifier for FailingIdentifier {
    fn identify(&self, _test: &Test) -> VendorResult<Option<BundleRef>> {
        Err(VendorError::Other(Failure {
            class: "java.lang.IllegalStateException".into(),
            message: None,
            cause: None,
            stacktrace: None,
        }))
    }
}

#[test]
fn a_bundle_identification_failure_propagates_and_loses_the_polled_test() {
    let hooks = ScriptedHooks::with_draws([], None);
    let mut queue = by_name(generate_tests(2));
    let result = fixed(2).process(
        &mut queue,
        &MetricsProviderStub::default(),
        Some(&FailingIdentifier),
        hooks.as_ref(),
    );
    assert!(matches!(result, Err(BatchingError::Bundle(VendorError::Other(_)))));
    assert_eq!(queue.len(), 1);
}

#[test]
fn batching_declares_the_metrics_keys_it_can_touch() {
    assert_eq!(
        duration_limited(5, 1).metrics_keys(),
        vec![MetricsKey::ExecutionTime {
            percentile: 90.0,
            limit: limit()
        }]
    );
    // F2 can null timeLimit; then the batcher never looks durations up.
    let no_limit = BatchingStrategy::from_config(&Cnf::FixedSize {
        size: 5,
        duration_millis: Some(1),
        percentile: Some(90.0),
        time_limit: None,
        last_mile_length: 0,
    });
    assert!(no_limit.metrics_keys().is_empty());
    assert!(fixed(5).metrics_keys().is_empty());
    assert!(BatchingStrategy::from_config(&Cnf::ClassName).metrics_keys().is_empty());
}

// ---- oracle (b) ----------------------------------------------------------------------------------------

fn batching_strategy(spec: &Value) -> BatchingStrategy {
    match spec["type"].as_str() {
        Some("FixedSize") => BatchingStrategy::from_config(&Cnf::FixedSize {
            size: spec["size"].as_i64().unwrap_or_default() as i32,
            duration_millis: spec["durationMillis"].as_i64(),
            percentile: spec["percentile"].as_str().map(|_| double_of(&spec["percentile"])),
            time_limit: (spec["timeLimit"] == true).then(limit),
            last_mile_length: spec["lastMileLength"].as_i64().unwrap_or_default() as i32,
        }),
        Some("ClassName") => BatchingStrategy::from_config(&Cnf::ClassName),
        Some("Isolate") => BatchingStrategy::from_config(&Cnf::Isolate),
        other => panic!("unknown batching {other:?}"),
    }
}

#[test]
fn batching_oracle_table_replays() {
    for row in rows(
        include_str!("../../../fixtures/oracle/strategies/batching.jsonl"),
        "batching",
    ) {
        let case = row["case"].as_str().unwrap_or_default();
        let trace = Trace::default();
        let metrics = Arc::new(RecordingMetrics::from_json(&row["metrics"], trace.clone()));
        let sorting = match row["sorting"].as_str() {
            Some("NoSorting") => SortingStrategyConfiguration::NoSorting,
            _ => SortingStrategyConfiguration::ExecutionTime {
                percentile: 90.0,
                time_limit: limit(),
            },
        };
        let hooks = ScriptedHooks::with_draws([], None);
        let lookup: Arc<dyn MetricsLookup> = metrics.clone();
        let comparator: TestComparator = SortingStrategy::from_config(&sorting).process(lookup, hooks.clone());
        let mut queue = TestQueue::new(recording(comparator, trace.clone()));
        let identifier = row["bundles"].as_array().map(|entries| {
            // One bundle object per id: bundle equality is identity.
            let mut by_id: HashMap<String, BundleRef> = HashMap::new();
            StubTestBundleIdentifier {
                bundles: entries
                    .iter()
                    .map(|e| {
                        let shared = e[1]
                            .as_str()
                            .map(|id| by_id.entry(id.to_owned()).or_insert_with(|| bundle(id)).clone());
                        (test_of(&e[0]), shared)
                    })
                    .collect(),
                trace: Some(trace.clone()),
            }
        });
        queue.add_all(tests_of(&row["queue"]));
        assert_eq!(Value::Array(trace.drain()), row["setup"], "{case}: setup");
        let strategy = batching_strategy(&row["strategy"]);
        let batches = row["batches"].as_array().cloned().unwrap_or_default();
        for (i, expected) in batches.iter().enumerate() {
            let identifier = identifier.as_ref().map(|i| i as &dyn TestBundleIdentifier);
            let batch = match strategy.process(&mut queue, metrics.as_ref(), identifier, hooks.as_ref()) {
                Ok(batch) => batch,
                Err(e) => panic!("{case}, batch {i}: {e:?}"),
            };
            let at = format!("{case}, batch {i}");
            assert_eq!(tsj(&batch.tests), expected["tests"], "{at}");
            assert_eq!(
                tsj(&queue.iter().cloned().collect::<Vec<_>>()),
                expected["array"],
                "{at}"
            );
            assert_eq!(Value::Array(trace.drain()), expected["trace"], "{at}");
        }
        // The probe stops at an empty queue or at its call count.
        assert!(queue.is_empty() || !batches.is_empty(), "{case}");
    }
}
