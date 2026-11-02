//! Port of core/src/test/kotlin/com/malinskiy/marathon/analytics/metrics/remote/influx/MetricsProviderImplTest.kt
//! and StartupTimingsTest.kt (analytics query timings).
mod metrics_support;
mod strategy_support;
use jcompat::logging::Level;
use marathon_core::{model::*, strategy::MetricsKey, wiring::MetricsProvider};
use metrics_support::*;
use std::sync::Arc;
use strategy_support::*;

#[test]
fn test_execution_time_from_cache() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source::default());
        let test = default_test();
        *source.times.lock().unwrap() = vec![time_row(&test, 100.5), time_row(&generate_tests(1)[0], 80.0)];
        let provider = h.provider(source.clone(), 300000, None);
        for _ in 0..2 {
            assert_eq!(provider.execution_time(&test, 90.0, limit()).await.unwrap(), 100.5);
        }
        assert_eq!(*source.calls.lock().unwrap(), vec![key(90.0)]);
    });
}
#[test]
fn test_execution_time_from_missing_cache() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source::default());
        let test = default_test();
        let provider = h.provider(source.clone(), 300000, None);
        for (p, value, limit) in [
            (
                80.0,
                900.0,
                jcompat::jtime::instant_of(limit().seconds() - 60, 0).unwrap(),
            ),
            (90.0, 1500.0, limit()),
        ] {
            *source.times.lock().unwrap() = vec![time_row(&test, value)];
            assert_eq!(provider.execution_time(&test, p, limit).await.unwrap(), value);
        }
        assert_eq!(source.calls.lock().unwrap().len(), 2);
    });
}
#[test]
fn test_success_rate_from_cache() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source::default());
        let test = default_test();
        *source.rates.lock().unwrap() = vec![rate_row(&test, 90.0)];
        let provider = h.provider(source.clone(), 300000, None);
        for _ in 0..2 {
            assert_eq!(provider.success_rate(&test, limit()).await.unwrap(), 90.0);
        }
        assert_eq!(source.calls.lock().unwrap().len(), 1);
    });
}
#[test]
fn test_success_rate_with_missing_cache() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source::default());
        let test = default_test();
        let provider = h.provider(source.clone(), 300000, None);
        for (value, limit) in [
            (90.0, jcompat::jtime::instant_of(limit().seconds() - 60, 0).unwrap()),
            (80.0, limit()),
        ] {
            *source.rates.lock().unwrap() = vec![rate_row(&test, value)];
            assert_eq!(provider.success_rate(&test, limit).await.unwrap(), value);
        }
        assert_eq!(source.calls.lock().unwrap().len(), 2);
    });
}
#[test]
fn tests_without_history_take_the_default_duration_if_observed_durations_are_not_enabled() {
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
        let t = generate_tests(2);
        p.observe_execution_times(&[result(&t[0], TestStatus::Passed, 20000)])
            .await
            .unwrap();
        for test in t {
            assert_eq!(p.execution_time(&test, 80.0, limit()).await.unwrap(), 300000.0);
        }
    });
}
#[test]
fn tests_without_history_are_estimated_from_this_run_once_enough_tests_have_passed_or_failed() {
    run(async {
        let h = Harness::new();
        let p = h.provider(
            Arc::new(Source {
                failure: true,
                ..Source::default()
            }),
            300000,
            Some(2),
        );
        let t = generate_tests(3);
        assert_eq!(p.execution_time(&t[2], 80.0, limit()).await.unwrap(), 300000.0);
        p.observe_execution_times(&[result(&t[0], TestStatus::Passed, 50000)])
            .await
            .unwrap();
        assert_eq!(p.lookup().execution_time(&t[0], 80.0, limit()), 50000.0);
        assert_eq!(p.lookup().execution_time(&t[2], 80.0, limit()), 300000.0);
        p.observe_execution_times(&[result(&t[1], TestStatus::Failure, 30000)])
            .await
            .unwrap();
        assert_eq!(p.lookup().execution_time(&t[1], 80.0, limit()), 30000.0);
        assert_eq!(p.lookup().execution_time(&t[2], 80.0, limit()), 40000.0);
    });
}
#[test]
fn a_test_that_ran_in_this_run_is_expected_to_take_the_percentile_of_its_own_durations() {
    run(async {
        let h = Harness::new();
        let p = h.provider(Arc::new(Source::default()), 300000, Some(10));
        let t = default_test();
        p.observe_execution_times(&[
            result(&t, TestStatus::Failure, 10000),
            result(&t, TestStatus::Failure, 100000),
            result(&t, TestStatus::Passed, 40000),
        ])
        .await
        .unwrap();
        for (q, expected) in [(50.0, 40000.0), (80.0, 100000.0), (0.0, 10000.0)] {
            assert_eq!(p.execution_time(&t, q, limit()).await.unwrap(), expected);
        }
    });
}
#[test]
fn history_takes_precedence_over_durations_observed_in_this_run() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source::default());
        let t = generate_tests(3);
        *source.times.lock().unwrap() = vec![time_row(&t[0], 7000.0)];
        let p = h.provider(source, 300000, Some(1));
        p.observe_execution_times(&[
            result(&t[0], TestStatus::Passed, 90000),
            result(&t[1], TestStatus::Passed, 20000),
        ])
        .await
        .unwrap();
        for (test, value) in t.iter().zip([7000.0, 20000.0, 55000.0]) {
            assert_eq!(p.execution_time(test, 80.0, limit()).await.unwrap(), value);
        }
    });
}
#[test]
fn only_passed_and_failed_results_are_observed() {
    run(async {
        let h = Harness::new();
        let p = h.provider(Arc::new(Source::default()), 300000, Some(1));
        let t = generate_tests(4);
        p.observe_execution_times(&[
            result(&t[0], TestStatus::Ignored, 1000),
            result(&t[1], TestStatus::Incomplete, 1),
            result(&t[2], TestStatus::AssumptionFailure, 2000),
        ])
        .await
        .unwrap();
        for test in t {
            assert_eq!(p.execution_time(&test, 80.0, limit()).await.unwrap(), 300000.0);
        }
    });
}
async fn coverage_case(count: usize, min: Option<i32>, level: Level, text: &str) {
    let h = Harness::new();
    let source = Arc::new(Source::default());
    let mut t = generate_tests(3);
    *source.times.lock().unwrap() = t[..count].iter().map(|t| time_row(t, 7000.0)).collect();
    t.push(t[0].clone());
    let p = h.provider(source, 300000, min);
    p.report_execution_time_coverage(&t, 80.0, limit(), &"the batch duration limit".into())
        .await
        .unwrap();
    let events = h.logs.take();
    let event = events.last().unwrap();
    assert_eq!(event.level, level);
    assert!(event.message.to_string_lossy().contains(text));
}
#[test]
fn coverage_is_a_warning_if_most_tests_have_no_history() {
    run(coverage_case(
        1,
        None,
        Level::Warn,
        "2 tests are expected to take the default 300000 ms",
    ));
}
#[test]
fn coverage_is_info_if_most_tests_have_history() {
    run(coverage_case(
        2,
        Some(20),
        Level::Info,
        "1 tests are estimated from this run",
    ));
}
#[test]
fn analytics_queries_log_their_duration_and_outcome() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source {
            failure: true,
            ..Source::default()
        });
        let p = h.provider(source.clone(), 300000, None);
        p.prefetch(&[key(80.0), MetricsKey::SuccessRate { limit: limit() }])
            .await
            .unwrap();
        assert_eq!(*h.clock.delays.lock().unwrap(), vec![50, 50, 50, 50]);
        let events = h.logs.take();
        let timings: Vec<_> = events.iter().filter(|e| e.logger == "Timings").collect();
        assert_eq!(timings.len(), 2);
        for e in timings {
            assert_eq!(e.level, Level::Info);
            assert!(e.message.to_string_lossy().ends_with("failed in 100 ms"));
        }
        p.prefetch(&[key(80.0)]).await.unwrap();
        assert!(h.logs.take().is_empty());
        assert_eq!(source.calls.lock().unwrap().len(), 6);
    });
}

