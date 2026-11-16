//! Port of core/src/test/kotlin/com/malinskiy/marathon/analytics/metrics/MetricsProviderFactoryTest.kt
//! and NoOpMetricsProviderTest.kt, analytics/external/ExecutionTimeCoverageTest.kt.
use marathon_logging::Level;

use marathon_error::FailureKind;
mod metrics_support;
mod strategy_support;
use jcompat::jtime::Instant;
use marathon_config::*;
use marathon_core::{
    analytics::{
        remote::{graphite::*, influx::*, influx2::*},
        *,
    },
    error::OperationError,
    executor::shared::{failure, requested},
    model::*,
    runtime::ServiceFuture,
    strategy::{FlakinessStrategy, MetricsKey},
    wiring::MetricsProvider,
};
use metrics_support::*;
use std::sync::{Arc, Mutex};
use strategy_support::*;

struct Setup;
struct BadGraphite;
impl GraphiteQuery for BadGraphite {
    fn query<'a>(&'a self, _: &'a str, _: Instant) -> ServiceFuture<'a, Vec<String>> {
        Box::pin(async { Err(failure(FailureKind::IllegalArgument, "Invalid URL host: not a host").into()) })
    }
}
impl MetricsSetup for Setup {
    fn influx<'a>(&'a self, _: &'a InfluxDbConfiguration, _: bool) -> ServiceFuture<'a, Arc<dyn InfluxQuery>> {
        Box::pin(async {
            Err(failure(
                FailureKind::IllegalArgument,
                "Expected URL scheme 'http' or 'https' but no scheme was found for host",
            )
            .into())
        })
    }
    fn influx2<'a>(&'a self, _: &'a InfluxDb2Configuration, _: bool) -> ServiceFuture<'a, Arc<dyn FluxQuery>> {
        Box::pin(async { Err(failure(FailureKind::IllegalArgument, "invalid URL").into()) })
    }
    fn graphite<'a>(&'a self, host: &'a str) -> ServiceFuture<'a, Arc<dyn GraphiteQuery>> {
        assert!(matches!(host, "not a host" | "�"));
        Box::pin(async { Ok(Arc::new(BadGraphite) as Arc<dyn GraphiteQuery>) })
    }
}
fn influx() -> AnalyticsConfiguration {
    AnalyticsConfiguration::InfluxDb(InfluxDbConfiguration {
        url: "host".into(),
        user: "user".into(),
        password: "password".into(),
        db_name: "db".into(),
        retention_policy_configuration: RetentionPolicyConfiguration::default_policy(),
        defaults: defaults(300000, None),
        read_only: false,
    })
}
#[test]
fn should_return_noop_provider_when_disabled() {
    run(async {
        let h = Harness::new();
        let p = MetricsProviderFactory::new(h.context.clone(), Arc::new(Setup))
            .unwrap()
            .create_for(&AnalyticsConfiguration::Disabled, false)
            .await
            .unwrap();
        assert!(p.is_noop());
        assert_eq!(p.execution_time(&default_test(), 80.0, limit()).await.unwrap(), 0.0);
        assert!(h.logs.take().is_empty());
    });
}
#[test]
fn should_return_noop_provider_when_configuration_is_invalid() {
    run(async {
        let h = Harness::new();
        let p = MetricsProviderFactory::new(h.context.clone(), Arc::new(Setup))
            .unwrap()
            .create_for(&influx(), false)
            .await
            .unwrap();
        p.report_execution_time_coverage(&generate_tests(1), 80.0, limit(), "sorting")
            .await
            .unwrap();
        let events = h.logs.take();
        assert_eq!(events.len(), 2);
        assert!(events[0].cause.is_some());
        assert_eq!(events[1].level, Level::WARN);
        assert!(
            events[1]
                .message
                .to_owned()
                .contains("the analytics backend could not be set up")
        );
    });
}
#[test]
fn should_pass_observed_duration_min_tests_to_the_provider() {
    run(async {
        let h = Harness::new();
        let config = AnalyticsConfiguration::Graphite(GraphiteConfiguration {
            host: "not a host".into(),
            port: Some(2003),
            prefix: None,
            defaults: defaults(300000, Some(1)),
            read_only: false,
        });
        let p = MetricsProviderFactory::new(h.context.clone(), Arc::new(Setup))
            .unwrap()
            .create_for(&config, false)
            .await
            .unwrap();
        let t = generate_tests(2);
        p.observe_execution_times(&[result(&t[0], TestStatus::Passed, 20000)])
            .await
            .unwrap();
        assert_eq!(p.execution_time(&t[1], 80.0, limit()).await.unwrap(), 20000.0);
        assert_eq!(*h.clock.delays.lock().unwrap(), vec![50, 50]);
    });
}
#[test]
fn should_return_0_as_success_rate() {
    run(async {
        let h = Harness::new();
        let p = NoOpMetricsProvider::new(false, h.context).unwrap();
        assert_eq!(p.success_rate(&default_test(), limit()).await.unwrap(), 0.0);
    });
}
#[test]
fn should_return_0_as_execution_time() {
    run(async {
        let h = Harness::new();
        let p = NoOpMetricsProvider::new(false, h.context).unwrap();
        assert_eq!(p.execution_time(&default_test(), 90.0, limit()).await.unwrap(), 0.0);
    });
}
async fn noop_coverage(unavailable: bool) {
    let h = Harness::new();
    let p = NoOpMetricsProvider::new(unavailable, h.context.clone()).unwrap();
    p.report_execution_time_coverage(&generate_tests(2), 80.0, limit(), "the batch duration limit")
        .await
        .unwrap();
    let e = h.logs.take();
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].level, if unavailable { Level::WARN } else { Level::INFO });
    assert!(e[0].message.to_owned().contains(if unavailable {
        "0 of 2 tests have it, the analytics backend could not be set up"
    } else {
        "0 of 2 tests have it, analytics is disabled"
    }));
}
#[test]
fn should_report_coverage_when_analytics_is_disabled() {
    run(noop_coverage(false));
}
#[test]
fn should_warn_about_coverage_when_the_backend_could_not_be_set_up() {
    run(noop_coverage(true));
}

