//! D107 in the production run graph. No clock advance releases a batch: the
//! witness decides exactly which batch can finish, and when.
use marathon_config::{
    AnalyticsConfiguration, Configuration, Defaults, ExecutionMode, FlakinessStrategyConfiguration,
    GraphiteConfiguration, PoolingStrategyConfiguration,
};
use marathon_core::{
    analytics::{
        Analytics, BackendFactory, BackendRequest, MetricsProviderImpl, TrackerInternal, TrackerInternalAdapter,
        remote::{ExecutionTime, RemoteDataSource, SuccessRate},
    },
    error::*,
    executor::{CoreExecutor, SharedDeferred, test_support::ControlledReplay},
    model::{
        Attachment, AttachmentType, BatchRef, DeviceInfo, DevicePoolId, ExecutionCommand, TestBatchResults, TestResult,
        TestStatus,
    },
    report::TestEvent,
    runtime::{Deferred, JobKind, ServiceFuture, VendorFuture},
    time::ReplayDriver,
    vendor::{Device, DeviceEvent, DeviceRef, VendorContext},
};
use marathon_logging::{Component, Level};
use marathon_test_support::{
    console::Line,
    factory::MarathonFactory,
    scenario,
    stub_device::{StubDevice, StubDeviceOptions},
};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::BTreeSet,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

type Gate<T> = Arc<SharedDeferred<T>>;
const LATE_ARTIFACT: &str = "redundant-batch-partial-attachment";
const POOL_STOP: &str = "All tests in the pool are decided";

#[derive(Clone)]
struct Attempt {
    serial: String,
    test: String,
    batch: String,
    release: Gate<TestStatus>,
    cleanup: Gate<()>,
}
#[derive(Default)]
struct Trace {
    attempts: Vec<Attempt>,
    seals: Vec<String>,
    cancelled: Vec<(String, bool)>,
    cleaned: Vec<String>,
    metrics: Vec<(String, String, TestStatus)>,
}
type Record = Arc<Mutex<Trace>>;

struct GatedDevice {
    info: Arc<StubDevice>,
    trace: Record,
    cleanup: Gate<()>,
}
impl Device for GatedDevice {
    fn info(&self) -> DeviceInfo {
        self.info.info()
    }
    fn abi(&self) -> String {
        self.info.abi()
    }
    fn worker_id(&self) -> Option<String> {
        None
    }
    fn component(&self) -> &Component {
        self.info.component()
    }
    fn prepare<'a>(&'a self, _: Arc<Configuration>, _: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn execute<'a>(
        &'a self,
        _: Arc<Configuration>,
        _: DevicePoolId,
        batch: BatchRef,
        result: Arc<Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let device = self.info();
            let [test] = batch.tests.as_slice() else {
                panic!("expected a singleton batch")
            };
            let release = SharedDeferred::new();
            self.trace.lock().unwrap().attempts.push(Attempt {
                serial: device.serial_number.clone(),
                test: test.method.as_str().into(),
                batch: batch.id.clone(),
                release: release.clone(),
                cleanup: self.cleanup.clone(),
            });
            let (status, cancelled) = tokio::select! { biased;
                reason = context.cancellation.cancelled() => (TestStatus::Passed, Some(reason)),
                status = release.wait() => (*status?, None),
            };
            let test_result = TestResult {
                test: test.clone(),
                device: device.clone(),
                test_batch_id: batch.id.clone(),
                status,
                start_time: 1_000,
                end_time: 2_000,
                stacktrace: (status == TestStatus::Failure).then(|| "assertion failed".into()),
                attachments: cancelled
                    .as_ref()
                    .map(|_| Attachment {
                        file: PathBuf::from(LATE_ARTIFACT),
                        kind: AttachmentType::Video,
                        name: Some(LATE_ARTIFACT.into()),
                    })
                    .into_iter()
                    .collect(),
            };
            let mut results = TestBatchResults {
                device: DeviceRef(self.info.clone()),
                finished: vec![],
                failed: vec![],
                uncompleted: vec![],
            };
            match status {
                TestStatus::Failure => results.failed.push(test_result),
                TestStatus::Incomplete => results.uncompleted.push(test_result),
                _ => results.finished.push(test_result),
            }
            let published = result.complete(Ok(results));
            if let Some(reason) = cancelled {
                if reason.failure().message.as_deref() == Some(POOL_STOP) {
                    self.trace
                        .lock()
                        .unwrap()
                        .cancelled
                        .push((device.serial_number, published));
                }
                return Err(VendorError::Cancelled(reason));
            }
            Ok(())
        })
    }
    fn seal_for_stop(&self) {
        self.trace.lock().unwrap().seals.push(self.info().serial_number);
    }
    fn join_cancelled_work(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.cleanup.wait().await?;
            self.trace.lock().unwrap().cleaned.push(self.info().serial_number);
            Ok(())
        })
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

