//! Port of core/src/test/kotlin/com/malinskiy/marathon/StartupTimingsTest.kt
//! (the fifth method, about analytics query timings, is 3.4a's).
use jcompat::{
    jtime::{duration_of, instant_of},
    logging::{Level, MarathonLogging},
};
use marathon_config::{Defaults, schema::SortingStrategyConfiguration};
use marathon_core::{
    analytics::{
        Analytics, MetricsProviderImpl,
        remote::{ExecutionTime, RemoteDataSource, SuccessRate},
    },
    logging::{LogRecord, OutputRecord},
    marathon::{TIMINGS, timing_message},
    model::{ExecutionCommand, Test, TestStatus},
    output::ConsoleSink,
    runtime::*,
    vendor::{LocalTestParser, RemoteTestParser, TestParser, VendorContext},
};
use marathon_test_support::{
    console::{Line, RecordingConsole},
    factory::{FakeTimer, MarathonFactory},
    scenario::{self, Script},
    stub_device::{StubDevice, StubDeviceOptions},
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

fn test() -> Test {
    scenario::test("pkg", "SimpleTest", "test")
}

fn timings(lines: &[Line]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| match line {
            Line::Log {
                logger, level, message, ..
            } if logger == TIMINGS => {
                assert_eq!(*level, Level::Info);
                Some(message.to_owned())
            }
            _ => None,
        })
        .collect()
}

fn connect_at_1000(device: &Arc<StubDevice>, end: i64) -> Script {
    Script {
        entry: scenario::Entry::RunAsync,
        command: ExecutionCommand::Run,
        events: vec![
            (1000, "connect".into(), Some("serial-1".into()), serde_json::Value::Null),
            (end, "end".into(), None, serde_json::Value::Null),
        ],
        devices: HashMap::from([("serial-1".to_owned(), device.clone())]),
        expect: serde_json::Value::Null,
    }
}

#[test]
fn timings_are_logged_at_info_while_other_loggers_only_log_errors_without_debug() {
    let registry = Arc::new(Mutex::new(MarathonLogging::default()));
    let other = {
        let mut registry = registry.lock().unwrap();
        registry.debug = false;
        registry.timings();
        registry.logger("StartupTimingsTest-other", None)
    };
    let line = timing_message("Phase took 5 ms", &FakeTimer(Arc::new(|| 1234))).unwrap();
    let console = RecordingConsole::new(registry.clone());
    futures_util::FutureExt::now_or_never(console.write(vec![OutputRecord::Log(LogRecord {
        logger: TIMINGS.into(),
        at_millis: 0,
        wall_millis: 0,
        level: Level::Info,
        message: line,
        cause: None,
    })]))
    .unwrap()
    .unwrap();

    assert!(!registry.lock().unwrap().enabled(&other, Level::Info));
    assert_eq!(
        timings(&console.lines()),
        vec!["Phase took 5 ms (+1234 ms since start)"]
    );
}

struct DelayedParser;
impl RemoteTestParser for DelayedParser {
    fn extract(
        &self,
        _device: marathon_core::vendor::DeviceRef,
        context: VendorContext,
    ) -> VendorFuture<'_, Vec<Test>> {
        Box::pin(async move {
            context.clock.delay(500, context.cancellation.clone()).await?;
            Ok(vec![test()])
        })
    }
}

#[test]
fn remote_parsing_logs_how_long_the_device_took_to_become_available_and_how_long_parsing_took() {
    let device = StubDevice::new(StubDeviceOptions {
        serial_number: "worker-1:sim-1".into(),
        ..StubDeviceOptions::default()
    });
    let mut factory = MarathonFactory::new();
    factory.configuration.configuration.debug = false;
    factory.parser = Some(TestParser::Remote(Arc::new(DelayedParser)));
    let script = Script {
        entry: scenario::Entry::RunAsync,
        command: ExecutionCommand::Parse {
            output_file_name: Some("parsed".into()),
            include_flaky_tests: false,
        },
        events: vec![(20_000, "end".into(), None, serde_json::Value::Null)],
        devices: HashMap::new(),
        expect: serde_json::Value::Null,
    };
    let borrowed = device.reference();
    let outcome = scenario::drive(
        factory,
        script,
        Some(Box::new(|core, factory| {
            let now = core.clock().now_source();
            factory.timer = Some(Arc::new(FakeTimer(now)));
        })),
        Some(Box::new(move |built| {
            *built.provider.on_initialize.lock().unwrap() = Some(Arc::new(|context: VendorContext| {
                Box::pin(async move { context.clock.delay(200, context.cancellation.clone()).await })
            }));
            *built.provider.borrowing_device.lock().unwrap() = Some(borrowed);
            *built.provider.borrow_delay_millis.lock().unwrap() = 3000;
        })),
    );

    assert!(matches!(outcome.value, Some(Ok(true))));
    assert_eq!(
        timings(&outcome.lines),
        vec![
            "Device provider initialized in 200 ms (+200 ms since start)",
            "Parsed 1 tests in 500 ms on worker-1:sim-1, which took 3000 ms to become available (+3700 ms since start)",
        ]
    );
}