type CoverageReport = (Vec<Test>, f64, Instant, String);
#[derive(Default)]
struct Recorder(Mutex<Vec<CoverageReport>>);
impl MetricsProvider for Recorder {
    fn success_rate<'a>(&'a self, _: &'a Test, _: Instant) -> ServiceFuture<'a, f64> {
        Box::pin(async { Ok(0.0) })
    }
    fn execution_time<'a>(&'a self, _: &'a Test, _: f64, _: Instant) -> ServiceFuture<'a, f64> {
        Box::pin(async { Ok(0.0) })
    }
    fn report_execution_time_coverage<'a>(
        &'a self,
        t: &'a [Test],
        p: f64,
        l: Instant,
        u: &'a str,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.0.lock().unwrap().push((t.to_vec(), p, l, u.to_string()));
            Ok(())
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
fn config() -> Configuration {
    use marathon_config::{
        binder::Binder,
        binding::{BindingContext, Profile},
    };
    use std::path::PathBuf;
    let context = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: PathBuf::from("/Marathonfile"),
        marathonfile_dir: PathBuf::from("/"),
        process_dir: PathBuf::from("/"),
        environment: vec![],
        now: limit(),
        analytics_tracking: false,
        bugsnag_reporting: false,
    };
    let stream = node::load_bytes(b"name: test\noutputDir: output\nvendorConfiguration: {}\n");
    let binder = Binder::new(&context, &stream);
    binder
        .first(&stream, |v| {
            bind::bind_configuration(v, |_| Ok(Some(VendorConfiguration::Stub)))
        })
        .unwrap()
        .unwrap()
        .unwrap()
}
#[test]
fn coverage_is_reported_for_the_batch_duration_limit_and_for_execution_time_sorting() {
    run(async {
        let mut c = config();
        let batch = jcompat::jtime::instant_of(1000, 0).unwrap();
        let sort = jcompat::jtime::instant_of(2000, 0).unwrap();
        c.batching_strategy = BatchingStrategyConfiguration::FixedSize {
            size: 5,
            duration_millis: Some(199000),
            percentile: Some(80.0),
            time_limit: Some(batch),
            last_mile_length: 0,
        };
        c.sorting_strategy = SortingStrategyConfiguration::ExecutionTime {
            percentile: 90.0,
            time_limit: sort,
        };
        let shard = TestShard {
            tests: generate_tests(2),
            flaky_tests: generate_tests_in("flaky", 1),
        };
        let recorder = Recorder::default();
        report_execution_time_coverage(&c, &shard, &recorder).await.unwrap();
        let tests: Vec<_> = shard.tests.into_iter().chain(shard.flaky_tests).collect();
        assert_eq!(
            *recorder.0.lock().unwrap(),
            vec![
                (tests.clone(), 80.0, batch, "the batch duration limit".into()),
                (tests, 90.0, sort, "execution time sorting".into())
            ]
        );
    });
}
#[test]
fn coverage_is_not_reported_if_no_strategy_uses_execution_time_history() {
    run(async {
        let mut c = config();
        c.batching_strategy = BatchingStrategyConfiguration::FixedSize {
            size: 5,
            duration_millis: None,
            percentile: Some(80.0),
            time_limit: Some(limit()),
            last_mile_length: 0,
        };
        let recorder = Recorder::default();
        report_execution_time_coverage(
            &c,
            &TestShard {
                tests: generate_tests(2),
                flaky_tests: vec![],
            },
            &recorder,
        )
        .await
        .unwrap();
        assert!(recorder.0.lock().unwrap().is_empty());
    });
}
const NO_COPIES: &str = "flakinessStrategy probability adds no copies";
fn probability() -> FlakinessStrategy {
    FlakinessStrategy::from_config(&FlakinessStrategyConfiguration::Probability {
        min_success_rate: 0.8,
        max_count: 4,
        time_limit: limit(),
    })
}
fn flaky(strategy: &FlakinessStrategy, p: &Analytics, tests: &[Test]) -> Vec<Test> {
    strategy
        .process(
            TestShard {
                tests: tests.to_vec(),
                flaky_tests: vec![],
            },
            p.lookup().as_ref(),
        )
        .flaky_tests
}
fn every(tests: &[Test], copies: usize) -> Vec<Test> {
    tests
        .iter()
        .flat_map(|t| std::iter::repeat_n(t.clone(), copies))
        .collect()
}
/// The D71 warnings, as (logger, message).
fn unavailable(h: &Harness) -> Vec<(String, String)> {
    h.logs
        .records()
        .iter()
        .filter(|r| r.level == Level::WARN && r.message.to_owned().starts_with("Test history unavailable"))
        .map(|r| (r.logger.clone(), r.message.to_owned()))
        .collect()
}
fn no_success_rate_lines(h: &Harness) -> usize {
    h.logs
        .records()
        .iter()
        .filter(|r| r.message.to_owned().starts_with("No success rate found"))
        .count()
}
/// D71 (C8 fixed): without history every test used to look like it never passed and got maxCount copies.
async fn assert_no_copies(h: &Harness, p: &Analytics, logger: &str, reason: &str) {
    let tests = generate_tests(3);
    let strategy = probability();
    assert_eq!(flaky(&strategy, p, &tests), vec![]);
    // Each pass and each shard asks again, but the run warns once.
    assert_eq!(flaky(&strategy, p, &tests), vec![]);
    assert_eq!(
        unavailable(h),
        vec![(
            logger.to_owned(),
            format!("Test history unavailable ({reason}); {NO_COPIES}")
        )]
    );
    assert_eq!(no_success_rate_lines(h), 0);
}
#[test]
fn d71_disabled_analytics_adds_no_probability_copies() {
    run(async {
        let h = Harness::new();
        let factory = MetricsProviderFactory::new(h.context.clone(), Arc::new(Setup)).unwrap();
        let p = factory
            .create_for(&AnalyticsConfiguration::Disabled, false)
            .await
            .unwrap();
        p.prefetch(&[MetricsKey::SuccessRate { limit: limit() }]).await.unwrap();
        assert_no_copies(&h, &p, "marathon_core::analytics::noop", "analytics is disabled").await;
    });
}
#[test]
fn d71_a_backend_that_could_not_be_set_up_adds_no_probability_copies() {
    run(async {
        let h = Harness::new();
        let factory = MetricsProviderFactory::new(h.context.clone(), Arc::new(Setup)).unwrap();
        let p = factory.create_for(&influx(), false).await.unwrap();
        p.prefetch(&[MetricsKey::SuccessRate { limit: limit() }]).await.unwrap();
        assert_no_copies(
            &h,
            &p,
            "marathon_core::analytics::noop",
            "the analytics backend could not be set up",
        )
        .await;
    });
}
#[test]
fn d71_a_failed_success_rate_query_adds_no_probability_copies() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source {
            failure: true,
            ..Source::default()
        });
        let p = h.provider(source, 300000, None);
        p.prefetch(&[MetricsKey::SuccessRate { limit: limit() }]).await.unwrap();
        assert_no_copies(
            &h,
            &p,
            "marathon_core::analytics::provider",
            "the success rate query failed",
        )
        .await;
        // Only the success-rate history is judged: a failed query of another key does not matter.
        let other = Harness::new();
        let source = Arc::new(Source::default());
        *source.errors.lock().unwrap() = (0..3)
            .map(|_| Some(failure(FailureKind::Runtime, "unreadable").into()))
            .collect();
        let p = other.provider(source, 300000, None);
        p.prefetch(&[metrics_support::key(80.0), MetricsKey::SuccessRate { limit: limit() }])
            .await
            .unwrap();
        let tests = generate_tests(2);
        assert_eq!(flaky(&probability(), &p, &tests), every(&tests, 4));
        assert_eq!(unavailable(&other), vec![]);
    });
}
#[test]
fn d71_a_cancelled_success_rate_query_adds_no_probability_copies() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source::default());
        *source.errors.lock().unwrap() = [Some(OperationError::Cancelled(requested("cancelled fetch")))].into();
        let p = h.provider(source, 300000, None);
        assert!(matches!(
            p.prefetch(&[MetricsKey::SuccessRate { limit: limit() }]).await,
            Err(OperationError::Cancelled(_))
        ));
        assert_no_copies(
            &h,
            &p,
            "marathon_core::analytics::provider",
            "the success rate query did not finish",
        )
        .await;
    });
}
/// A shard that filtering emptied is not prefetched (`prepare_test_shard`), so asking about its
/// history would warn about a working backend. Such a pass has nothing to copy and asks nothing.
#[test]
fn d71_an_empty_shard_does_not_report_history_unavailable() {
    run(async {
        let h = Harness::new();
        let p = h.provider(Arc::new(Source::default()), 300000, None);
        assert_eq!(flaky(&probability(), &p, &[]), vec![]);
        let factory = MetricsProviderFactory::new(h.context.clone(), Arc::new(Setup)).unwrap();
        let p = factory
            .create_for(&AnalyticsConfiguration::Disabled, false)
            .await
            .unwrap();
        assert_eq!(flaky(&probability(), &p, &[]), vec![]);
        assert_eq!(unavailable(&h), vec![]);
    });
}
/// C8 kept as configured: a working backend's window, whatever it holds, is history. A test it
/// does not name takes `defaults.successRate` (0 unless configured), so new tests still get copies.
#[test]
fn c8_tests_missing_from_working_history_take_the_configured_default_success_rate() {
    run(async {
        let tests = generate_tests(3);
        let key = MetricsKey::SuccessRate { limit: limit() };
        // A successful empty window.
        let h = Harness::new();
        let p = h.provider(Arc::new(Source::default()), 300000, None);
        p.prefetch(&[key]).await.unwrap();
        assert_eq!(flaky(&probability(), &p, &tests), every(&tests, 4));
        assert_eq!(unavailable(&h), vec![]);
        assert_eq!(no_success_rate_lines(&h), 3);
        // The same window with a configured default at or above minSuccessRate.
        let h = Harness::new();
        let mut defaults = defaults(300000, None);
        defaults.success_rate = 0.8;
        let p =
            Analytics::new(MetricsProviderImpl::new(Arc::new(Source::default()), defaults, h.context.clone()).unwrap());
        p.prefetch(&[key]).await.unwrap();
        assert_eq!(flaky(&probability(), &p, &tests), vec![]);
        // Partial history: only the tests it does not name take the default.
        let h = Harness::new();
        let source = Arc::new(Source::default());
        *source.rates.lock().unwrap() = vec![rate_row(&tests[1], 1.0)];
        let p = h.provider(source, 300000, None);
        p.prefetch(&[key]).await.unwrap();
        assert_eq!(
            flaky(&probability(), &p, &tests),
            [every(&tests[..1], 4), every(&tests[2..], 4)].concat()
        );
        assert_eq!(no_success_rate_lines(&h), 2);
        // A genuine 0% success rate.
        let h = Harness::new();
        let source = Arc::new(Source::default());
        *source.rates.lock().unwrap() = tests.iter().map(|t| rate_row(t, 0.0)).collect();
        let p = h.provider(source, 300000, None);
        p.prefetch(&[key]).await.unwrap();
        assert_eq!(flaky(&probability(), &p, &tests), every(&tests, 4));
        assert_eq!((unavailable(&h), no_success_rate_lines(&h)), (vec![], 0));
    });
}
/// D72: fixed copies do not read history, so no state of the analytics backend changes them.
#[test]
fn d72_fixed_copies_are_made_whatever_the_history() {
    run(async {
        let tests = generate_tests(3);
        let fixed = FlakinessStrategy::from_config(&FlakinessStrategyConfiguration::Fixed { count: 2 });
        assert!(fixed.metrics_keys().is_empty());
        let h = Harness::new();
        let factory = MetricsProviderFactory::new(h.context.clone(), Arc::new(Setup)).unwrap();
        let disabled = factory
            .create_for(&AnalyticsConfiguration::Disabled, false)
            .await
            .unwrap();
        let not_set_up = factory.create_for(&influx(), false).await.unwrap();
        let failing = h.provider(
            Arc::new(Source {
                failure: true,
                ..Source::default()
            }),
            300000,
            None,
        );
        failing
            .prefetch(&[MetricsKey::SuccessRate { limit: limit() }])
            .await
            .unwrap();
        let source = Arc::new(Source::default());
        *source.rates.lock().unwrap() = tests.iter().map(|t| rate_row(t, 1.0)).collect();
        let perfect = h.provider(source, 300000, None);
        perfect
            .prefetch(&[MetricsKey::SuccessRate { limit: limit() }])
            .await
            .unwrap();
        h.logs.take();
        for p in [disabled, not_set_up, failing, perfect] {
            assert_eq!(flaky(&fixed, &p, &tests), every(&tests, 2));
        }
        assert_eq!(unavailable(&h), vec![]);
    });
}