struct EmptyHistory;
impl RemoteDataSource for EmptyHistory {
    fn request_all_success_rates(&self, _: jcompat::jtime::Instant) -> ServiceFuture<'_, Vec<SuccessRate>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn request_all_execution_times(&self, _: f64, _: jcompat::jtime::Instant) -> ServiceFuture<'_, Vec<ExecutionTime>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct Backend(Record);
impl BackendFactory for Backend {
    fn create(&self, _: BackendRequest) -> ServiceFuture<'_, Result<Option<Arc<dyn TrackerInternal>>, Failure>> {
        Box::pin(async { Ok(Ok(Some(Arc::new(Self(self.0.clone())) as Arc<dyn TrackerInternal>))) })
    }
}
impl TrackerInternalAdapter for Backend {
    fn test<'a>(&'a self, event: &'a TestEvent) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.0.lock().unwrap().metrics.push((
                event.device.serial_number.clone(),
                event.test_result.test.method.as_str().into(),
                event.test_result.status,
            ));
            Ok(())
        })
    }
}

struct Run {
    replay: Rc<ControlledReplay>,
    provider: Arc<marathon_test_support::stub_provider::StubDeviceProvider>,
    trace: Record,
    result: Rc<RefCell<Option<VendorResult<bool>>>>,
}
impl Run {
    async fn connect(&self, serial: &str, pool: &str, hold_cleanup: bool) -> Attempt {
        let seen = self.trace.lock().unwrap().attempts.len();
        let cleanup = SharedDeferred::new();
        if !hold_cleanup {
            cleanup.complete(Ok(()));
        }
        let device = Arc::new(GatedDevice {
            info: StubDevice::new(StubDeviceOptions {
                serial_number: serial.into(),
                model: pool.into(),
                ..StubDeviceOptions::default()
            }),
            trace: self.trace.clone(),
            cleanup,
        });
        assert!(self.provider.send(DeviceEvent::Connected(DeviceRef(device))));
        if seen == 0 {
            // Let the scheduler's first pool-discovery poll run. Individual
            // attempts are controlled only by their gates.
            self.replay.advance_to(100).await.unwrap();
        } else {
            self.replay.drain_current().await.unwrap();
        }
        self.trace.lock().unwrap().attempts[seen..]
            .iter()
            .find(|a| a.serial == serial)
            .unwrap_or_else(|| panic!("{serial} received no batch"))
            .clone()
    }
    async fn release(&self, attempt: &Attempt, status: TestStatus) {
        assert!(attempt.release.complete(Ok(status)));
        self.replay.drain_current().await.unwrap();
    }
    fn stopped(&self, serial: &str) -> bool {
        self.trace.lock().unwrap().cancelled.iter().any(|(s, _)| s == serial)
    }
    fn result(&self) -> Option<bool> {
        self.result
            .borrow()
            .as_ref()
            .map(|result| *result.as_ref().expect("run verdict"))
    }
}

