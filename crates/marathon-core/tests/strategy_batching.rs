//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/strategy/impl/batching/FixedSizeBatchingStrategyTest.kt
//! and impl/batching/{ClassName,Isolate}BatchingStrategyTest.kt, plus the replay of
//! fixtures/oracle/strategies/batching.jsonl (tools/oracle/strategy-probes).
//!
//! The Kotlin tests batch from a LinkedList; WP 3.2 P3 ports them onto a
//! JavaPriorityQueue with the no-sorting comparator, which keeps insertion
//! order for what they check. The 4 "without history" tests are WP 3.4a's.
//! `D50_ROWS` names the JVM rows that stay witnesses of C33.

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
fn fixed_size_counts_only_accepted_tests_against_the_size_and_re_adds_the_rest() {
    let t = generate_tests(4);
    // A flaky copy of t0 is polled second: it takes no slot, so t2 still fits (D50).
    let mut queue = by_name(vec![
        t[0].clone(),
        t[0].clone(),
        t[1].clone(),
        t[2].clone(),
        t[3].clone(),
    ]);
    let hooks = ScriptedHooks::with_draws([], None);
    let batch = fixed(3)
        .process(&mut queue, &MetricsProviderStub::default(), None, hooks.as_ref())
        .map(|b| b.tests.clone());
    assert_eq!(batch, Ok(vec![t[0].clone(), t[1].clone(), t[2].clone()]));
    assert_eq!(queue.len(), 2);
    assert_eq!(queue.iter().filter(|q| **q == t[0]).count(), 1);
}

#[test]
fn fixed_size_adds_only_accepted_durations_and_breaks_at_the_limit() {
    let t = generate_tests(4);
    let metrics = MetricsProviderStub::with_execution_times([
        (t[0].clone(), 3000.0),
        (t[1].clone(), 1000.0),
        (t[2].clone(), 2000.0),
        (t[3].clone(), 1.0),
    ]);
    // t0's copy adds nothing (D50): 3000 + 1000 + 2000 reaches 6000 at t2.
    let mut queue = by_name(vec![
        t[0].clone(),
        t[0].clone(),
        t[1].clone(),
        t[2].clone(),
        t[3].clone(),
    ]);
    let hooks = ScriptedHooks::with_draws([], None);
    let batch = duration_limited(5, 6000).process(&mut queue, &metrics, None, hooks.as_ref());
    assert_eq!(
        batch.map(|b| b.tests.clone()),
        Ok(vec![t[0].clone(), t[1].clone(), t[2].clone()])
    );
    // At least one test is taken even when it alone exceeds the limit.
    let mut queue = by_name(vec![t[0].clone(), t[1].clone()]);
    let batch = duration_limited(5, 1).process(&mut queue, &metrics, None, hooks.as_ref());
    assert_eq!(batch.map(|b| b.tests.clone()), Ok(vec![t[0].clone()]));
}

#[test]
fn fixed_size_asks_no_duration_for_a_rejected_test() {
    let t = generate_tests(2);
    let trace = Trace::default();
    let recorded = oracle_rows()
        .first()
        .map(|row| row["metrics"].clone())
        .unwrap_or_default();
    let metrics = RecordingMetrics::from_json(&recorded, trace.clone());
    let mut queue = by_name(vec![t[0].clone(), t[0].clone(), t[1].clone()]);
    let hooks = ScriptedHooks::with_draws([], None);
    let batch = duration_limited(5, 1_000_000).process(&mut queue, &metrics, None, hooks.as_ref());
    assert_eq!(batch.map(|b| b.tests.clone()), Ok(vec![t[0].clone(), t[1].clone()]));
    assert_eq!(
        trace.drain(),
        vec![json!(["executionTime", tj(&t[0])]), json!(["executionTime", tj(&t[1])])]
    );
}