#[test]
fn logger_levels_survive_configuration_as_in_the_jvm_probe() {
    run(async {
        for line in include_str!("../../../fixtures/oracle/metrics/loggers.jsonl").lines() {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            let h = Harness::new();
            let p = h.provider(Arc::new(Source::default()), 0, None);
            let noop = NoOpMetricsProvider::new(false, h.context.clone()).unwrap();
            let factory = MetricsProviderFactory::new(h.context.clone(), Arc::new(Setup)).unwrap();
            factory.create_for(&influx(), false).await.unwrap();
            h.logs.take();
            h.context
                .registry
                .lock()
                .unwrap()
                .configure(row["debug"].as_bool().unwrap(), marathon_logging::Vendor::Android);
            {
                let registry = h.context.registry.lock().unwrap();
                for name in [
                    "marathon_core::analytics::provider",
                    "marathon_core::analytics::noop",
                    "marathon_core::analytics::factory",
                ] {
                    let logger = registry.component(name);
                    for level in [Level::DEBUG, Level::WARN] {
                        assert!(registry.enabled(&logger, level));
                    }
                }
            }
            p.execution_time(&generate_tests(1)[0], 80.0, limit()).await.unwrap();
            p.success_rate(&generate_tests(1)[0], limit()).await.unwrap();
            noop.report_execution_time_coverage(&generate_tests(1), 80.0, limit(), "sorting")
                .await
                .unwrap();
            let events = h
                .logs
                .take()
                .iter()
                .map(|e| {
                    let mut m = e.message.to_owned();
                    if e.logger == "marathon_core::timing" {
                        m.truncate(m.rfind(" in ").unwrap());
                        m.push_str(" in <ms> ms");
                    }
                    serde_json::json!([e.logger, e.level.as_str(), m])
                })
                .collect::<Vec<_>>();
            let mut expected = marathon_test_support::recorded_logs::native_logs(&serde_json::json!(
                row["events"].as_array().unwrap()[6..]
            ));
            for event in expected.as_array_mut().unwrap() {
                if event[0] == "marathon_core::analytics::provider" && event[1] == "DEBUG" && event[2] == "[]" {
                    event[2] = "Execution time history: []".into();
                }
            }
            assert_eq!(serde_json::json!(events), expected);
        }
    });
}