struct Options {
    methods: &'static [&'static str],
    fast: bool,
    mode: ExecutionMode,
    probability: bool,
    separate_pools: bool,
    ignore_failures: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            methods: &["one"],
            fast: true,
            mode: ExecutionMode::AnySuccess,
            probability: false,
            separate_pools: false,
            ignore_failures: false,
        }
    }
}
type Exercise = for<'a> fn(&'a Run) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>>;
struct Output {
    dir: tempfile::TempDir,
    lines: Vec<Line>,
    trace: Record,
}
fn run(options: Options, exercise: Exercise) -> Output {
    let mut factory = MarathonFactory::new();
    let output = factory.configuration.output.take().unwrap();
    let config = &mut factory.configuration.configuration;
    config.output_dir = output.path().join("output");
    config.uncompleted_test_retry_quota = 0;
    config.execution_strategy.fast = options.fast;
    config.execution_strategy.mode = options.mode;
    config.ignore_failures = options.ignore_failures;
    config.pooling_strategy = if options.separate_pools {
        PoolingStrategyConfiguration::Model
    } else {
        PoolingStrategyConfiguration::Omni
    };
    let epoch = jcompat::jtime::instant_of(0, 0).unwrap();
    config.flakiness_strategy = if options.probability {
        FlakinessStrategyConfiguration::Probability {
            min_success_rate: 0.8,
            max_count: 9,
            time_limit: epoch,
        }
    } else {
        FlakinessStrategyConfiguration::Fixed { count: 1 }
    };
    let defaults = Defaults {
        success_rate: 0.0,
        duration: jcompat::jtime::duration_of(0, 0).unwrap(),
        observed_duration_min_tests: None,
    };
    config.analytics_configuration = AnalyticsConfiguration::Graphite(GraphiteConfiguration {
        host: "carbon.invalid".into(),
        port: None,
        prefix: None,
        defaults,
        read_only: false,
    });
    factory.analytics = Some(Box::new(move |context| {
        Ok(Analytics::new(MetricsProviderImpl::new(
            Arc::new(EmptyHistory),
            defaults,
            context,
        )?))
    }));
    factory.tests = options
        .methods
        .iter()
        .map(|method| scenario::test("test", "Steps", method))
        .collect();
    let trace = Record::default();
    factory.backend = Some(Arc::new(Backend(trace.clone())));
    let observed = trace.clone();
    let lines = CoreExecutor::run_virtual_with_watchdog(Duration::from_secs(60), move |core, replay| {
        Box::pin(async move {
            let built = factory.build(&core).await?;
            let result = Rc::new(RefCell::new(None));
            let completed = result.clone();
            let marathon = built.marathon.clone();
            let job = core.spawn(
                core.job(JobKind::Ordinary, None),
                Box::new(move |task| {
                    Box::pin(async move {
                        *completed.borrow_mut() = Some(
                            marathon
                                .run_under(ExecutionCommand::Run, Some(task.job.clone()))
                                .await
                                .map_err(|error| match error {
                                    OperationError::Cancelled(reason) => VendorError::Cancelled(reason),
                                    OperationError::Failed(cause) => VendorError::Other(cause),
                                }),
                        );
                        Ok(())
                    })
                }),
            );
            let harness = Run {
                replay,
                provider: built.provider.clone(),
                trace: observed,
                result,
            };
            exercise(&harness).await;
            assert!(
                harness.result().is_some(),
                "run still waiting at the end of the witness"
            );
            assert!(
                !harness
                    .replay
                    .open_mailboxes()
                    .iter()
                    .any(|name| name.ends_with("Actor"))
            );
            let _ = job.join().await;
            assert!(built.provider.calls.lock().unwrap().contains(&"terminate"));
            Ok(built.console.lines())
        })
    })
    .unwrap();
    Output {
        dir: output,
        lines,
        trace,
    }
}
impl Output {
    fn raw(&self) -> Vec<Value> {
        serde_json::from_str(&self.read("test_result/raw.json")).unwrap()
    }
    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.dir.path().join("output").join(path)).unwrap()
    }
    fn summary(&self) -> &str {
        self.lines
            .iter()
            .find_map(|line| match line {
                Line::Raw(text) if text.starts_with("Marathon run finished") => Some(text.as_str()),
                _ => None,
            })
            .unwrap()
    }
}