/// `pkg` order; the first test is bundle "a", the rest bundle "b", which is what makes a scan expensive.
fn one_then_many(count: usize) -> (Vec<Test>, StubTestBundleIdentifier) {
    let t = generate_tests(count);
    let (a, b) = (bundle("a"), bundle("b"));
    let mut identifier = StubTestBundleIdentifier::default();
    for (i, test) in t.iter().enumerate() {
        identifier.put(test, if i == 0 { a.clone() } else { b.clone() });
    }
    (t, identifier)
}

#[test]
fn fixed_size_stops_after_two_rejected_polls_per_slot() {
    let (t, mut identifier) = one_then_many(100);
    let trace = Trace::default();
    identifier.trace = Some(trace.clone());
    let mut queue = by_name(t.clone());
    let hooks = ScriptedHooks::with_draws([], None);
    let batch = fixed(3).process(
        &mut queue,
        &MetricsProviderStub::default(),
        Some(&identifier),
        hooks.as_ref(),
    );
    assert_eq!(batch.map(|b| b.tests.clone()), Ok(vec![t[0].clone()]));
    assert_eq!(queue.len(), 99);
    // The accepted test and 3 x 2 rejected ones, out of 99 that could be rejected.
    assert_eq!(trace.drain().len(), 7);
}

#[test]
fn fixed_size_of_integer_max_value_does_not_overflow_the_rejected_limit() {
    let (t, identifier) = one_then_many(5);
    let mut queue = by_name(t.clone());
    let hooks = ScriptedHooks::with_draws([], None);
    let batch = fixed(i32::MAX).process(
        &mut queue,
        &MetricsProviderStub::default(),
        Some(&identifier),
        hooks.as_ref(),
    );
    assert_eq!(batch.map(|b| b.tests.clone()), Ok(vec![t[0].clone()]));
    assert_eq!(queue.len(), 4);
}

/// Bundle by a test's position, keyed on `pkg<n>`, mixing `count` bundles in priority order.
fn mixed_bundles(tests: &[Test], count: usize) -> StubTestBundleIdentifier {
    let bundles: Vec<_> = (0..count).map(|i| bundle(&i.to_string())).collect();
    let mut identifier = StubTestBundleIdentifier::default();
    for (i, test) in tests.iter().enumerate() {
        identifier.put(test, bundles[i % count].clone());
    }
    identifier
}

#[test]
fn rejected_tests_keep_every_tests_place_under_a_strict_order() {
    // 26 tests: pkg names sort as numbers only below 10, so rank them by zero-padded name.
    let t: Vec<Test> = (0..26).map(|i| generate_test(&format!("p{i:02}"), "c", "m")).collect();
    let identifier = mixed_bundles(&t, 2);
    let mut queue = by_name(t.clone());
    let mut per_bundle: [Vec<Test>; 2] = [Vec::new(), Vec::new()];
    let mut sizes = Vec::new();
    while !queue.is_empty() {
        let hooks = ScriptedHooks::with_draws([], None);
        let batch = fixed(5).process(
            &mut queue,
            &MetricsProviderStub::default(),
            Some(&identifier),
            hooks.as_ref(),
        );
        let tests = batch.map(|b| b.tests.clone()).unwrap_or_default();
        sizes.push(tests.len());
        for test in tests {
            let rank = t.iter().position(|it| *it == test).unwrap_or_default();
            per_bundle[rank % 2].push(test);
        }
    }
    assert_eq!(sizes, [5, 5, 5, 5, 3, 3]);
    let expected = |rest: usize| -> Vec<Test> { t.iter().skip(rest).step_by(2).cloned().collect() };
    assert_eq!(per_bundle[0], expected(0));
    assert_eq!(per_bundle[1], expected(1));
}

