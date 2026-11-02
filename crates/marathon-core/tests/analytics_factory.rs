//! Port of core/src/test/kotlin/com/malinskiy/marathon/analytics/metrics/MetricsProviderFactoryTest.kt
//! and NoOpMetricsProviderTest.kt, analytics/external/ExecutionTimeCoverageTest.kt.
mod metrics_support;
mod strategy_support;
use jcompat::{jtime::Instant, logging::Level, writers::JavaString};
use marathon_config::*;
use marathon_core::{
    analytics::{
        remote::{graphite::*, influx::*, influx2::*},
        *,
    },
    executor::shared::failure,
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
        Box::pin(async { Err(failure("java.lang.IllegalArgumentException", "Invalid URL host: not a host").into()) })
    }
}
impl MetricsSetup for Setup {
    fn influx<'a>(&'a self, _: &'a InfluxDbConfiguration, _: bool) -> ServiceFuture<'a, Arc<dyn InfluxQuery>> {
        Box::pin(async {
            Err(failure(
                "java.lang.IllegalArgumentException",
                "Expected URL scheme 'http' or 'https' but no scheme was found for host",
            )
            .into())
        })
    }
    fn influx2<'a>(&'a self, _: &'a InfluxDb2Configuration, _: bool) -> ServiceFuture<'a, Arc<dyn FluxQuery>> {
        Box::pin(async { Err(failure("java.lang.IllegalArgumentException", "invalid URL").into()) })
    }
    fn graphite<'a>(&'a self, host: &'a str) -> ServiceFuture<'a, Arc<dyn GraphiteQuery>> {
        assert_eq!(host, "not a host");
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
        p.report_execution_time_coverage(&generate_tests(1), 80.0, limit(), &"sorting".into())
            .await
            .unwrap();
        let events = h.logs.take();
        assert_eq!(events.len(), 2);
        assert!(events[0].cause.is_some());
        assert_eq!(events[1].level, Level::Warn);
        assert!(
            events[1]
                .message
                .to_string_lossy()
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
    p.report_execution_time_coverage(&generate_tests(2), 80.0, limit(), &"the batch duration limit".into())
        .await
        .unwrap();
    let e = h.logs.take();
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].level, if unavailable { Level::Warn } else { Level::Info });
    assert!(e[0].message.to_string_lossy().contains(if unavailable {
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

type CoverageReport = (Vec<Test>, f64, Instant, JavaString);
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
        u: &'a JavaString,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.0.lock().unwrap().push((t.to_vec(), p, l, u.clone()));
            Ok(())
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
fn config() -> Configuration {
    use jcompat::file_manager::JavaPath;
    use marathon_config::{
        binder::Binder,
        binding::{BindingContext, Profile},
    };
    let context = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: JavaPath::from_text("/Marathonfile"),
        marathonfile_dir: JavaPath::from_text("/"),
        process_dir: JavaPath::from_text("/"),
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
#[test]
fn c8_zero_success_rate_repeats_every_test_for_disabled_unavailable_and_empty_history() {
    run(async {
        let h = Harness::new();
        let factory = MetricsProviderFactory::new(h.context.clone(), Arc::new(Setup)).unwrap();
        let providers = [
            factory
                .create_for(&AnalyticsConfiguration::Disabled, false)
                .await
                .unwrap(),
            factory.create_for(&influx(), false).await.unwrap(),
            h.provider(Arc::new(Source::default()), 300000, None),
        ];
        for p in providers {
            p.prefetch(&[MetricsKey::SuccessRate { limit: limit() }]).await.unwrap();
            let tests = generate_tests(3);
            let strategy = FlakinessStrategy::from_config(&FlakinessStrategyConfiguration::Probability {
                min_success_rate: 0.8,
                max_count: 4,
                time_limit: limit(),
            });
            let shard = strategy.process(
                TestShard {
                    tests: tests.clone(),
                    flaky_tests: vec![],
                },
                p.lookup().as_ref(),
            );
            assert_eq!(
                shard.flaky_tests,
                tests
                    .into_iter()
                    .flat_map(|t| std::iter::repeat_n(t, 4))
                    .collect::<Vec<_>>()
            );
        }
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
                .configure(row["debug"].as_bool().unwrap(), jcompat::logging::Vendor::Android);
            {
                let registry = h.context.registry.lock().unwrap();
                for name in ["MetricsProviderImpl", "NoOpMetricsProvider", "MetricsProviderFactory"] {
                    assert_eq!(
                        registry.effective_level(name).as_str(),
                        row["levels"][name].as_str().unwrap()
                    );
                    let logger = registry.slf4j_logger(name);
                    for level in [Level::Debug, Level::Warn] {
                        assert!(registry.enabled(&logger, level));
                    }
                }
            }
            p.execution_time(&generate_tests(1)[0], 80.0, limit()).await.unwrap();
            p.success_rate(&generate_tests(1)[0], limit()).await.unwrap();
            noop.report_execution_time_coverage(&generate_tests(1), 80.0, limit(), &"sorting".into())
                .await
                .unwrap();
            let events = h
                .logs
                .take()
                .iter()
                .map(|e| {
                    let mut m = e.message.to_string_lossy();
                    if e.logger == "Timings" {
                        m.truncate(m.rfind(" in ").unwrap());
                        m.push_str(" in <ms> ms");
                    }
                    serde_json::json!([e.logger, e.level.as_str(), m])
                })
                .collect::<Vec<_>>();
            assert_eq!(events, row["events"].as_array().unwrap()[6..]);
        }
    });
}

#[test]
fn lone_surrogate_graphite_text_fails_queries_instead_of_setup_under_p2() {
    run(async {
        for host in [false, true] {
            let h = Harness::new();
            let config = AnalyticsConfiguration::Graphite(GraphiteConfiguration {
                host: if host {
                    JavaString(vec![0xd800])
                } else {
                    "not a host".into()
                },
                port: None,
                prefix: (!host).then(|| JavaString(vec![0xd800])),
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
            assert!(h.logs.take().iter().all(|r| r.logger != "MetricsProviderFactory"));
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
                    "java.lang.IllegalArgumentException",
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
    use jcompat::logging::{JansiMode, OutputTarget, Vendor};
    use marathon_core::logging::LogRecord;
    let h = Harness::new();
    let _provider = h.provider(Arc::new(Source::default()), 0, None);
    let _noop = NoOpMetricsProvider::new(false, h.context.clone()).unwrap();
    let _factory = MetricsProviderFactory::new(h.context.clone(), Arc::new(Setup)).unwrap();
    let mut registry = h.context.registry.lock().unwrap();
    registry.configure(false, Vendor::Android);
    for (name, level) in [
        ("Timings", Level::Info),
        ("MetricsProviderImpl", Level::Debug),
        ("NoOpMetricsProvider", Level::Debug),
        ("MetricsProviderFactory", Level::Debug),
    ] {
        let record = LogRecord {
            logger: name.into(),
            level,
            message: "visible".into(),
            at_millis: 0,
            wall_millis: 0,
            cause: None,
        };
        let logger = registry.slf4j_logger(&record.logger);
        let rendered = record.render(&registry, &logger, "output", 0, (JansiMode::Strip, OutputTarget::Pipe));
        assert!(rendered.is_some_and(|line| line.contains("visible")), "{name}");
        assert_eq!(registry.effective_level(name), level);
    }
}