#[test]
fn probability_copies_stop_at_three_deciding_passes_d107() {
    let output = run(
        Options {
            methods: &["one", "two", "three"],
            probability: true,
            ..Options::default()
        },
        |run| {
            Box::pin(async move {
                let mut attempts = vec![];
                for serial in [
                    "hung-device",
                    "healthy-1",
                    "healthy-2",
                    "healthy-3",
                    "healthy-4",
                    "healthy-5",
                    "healthy-6",
                    "healthy-7",
                ] {
                    attempts.push(run.connect(serial, "omni", serial == "hung-device").await);
                }
                assert_eq!(
                    attempts[1..4].iter().map(|a| a.test.as_str()).collect::<BTreeSet<_>>(),
                    BTreeSet::from(["one", "two", "three"])
                );
                for ready in &attempts[1..4] {
                    run.release(ready, TestStatus::Passed).await;
                }
                assert!(
                    run.stopped("hung-device"),
                    "the hung copy was still waiting after all three passed"
                );
                assert_eq!(run.result(), None, "the pool did not join vendor cleanup");
                assert!(attempts[0].cleanup.complete(Ok(())));
                run.replay.drain_current().await.unwrap();
                assert_eq!(run.result(), Some(true));
            })
        },
    );
    let trace = output.trace.lock().unwrap();
    let raw = output.raw();
    assert_eq!(raw.len(), 3, "{raw:?}");
    assert!(
        raw.iter()
            .all(|row| row["status"] == "PASSED" && row["deviceSerial"] != "hung-device")
    );
    assert_eq!(trace.metrics.len(), 3);
    assert!(
        trace
            .metrics
            .iter()
            .all(|(serial, _, status)| serial != "hung-device" && *status == TestStatus::Passed)
    );
    assert!(trace.cleaned.iter().any(|s| s == "hung-device"));
    assert_eq!(trace.seals.len(), trace.cleaned.len());
    assert_eq!(trace.seals.len(), trace.cancelled.len());
    assert!(trace.cancelled.iter().all(|(_, published)| !published));
    assert!(
        output
            .summary()
            .contains("Raw: 3 passed, 0 failed, 0 ignored, 0 incomplete tests")
    );
    assert!(!output.summary().contains("redundant"));
    assert!(output.lines.iter().all(|line| !matches!(
        line,
        Line::Log {
            level: Level::ERROR,
            ..
        }
    )));
    assert_eq!(
        output
            .lines
            .iter()
            .filter(|line| matches!(line,
        Line::Log { level: Level::INFO, message, .. } if message.starts_with("Stopping redundant batch on ")))
            .count(),
        trace.seals.len()
    );
    assert!(output.lines.iter().any(|line| matches!(line,
        Line::Log { level: Level::INFO, message, .. } if message == &format!("Pool omni stopped {} redundant batches", trace.seals.len()))));
    let junit = output.read("tests/omni/marathon_junit_report.xml");
    assert_eq!(junit.matches("<testcase ").count(), 3, "{junit}");
    let root = output.dir.path().join("output");
    let paths = |path: &str| {
        std::fs::read_dir(root.join(path))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>()
    };
    let allure = paths("allure-results")
        .into_iter()
        .filter(|path| path.to_string_lossy().ends_with("-result.json"))
        .collect::<Vec<_>>();
    assert_eq!(allure.len(), 3);
    for path in allure.into_iter().chain(paths("html/timeline")).chain(paths("html")) {
        if path.is_file() {
            let data = std::fs::read(path).unwrap();
            let text = String::from_utf8_lossy(&data);
            assert!(!text.contains(LATE_ARTIFACT));
            let hung = trace.attempts.iter().find(|a| a.serial == "hung-device").unwrap();
            assert!(!text.contains(&hung.batch));
        }
    }
    assert!(!root.join("test_result/omni/hung-device").exists());
}