#[test]
fn nan_payloads_share_a_key_signed_zeroes_do_not_and_empty_coverage_skips_fetch() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source::default());
        let p = h.provider(source.clone(), 300000, None);
        p.report_execution_time_coverage(&[], 90.0, limit(), &"empty".into())
            .await
            .unwrap();
        assert!(source.calls.lock().unwrap().is_empty());
        assert!(h.logs.take().is_empty());
        p.prefetch(&[
            key(f64::NAN),
            key(f64::from_bits(0x7ff0000000000001)),
            key(-0.0),
            key(0.0),
        ])
        .await
        .unwrap();
        assert_eq!(source.calls.lock().unwrap().len(), 3);
    });
}
#[test]
fn huge_default_duration_only_fails_when_its_fallback_is_used() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source::default());
        let t = generate_tests(2);
        *source.times.lock().unwrap() = vec![time_row(&t[0], 100.0)];
        let mut defaults = defaults(0, Some(1));
        defaults.duration = jcompat::jtime::duration_of(i64::MAX, 0).unwrap();
        let p = marathon_core::analytics::Analytics::new(
            marathon_core::analytics::MetricsProviderImpl::new(source, defaults, h.context.clone()).unwrap(),
        );
        assert_eq!(p.execution_time(&t[0], 80.0, limit()).await.unwrap(), 100.0);
        assert!(matches!(
            p.execution_time(&t[1], 80.0, limit()).await,
            Err(marathon_core::error::OperationError::Failed(_))
        ));
        p.observe_execution_times(&[result(&t[0], TestStatus::Passed, 2000)])
            .await
            .unwrap();
        assert_eq!(p.execution_time(&t[1], 80.0, limit()).await.unwrap(), 2000.0);
    });
}
