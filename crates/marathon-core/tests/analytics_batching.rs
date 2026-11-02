//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/strategy/impl/batching/FixedSizeBatchingStrategyTest.kt (without history).
mod metrics_support;
mod strategy_support;
use marathon_config::{BatchingStrategyConfiguration, SortingStrategyConfiguration};
use marathon_core::{analytics::Analytics, model::*, strategy::*, wiring::MetricsProvider};
use metrics_support::*;
use std::sync::Arc;
use strategy_support::*;
fn strategy() -> BatchingStrategy {
    BatchingStrategy::from_config(&BatchingStrategyConfiguration::FixedSize {
        size: 5,
        duration_millis: Some(199000),
        percentile: Some(80.0),
        time_limit: Some(limit()),
        last_mile_length: 0,
    })
}
fn queue(tests: Vec<Test>, p: &Analytics) -> TestQueue {
    let mut q = TestQueue::new(
        SortingStrategy::from_config(&SortingStrategyConfiguration::NoSorting)
            .process(p.lookup(), ScriptedHooks::with_draws([], None)),
    );
    q.add_all(tests);
    q
}
fn batch(q: &mut TestQueue, p: &Analytics) -> Vec<Test> {
    strategy()
        .process(
            q,
            p.lookup().as_ref(),
            None,
            ScriptedHooks::with_draws([], None).as_ref(),
        )
        .unwrap()
        .tests
}
#[test]
fn without_history_and_with_the_default_duration_above_the_limit_every_batch_holds_one_test() {
    run(async {
        let h = Harness::new();
        let p = h.provider(
            Arc::new(Source {
                failure: true,
                ..Source::default()
            }),
            300000,
            None,
        );
        p.prefetch(&strategy().metrics_keys()).await.unwrap();
        let mut q = queue(generate_tests(20), &p);
        for _ in 0..3 {
            assert_eq!(batch(&mut q, &p).len(), 1);
        }
    });
}
#[test]
fn without_history_batches_follow_the_size_and_the_duration_limit_with_the_default_duration() {
    run(async {
        for (duration, count) in [(40000, 5), (60000, 4)] {
            let h = Harness::new();
            let p = h.provider(
                Arc::new(Source {
                    failure: true,
                    ..Source::default()
                }),
                duration,
                None,
            );
            p.prefetch(&strategy().metrics_keys()).await.unwrap();
            assert_eq!(batch(&mut queue(generate_tests(20), &p), &p).len(), count);
        }
    });
}
#[test]
fn without_history_batches_follow_the_durations_observed_in_this_run() {
    run(async {
        let h = Harness::new();
        let p = h.provider(
            Arc::new(Source {
                failure: true,
                ..Source::default()
            }),
            300000,
            Some(3),
        );
        p.prefetch(&strategy().metrics_keys()).await.unwrap();
        let mut q = queue(generate_tests(20), &p);
        let mut ran = vec![];
        for _ in 0..3 {
            let b = batch(&mut q, &p);
            assert_eq!(b.len(), 1);
            ran.extend(b);
        }
        p.observe_execution_times(
            &ran.iter()
                .map(|t| result(t, TestStatus::Passed, 50000))
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap();
        assert_eq!(batch(&mut q, &p).len(), 4);
    });
}
#[test]
fn without_history_a_test_that_was_slow_in_this_run_is_batched_alone() {
    run(async {
        let h = Harness::new();
        let p = h.provider(
            Arc::new(Source {
                failure: true,
                ..Source::default()
            }),
            300000,
            Some(3),
        );
        let t = generate_tests(4);
        p.observe_execution_times(
            &t.iter()
                .zip([250000, 20000, 20000, 20000])
                .map(|(t, d)| result(t, TestStatus::Passed, d))
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap();
        p.prefetch(&strategy().metrics_keys()).await.unwrap();
        let mut tests = vec![t[0].clone()];
        tests.extend(generate_tests_in("other", 10));
        let mut q = queue(tests, &p);
        assert_eq!(batch(&mut q, &p), vec![t[0].clone()]);
        assert_eq!(batch(&mut q, &p).len(), 3);
    });
}