fn redundant_pair(run: &Run) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
    Box::pin(async move {
        let first = run.connect("first", "omni", false).await;
        let _second = run.connect("second", "omni", false).await;
        run.release(&first, TestStatus::Passed).await;
        assert!(run.stopped("second"), "identical fast run did not stop the copy");
        assert_eq!(run.result(), Some(true));
    })
}

#[test]
fn fast_false_waits_for_every_copy_d107() {
    let output = run(
        Options {
            fast: false,
            ..Options::default()
        },
        |run| {
            Box::pin(async move {
                let first = run.connect("first", "omni", false).await;
                let second = run.connect("second", "omni", false).await;
                run.release(&first, TestStatus::Passed).await;
                assert!(!run.stopped("second"));
                assert_eq!(run.result(), None);
                run.release(&second, TestStatus::Failure).await;
                assert_eq!(run.result(), Some(true));
            })
        },
    );
    assert_eq!(output.raw().len(), 2);
    run(Options::default(), redundant_pair);
}

#[test]
fn an_undecided_test_keeps_both_batches_running_d107() {
    let output = run(
        Options {
            methods: &["one", "two"],
            ..Options::default()
        },
        |run| {
            Box::pin(async move {
                let first = run.connect("first", "omni", false).await;
                let second = run.connect("second", "omni", false).await;
                let held_first = run.connect("held-first", "omni", false).await;
                let held_second = run.connect("held-second", "omni", false).await;
                assert_eq!(
                    (first.test.as_str(), second.test.as_str()),
                    (held_first.test.as_str(), held_second.test.as_str())
                );
                run.release(&first, TestStatus::Passed).await;
                assert!(run.trace.lock().unwrap().seals.is_empty());
                assert_eq!(run.result(), None);
                run.release(&second, TestStatus::Passed).await;
                assert!(
                    run.stopped("held-first") && run.stopped("held-second"),
                    "copies still running after second verdict"
                );
                assert_eq!(run.result(), Some(true));
            })
        },
    );
    assert_eq!(output.raw().len(), 2);
}

#[test]
fn all_success_fast_stops_at_the_deciding_failure_d107() {
    for ignore_failures in [false, true] {
        let output = run(
            Options {
                mode: ExecutionMode::AllSuccess,
                ignore_failures,
                ..Options::default()
            },
            |run| {
                Box::pin(async move {
                    let first = run.connect("first", "omni", false).await;
                    let _second = run.connect("second", "omni", false).await;
                    run.release(&first, TestStatus::Failure).await;
                    assert!(run.stopped("second"), "copy still running after deciding failure");
                    assert_eq!(run.result(), Some(false));
                })
            },
        );
        assert_eq!(output.raw().len(), 1);
        assert_eq!(output.raw()[0]["status"], "FAILURE");
        assert!(output.summary().contains("0 passed, 1 failed, 0 ignored tests"));
    }
}

#[test]
fn only_the_decided_pool_stops_its_copy_d107() {
    let output = run(
        Options {
            separate_pools: true,
            ..Options::default()
        },
        |run| {
            Box::pin(async move {
                let a = run.connect("a", "pool-a", false).await;
                let _held = run.connect("a-held", "pool-a", false).await;
                let b = run.connect("b", "pool-b", false).await;
                run.release(&a, TestStatus::Passed).await;
                assert!(run.stopped("a-held"), "decided pool did not stop");
                assert!(!run.stopped("b"));
                assert_eq!(run.result(), None);
                run.release(&b, TestStatus::Passed).await;
                assert_eq!(run.result(), Some(true));
            })
        },
    );
    assert_eq!(output.trace.lock().unwrap().seals, ["a-held"]);
    assert_eq!(output.raw().len(), 2);
}