#[test]
fn a_run_logs_the_start_of_scheduling_without_debug() {
    let device = StubDevice::new(StubDeviceOptions::default());
    device.set_execution_results(HashMap::from([(test(), vec![TestStatus::Passed])]));
    let mut factory = MarathonFactory::new();
    factory.configuration.configuration.debug = false;
    factory.tests = vec![test()];
    let outcome = scenario::drive(factory, connect_at_1000(&device, 20_000), None, None);

    assert!(outcome.completed);
    let lines = timings(&outcome.lines);
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(lines[0].contains("Device provider initialized in "));
    assert!(lines[1].contains("Parsed 1 tests in "));
    assert!(lines[2].contains("Prepared the shard of 1 tests and the analytics history it uses in "));
    assert!(lines[3].contains("Scheduling 1 tests (+"));
}

struct History;
impl RemoteDataSource for History {
    fn request_all_success_rates(&self, _limit: jcompat::jtime::Instant) -> ServiceFuture<'_, Vec<SuccessRate>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn request_all_execution_times(
        &self,
        _percentile: f64,
        _limit: jcompat::jtime::Instant,
    ) -> ServiceFuture<'_, Vec<ExecutionTime>> {
        Box::pin(async {
            Ok(vec![ExecutionTime {
                test_name: "pkg.SimpleTest#test".into(),
                percentile: 1.0,
            }])
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct OneTest;
impl LocalTestParser for OneTest {
    fn extract(&self, _context: VendorContext) -> VendorFuture<'_, Vec<Test>> {
        Box::pin(async { Ok(vec![test()]) })
    }
}

#[test]
fn the_execution_time_history_is_queried_while_the_run_starts_before_scheduling() {
    let device = StubDevice::new(StubDeviceOptions::default());
    device.set_execution_results(HashMap::from([(test(), vec![TestStatus::Passed])]));
    let mut factory = MarathonFactory::new();
    factory.configuration.configuration.debug = false;
    factory.configuration.configuration.sorting_strategy = SortingStrategyConfiguration::ExecutionTime {
        percentile: 90.0,
        time_limit: instant_of(0, 0).unwrap(),
    };
    factory.parser = Some(TestParser::Local(Arc::new(OneTest)));
    factory.analytics = Some(Box::new(|context| {
        let defaults = Defaults {
            success_rate: 0.5,
            duration: duration_of(1, 0).unwrap(),
            observed_duration_min_tests: None,
        };
        Ok(Analytics::new(MetricsProviderImpl::new(
            Arc::new(History),
            defaults,
            context,
        )?))
    }));
    let outcome = scenario::drive(
        factory,
        connect_at_1000(&device, 20_000),
        Some(Box::new(|core, factory| {
            factory.timer = Some(Arc::new(FakeTimer(core.clock().now_source())));
        })),
        None,
    );

    assert!(outcome.completed);
    let prefixes: Vec<String> = timings(&outcome.lines)
        .iter()
        .map(|line| line.split(" in ").next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        prefixes,
        vec![
            "Device provider initialized",
            "Parsed 1 tests",
            "Execution time history query (percentile 90.0 since 1970-01-01T00:00:00Z) returned 1 tests",
            "Prepared the shard of 1 tests and the analytics history it uses",
            "Scheduling 1 tests (+0 ms since start)",
        ]
    );
}

/// Fails the first `failing` queries, and then knows one test.
#[derive(Default)]
struct Unreliable {
    failing: usize,
    calls: AtomicUsize,
    closed: AtomicBool,
    after_close: AtomicUsize,
}
impl RemoteDataSource for Unreliable {
    fn request_all_success_rates(&self, _limit: jcompat::jtime::Instant) -> ServiceFuture<'_, Vec<SuccessRate>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn request_all_execution_times(
        &self,
        _percentile: f64,
        _limit: jcompat::jtime::Instant,
    ) -> ServiceFuture<'_, Vec<ExecutionTime>> {
        Box::pin(async {
            if self.closed.load(Ordering::SeqCst) {
                self.after_close.fetch_add(1, Ordering::SeqCst);
            }
            if self.calls.fetch_add(1, Ordering::SeqCst) < self.failing {
                return Err(marathon_core::executor::shared::failure("java.io.IOException", "down").into());
            }
            Ok(vec![ExecutionTime {
                test_name: "pkg.SimpleTest#test".into(),
                percentile: 1.0,
            }])
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async {
            self.closed.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}
fn run_with(source: Arc<Unreliable>, connect_at: i64) -> scenario::Outcome {
    let device = StubDevice::new(StubDeviceOptions::default());
    device.set_execution_results(HashMap::from([(test(), vec![TestStatus::Passed])]));
    let mut factory = MarathonFactory::new();
    factory.configuration.configuration.debug = false;
    factory.configuration.configuration.sorting_strategy = SortingStrategyConfiguration::ExecutionTime {
        percentile: 90.0,
        time_limit: instant_of(0, 0).unwrap(),
    };
    factory.parser = Some(TestParser::Local(Arc::new(OneTest)));
    factory.analytics = Some(Box::new(move |context| {
        let defaults = Defaults {
            success_rate: 0.5,
            duration: duration_of(1, 0).unwrap(),
            observed_duration_min_tests: None,
        };
        Ok(Analytics::new(MetricsProviderImpl::new(source, defaults, context)?))
    }));
    let mut script = connect_at_1000(&device, 2_000_000);
    script.events[0].0 = connect_at;
    scenario::drive(
        factory,
        script,
        Some(Box::new(|core, factory| {
            factory.timer = Some(Arc::new(FakeTimer(core.clock().now_source())));
        })),
        None,
    )
}
const FAILED: &str = "Execution time history query (percentile 90.0 since 1970-01-01T00:00:00Z) failed";
const ANSWERED: &str = "Execution time history query (percentile 90.0 since 1970-01-01T00:00:00Z) returned 1 tests";
fn query_lines(lines: &[Line]) -> Vec<String> {
    timings(lines)
        .iter()
        .map(|line| line.split(" in ").next().unwrap_or_default().to_owned())
        .filter(|line| line.contains("history query"))
        .collect()
}

/// D34, through the real vendor runtime and run flow.
#[test]
fn a_run_against_a_backend_that_stays_down_ends_without_waiting_for_its_refresh() {
    let source = Arc::new(Unreliable {
        failing: usize::MAX,
        ..Unreliable::default()
    });
    let outcome = run_with(source.clone(), 1000);
    assert!(outcome.completed);
    assert!(matches!(outcome.value, Some(Ok(true))), "{:?}", outcome.value);
    // It ends when a run with a healthy backend ends, with the next re-query 10 s away.
    let healthy = run_with(Arc::new(Unreliable::default()), 1000);
    assert!(healthy.completed_at.is_some());
    assert_eq!(outcome.completed_at, healthy.completed_at);
    assert_eq!(query_lines(&healthy.lines), vec![ANSWERED]);
    // The first refresh, due at 5 s, happened during the run; the second, at 15 s, did not.
    assert_eq!(query_lines(&outcome.lines), vec![FAILED, FAILED]);
    assert_eq!(source.calls.load(Ordering::SeqCst), 4);
    assert!(source.closed.load(Ordering::SeqCst));
    assert_eq!(source.after_close.load(Ordering::SeqCst), 0);
}

#[test]
fn history_that_comes_back_reaches_a_queue_built_after_the_refresh() {
    let source = Arc::new(Unreliable {
        failing: 3,
        ..Unreliable::default()
    });
    // The first refresh is due at 5 s, and the device connects at 8 s.
    let outcome = run_with(source.clone(), 8000);
    assert!(outcome.completed);
    assert!(matches!(outcome.value, Some(Ok(true))), "{:?}", outcome.value);
    assert_eq!(query_lines(&outcome.lines), vec![FAILED, ANSWERED]);
    assert_eq!(source.calls.load(Ordering::SeqCst), 4);
    assert_eq!(source.after_close.load(Ordering::SeqCst), 0);
}