#[test]
fn rejected_tests_leave_tests_never_polled_in_their_order_under_ties() {
    let t = generate_tests(12);
    // Under ties a poll takes the root and moves the array's last test there, so after t0 the polls come from
    // the end: four of bundle "b", rejected, then t7 and t6 of bundle "a".
    let (a, b) = (bundle("a"), bundle("b"));
    let mut identifier = StubTestBundleIdentifier::default();
    for (i, test) in t.iter().enumerate() {
        identifier.put(test, if i < 8 { a.clone() } else { b.clone() });
    }
    let mut queue = no_sorting_queue(t.clone());
    assert_eq!(
        process(&fixed(3), &mut queue, Some(&identifier)),
        vec![t[0].clone(), t[7].clone(), t[6].clone()],
    );
    let array: Vec<Test> = queue.iter().cloned().collect();
    // The rejected tests go to the end, in the order they were polled. Each poll moved the array's
    // last test to the root, which is the only move: t1 to t5 stay as they were.
    assert_eq!(
        array,
        vec![
            t[5].clone(),
            t[1].clone(),
            t[2].clone(),
            t[3].clone(),
            t[4].clone(),
            t[11].clone(),
            t[10].clone(),
            t[9].clone(),
            t[8].clone(),
        ]
    );
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
    assert_eq!(batch.map(|b| b.tests.clone()), Ok(vec![t[0].clone(), t[1].clone()]));
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
    assert_eq!(batch.map(|b| b.id.to_owned()), Ok("batch-1".to_owned()));
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

/// C33's rule as Kotlin has it, which the JVM rows of D50_ROWS recorded: every poll takes a slot and adds its
/// duration. It lives only here, to show that this rule alone separates those rows from D50.
fn kotlin_fixed_size(
    spec: &Value,
    queue: &mut TestQueue,
    metrics: &dyn MetricsLookup,
    identifier: Option<&dyn TestBundleIdentifier>,
) -> (Vec<Test>, usize) {
    let last_mile = spec["lastMileLength"].as_i64().unwrap_or_default();
    if (queue.len() as i64) < last_mile {
        return (queue.poll().into_iter().collect(), 0);
    }
    let duration = spec["durationMillis"]
        .as_i64()
        .filter(|_| spec["timeLimit"] == true && spec["percentile"].is_string());
    let mut counter = 0;
    let mut sum = 0.0;
    let mut unbatchable: Vec<Test> = Vec::new();
    let mut result: Vec<Test> = Vec::new();
    let mut locked = None;
    while counter < spec["size"].as_i64().unwrap_or_default() {
        let Some(item) = queue.poll() else { break };
        counter += 1;
        let item_bundle = identifier.and_then(|i| i.identify(&item).ok().flatten());
        if result.contains(&item) || (locked.is_some() && locked != item_bundle) {
            unbatchable.push(item.clone());
        } else {
            locked = item_bundle;
            result.push(item.clone());
        }
        if let Some(millis) = duration {
            sum += metrics.execution_time(&item, double_of(&spec["percentile"]), limit());
            if sum >= millis as f64 {
                break;
            }
        }
    }
    let rejected = unbatchable.len();
    queue.add_all(unbatchable);
    (result, rejected)
}

#[derive(Clone, Copy, PartialEq)]
enum Rule {
    D50,
    Kotlin,
}

/// Replays one row with `rule`: Ok with the polls rejected, or the first difference from the recording.
fn replay_row(row: &Value, rule: Rule) -> Result<usize, String> {
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
    let compare = |what: &str, actual: Value, expected: &Value| {
        if actual == *expected {
            Ok(())
        } else {
            Err(format!("{case}: {what}\n  rust {actual}\n  jvm  {expected}"))
        }
    };
    compare("setup", Value::Array(trace.drain()), &row["setup"])?;
    let strategy = batching_strategy(&row["strategy"]);
    let batches = row["batches"].as_array().cloned().unwrap_or_default();
    let mut rejected = 0;
    for (i, expected) in batches.iter().enumerate() {
        let identifier = identifier.as_ref().map(|i| i as &dyn TestBundleIdentifier);
        let tests = match rule {
            Rule::D50 => match strategy.process(&mut queue, metrics.as_ref(), identifier, hooks.as_ref()) {
                Ok(batch) => batch.tests,
                Err(e) => return Err(format!("{case}, batch {i}: {e:?}")),
            },
            Rule::Kotlin => {
                let (tests, count) = kotlin_fixed_size(&row["strategy"], &mut queue, metrics.as_ref(), identifier);
                rejected += count;
                tests
            }
        };
        compare(&format!("batch {i} tests"), tsj(&tests), &expected["tests"])?;
        let array = queue.iter().cloned().collect::<Vec<_>>();
        compare(&format!("batch {i} array"), tsj(&array), &expected["array"])?;
        compare(
            &format!("batch {i} trace"),
            Value::Array(trace.drain()),
            &expected["trace"],
        )?;
    }
    // The probe stops at an empty queue or at its call count.
    if !(queue.is_empty() || !batches.is_empty()) {
        return Err(format!("{case}: no batches"));
    }
    Ok(rejected)
}

/// D50's allowance: the JVM rows in which a rejected poll took a slot. Each stays a witness of C33 through
/// `kotlin_fixed_size`; every other row must match D50's code exactly.
const D50_ROWS: [&str; 7] = [
    "fixed size 4 with flaky copies [NoSorting]",
    "fixed size 5 with mixed and null bundles [NoSorting]",
    "fixed size 5 with duration and bundles [NoSorting]",
    "fixed size 4 with flaky copies [ExecutionTime]",
    "fixed size 5 with mixed and null bundles [ExecutionTime]",
    "fixed size 5 with duration 7000 and flaky copies [ExecutionTime]",
    "fixed size 5 with duration and bundles [ExecutionTime]",
];

fn is_d50_row(case: &str) -> bool {
    D50_ROWS.contains(&case)
}

fn oracle_rows() -> Vec<Value> {
    rows(
        include_str!("../../../fixtures/oracle/strategies/batching.jsonl"),
        "batching",
    )
}

#[test]
fn batching_oracle_table_replays() {
    let mut replayed = 0;
    for row in oracle_rows() {
        if is_d50_row(row["case"].as_str().unwrap_or_default()) {
            continue;
        }
        replayed += 1;
        if let Err(difference) = replay_row(&row, Rule::D50) {
            panic!("{difference}");
        }
    }
    assert!(replayed > 0);
}

#[test]
fn the_d50_allowance_covers_only_rows_where_a_rejected_poll_took_a_slot() {
    let mut allowed = 0;
    for row in oracle_rows() {
        let case = row["case"].as_str().unwrap_or_default();
        if is_d50_row(case) {
            allowed += 1;
            // The recording is C33's rule, byte for byte, and D50's code differs from it.
            match replay_row(&row, Rule::Kotlin) {
                Ok(rejected) => assert!(rejected > 0, "{case}: nothing rejected, so no allowance is due"),
                Err(difference) => panic!("{difference}"),
            }
            assert!(replay_row(&row, Rule::D50).is_err(), "{case}: no longer differs");
        }
    }
    assert_eq!(allowed, D50_ROWS.len());
    // Rejections that end no batch early need no allowance: the queue ran out first.
    let rows = oracle_rows();
    let leading = rows
        .iter()
        .find(|r| r["case"] == "fixed size 5 with leading null bundles [NoSorting]");
    let leading = leading.cloned().unwrap_or_default();
    assert_eq!(replay_row(&leading, Rule::Kotlin), Ok(3));
    assert_eq!(replay_row(&leading, Rule::D50), Ok(0));
}

#[test]
fn the_d50_allowance_hides_no_other_change() {
    let rows = oracle_rows();
    let row_named = |case: &str| rows.iter().find(|r| r["case"] == case).cloned().unwrap_or_default();
    // A difference in a row outside the allowance is reported.
    let mut outside = row_named("fixed size 3 over 8 [ExecutionTime]");
    outside["batches"][0]["trace"][0] = json!(["compare", "tampered"]);
    assert!(replay_row(&outside, Rule::D50).is_err());
    // A row inside it still answers to the recording, through Kotlin's rule.
    let mut inside = row_named("fixed size 4 with flaky copies [ExecutionTime]");
    assert!(replay_row(&inside, Rule::Kotlin).is_ok());
    inside["batches"][0]["array"][0] = json!(["tampered", "tampered", "tampered"]);
    assert!(replay_row(&inside, Rule::Kotlin).is_err());
}

#[test]
fn d50_fills_the_slot_a_flaky_copy_took_in_the_jvm_row() {
    let rows = oracle_rows();
    let row = rows
        .iter()
        .find(|r| r["case"] == "fixed size 4 with flaky copies [NoSorting]")
        .cloned()
        .unwrap_or_default();
    let tests_in = |batch: &Value| batch["tests"].as_array().map_or(0, Vec::len);
    assert_eq!(
        tests_in(&row["batches"][0]),
        3,
        "the witness: a copy took the fourth slot"
    );
    let mut queue = no_sorting_queue(tests_of(&row["queue"]));
    assert_eq!(process(&batching_strategy(&row["strategy"]), &mut queue, None).len(), 4);
}

/// One flaky copy of each test, next to it in priority order: Kotlin's rule against D50's, in batches.
#[test]
fn flaky_copies_cost_fewer_batches_than_under_c33() {
    let t: Vec<Test> = (0..100).map(|i| generate_test(&format!("p{i:03}"), "c", "m")).collect();
    let copies = || -> Vec<Test> { t.iter().flat_map(|test| [test.clone(), test.clone()]).collect() };
    let queue_of = |tests| {
        let comparator: TestComparator = Box::new(|a: &Test, b: &Test| a.pkg.cmp(&b.pkg));
        let mut queue = TestQueue::new(comparator);
        queue.add_all(tests);
        queue
    };
    let metrics = MetricsProviderStub::default();
    let (mut kotlin, mut kotlin_batches) = (queue_of(copies()), 0);
    let spec = json!({"size": 10, "lastMileLength": 0});
    while !kotlin.is_empty() {
        kotlin_fixed_size(&spec, &mut kotlin, &metrics, None);
        kotlin_batches += 1;
    }
    let (mut rust, mut rust_batches) = (queue_of(copies()), 0);
    while !rust.is_empty() {
        process(&fixed(10), &mut rust, None);
        rust_batches += 1;
    }
    eprintln!("flaky copies, 200 entries, size 10: kotlin {kotlin_batches}, d50 {rust_batches}");
    assert_eq!((kotlin_batches, rust_batches), (30, 20));
}

/// With nothing rejected, D50's code performs Kotlin's operations: batches, array and every comparison.
#[test]
fn batches_with_nothing_rejected_match_kotlins_rule_operation_by_operation() {
    let mut state = 0x2545_f491u64;
    let mut next = move |bound: u64| {
        state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (state >> 33) % bound
    };
    for _ in 0..200 {
        let size = 1 + next(12) as i64;
        let count = 1 + next(60) as usize;
        let tests = generate_tests(count);
        // Few distinct ranks, so ties occur often.
        let ranks: HashMap<Test, u64> = tests.iter().map(|t| (t.clone(), next(8))).collect();
        let build = |trace: &Trace| {
            let ranks = ranks.clone();
            let comparator: TestComparator = Box::new(move |a, b| ranks[a].cmp(&ranks[b]));
            let mut queue = TestQueue::new(recording(comparator, trace.clone()));
            queue.add_all(tests.clone());
            queue
        };
        let (rust_trace, kotlin_trace) = (Trace::default(), Trace::default());
        let (mut rust, mut kotlin) = (build(&rust_trace), build(&kotlin_trace));
        let spec = json!({"size": size, "lastMileLength": 0});
        let metrics = MetricsProviderStub::default();
        while !kotlin.is_empty() {
            let (expected, rejected) = kotlin_fixed_size(&spec, &mut kotlin, &metrics, None);
            assert_eq!(rejected, 0);
            let hooks = ScriptedHooks::with_draws([], None);
            let batch = fixed(size as i32).process(&mut rust, &metrics, None, hooks.as_ref());
            assert_eq!(batch.map(|b| b.tests.clone()), Ok(expected));
            assert_eq!(rust.iter().collect::<Vec<_>>(), kotlin.iter().collect::<Vec<_>>());
            assert_eq!(rust_trace.drain(), kotlin_trace.drain());
        }
    }
}