#[test]
fn normalized_surrogates_reach_graphite_setup_and_query_failures_retry() {
    run(async {
        for host in [false, true] {
            let h = Harness::new();
            let config = AnalyticsConfiguration::Graphite(GraphiteConfiguration {
                host: if host {
                    String::from_utf16_lossy(&[0xd800])
                } else {
                    "not a host".into()
                },
                port: None,
                prefix: (!host).then(|| String::from_utf16_lossy(&[0xd800])),
                defaults: defaults(300000, None),
                read_only: false,
            });
            let p = MetricsProviderFactory::new(h.context.clone(), Arc::new(Setup))
                .unwrap()
                .create_for(&config, false)
                .await
                .unwrap();
            assert_eq!(
                p.execution_time(&default_test(), 80.0, limit()).await.unwrap(),
                300000.0
            );
            assert_eq!(*h.clock.delays.lock().unwrap(), vec![50, 50]);
            assert!(
                h.logs
                    .take()
                    .iter()
                    .all(|r| r.logger != "marathon_core::analytics::factory")
            );
        }
    });
}

#[test]
fn factory_rejects_overflowing_default_before_setup_or_noop_fallback() {
    run(async {
        let h = Harness::new();
        let factory = MetricsProviderFactory::new(h.context.clone(), Arc::new(Setup)).unwrap();
        let mut configs = vec![
            influx(),
            AnalyticsConfiguration::InfluxDb2(InfluxDb2Configuration {
                url: "host".into(),
                token: "token".into(),
                organization: "org".into(),
                bucket: "bucket".into(),
                retention_policy_configuration: InfluxDb2RetentionPolicyConfiguration::default_policy(),
                defaults: defaults(0, None),
                read_only: false,
            }),
            AnalyticsConfiguration::Graphite(GraphiteConfiguration {
                host: "setup must not run".into(),
                port: None,
                prefix: None,
                defaults: defaults(0, None),
                read_only: false,
            }),
        ];
        for config in &mut configs {
            let defaults = match config {
                AnalyticsConfiguration::InfluxDb(c) => &mut c.defaults,
                AnalyticsConfiguration::InfluxDb2(c) => &mut c.defaults,
                AnalyticsConfiguration::Graphite(c) => &mut c.defaults,
                _ => unreachable!(),
            };
            defaults.duration = jcompat::jtime::duration_of(i64::MAX, 0).unwrap();
            let error = factory.create_for(config, false).await.err().unwrap();
            assert_eq!(
                error,
                failure(
                    FailureKind::IllegalArgument,
                    "analyticsConfiguration.defaults.duration exceeds the millisecond range of a Java Long"
                )
                .into()
            );
        }
        assert!(h.logs.take().is_empty());
    });
}

#[test]
fn queued_records_render_with_registered_levels_after_debug_false() {
    use marathon_core::logging::LogRecord;
    use marathon_logging::Vendor;
    let h = Harness::new();
    let _provider = h.provider(Arc::new(Source::default()), 0, None);
    let _noop = NoOpMetricsProvider::new(false, h.context.clone()).unwrap();
    let _factory = MetricsProviderFactory::new(h.context.clone(), Arc::new(Setup)).unwrap();
    let mut registry = h.context.registry.lock().unwrap();
    registry.configure(false, Vendor::Android);
    for (name, level) in [
        ("marathon_core::timing", Level::INFO),
        ("marathon_core::analytics::provider", Level::DEBUG),
        ("marathon_core::analytics::noop", Level::DEBUG),
        ("marathon_core::analytics::factory", Level::DEBUG),
    ] {
        let record = LogRecord {
            target: module_path!().into(),
            logger: name.into(),
            level,
            message: "visible".into(),
            at_millis: 0,
            wall_millis: 0,
            cause: None,
        };
        assert!(registry.enabled_target(record.target(), level), "{name}");
    }
}
