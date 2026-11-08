//! Port of vendor/vendor-android/src/test/kotlin/com/malinskiy/marathon/android/executor/listeners/
//! CompositeTestRunListenerTest.kt, LogListenerTest.kt and TestResultsListenerTest.kt.
mod support;
use jcompat::file_manager::JavaPath;
use marathon_adb::testrunner::{MetricMap, TestEvent as E, TestIdentifier};
use marathon_android::{device::AndroidDevice, listeners::*, services::AndroidRuntime};
use marathon_core::{
    artifacts::{AttachmentCollector, LogListener},
    error::*,
    execution::result::TestRunResultsAccumulator,
    executor::shared::SharedDeferred,
    listener::*,
    model::*,
    runtime::*,
    time::{ClockResult, Timer},
    vendor::*,
};
use std::sync::{Arc, Mutex};
use support::*;
struct ZeroTimer;
impl Timer for ZeroTimer {
    fn start_time_millis(&self) -> i64 {
        0
    }
    fn elapsed_time_millis(&self) -> ClockResult<i64> {
        Ok(0)
    }
    fn current_time_millis(&self) -> ClockResult<i64> {
        Ok(0)
    }
    fn measure(&self, body: &mut dyn FnMut()) -> ClockResult<i64> {
        body();
        Ok(0)
    }
}
fn id(method: &str) -> TestIdentifier {
    TestIdentifier {
        class_name: "com.example.Class".into(),
        test_name: method.into(),
    }
}
fn test(method: &str) -> Test {
    to_test(&id(method))
}
fn end(method: &str) -> E {
    E::TestEnded {
        id: id(method),
        metrics: MetricMap::new(),
    }
}
fn start(method: &str) -> E {
    E::TestStarted { id: id(method) }
}
fn ended() -> E {
    E::TestRunEnded {
        elapsed_time_millis: 1000,
        metrics: MetricMap::new(),
    }
}
fn results_listener(
    runtime: &Arc<AndroidRuntime>,
    device: &AndroidDevice,
    tests: Vec<Test>,
) -> (TestResultsListener, Arc<SharedDeferred<TestBatchResults>>) {
    let deferred = Arc::new(SharedDeferred::default());
    let logging = runtime.clone();
    (
        TestResultsListener {
            accumulator: AccumulatingResultTestRunListener {
                result: Mutex::new(checked(TestRunResultsAccumulator::new(
                    Arc::new(ZeroTimer),
                    Arc::new(|| 0),
                ))),
                logs: Arc::new(move |r| {
                    (logging.log)(&logging.logger(&r.logger), r.level, &r.message, r.cause.as_ref())
                }),
            },
            batch: Arc::new(TestBatch {
                tests,
                id: "batch".into(),
            }),
            device: device.reference(),
            deferred: deferred.clone(),
            attachments: Arc::new(AttachmentCollector::default()),
            log: Arc::new(|_, _| {}),
        },
        deferred,
    )
}
fn run_results(
    tests: Vec<Test>,
    events: Vec<E>,
    verify: impl FnOnce(&TestBatchResults, &TestResultsListener) + Send + 'static,
) {
    run(configuration(), move |runtime, _| {
        Box::pin(async move {
            let mut server = server(base_script(30, vec![])).await;
            let device = device(&server, runtime.clone());
            let (listener, deferred) = results_listener(&runtime, &device, tests);
            checked(listener.before_test_run(None).await);
            for event in events {
                if matches!(&event, E::TestFailed { id, .. } if id.test_name == "method2") {
                    checked(listener.attachments.on_attachment(
                        &test("method2"),
                        Attachment {
                            file: JavaPath::from_text("/log"),
                            kind: AttachmentType::Log,
                            name: None,
                        },
                    ));
                }
                checked(listener.on_event(&event).await);
            }
            checked(listener.after_test_run().await);
            let result = checked(deferred.wait().await);
            verify(&result, &listener);
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
fn assert_result(result: &TestResult, method: &str, status: TestStatus, trace: Option<&str>) {
    assert_eq!(result.test, test(method));
    assert_eq!(result.status, status);
    assert_eq!((result.start_time, result.end_time), (0, 0));
    assert_eq!(result.stacktrace, trace.map(String::from));
    assert_eq!(result.test_batch_id, String::from("batch"));
}
mod test_results_listener_test {
    use super::*;
    #[test]
    fn test_default() {
        run_results(
            (1..=5).map(|i| test(&format!("method{i}"))).collect(),
            vec![
                E::TestFailed {
                    id: id("method0"),
                    stack_trace: "trace".into(),
                },
                E::TestRunStartedEvent { test_count: 3 },
                start("method1"),
                E::TestEnded {
                    id: id("method1"),
                    metrics: [("metric".into(), "value".into())].into_iter().collect(),
                },
                start("method2"),
                E::TestFailed {
                    id: id("method2"),
                    stack_trace: "trace".into(),
                },
                start("method3"),
                E::TestIgnored { id: id("method3") },
                start("method4"),
                E::TestAssumptionFailed {
                    id: id("method4"),
                    stack_trace: "trace".into(),
                },
                start("method5"),
                E::TestRunEnded {
                    elapsed_time_millis: 1234,
                    metrics: [("metric1".into(), "value1".into())].into_iter().collect(),
                },
            ],
            |r, l| {
                assert_eq!((r.finished.len(), r.failed.len(), r.uncompleted.len()), (3, 2, 1));
                for (result, method, status, trace) in [
                    (&r.finished[0], "method1", TestStatus::Passed, None),
                    (&r.finished[1], "method3", TestStatus::Ignored, None),
                    (&r.finished[2], "method4", TestStatus::AssumptionFailure, Some("trace")),
                    (&r.failed[0], "method0", TestStatus::Failure, Some("trace")),
                    (&r.failed[1], "method2", TestStatus::Failure, Some("trace")),
                    (&r.uncompleted[0], "method5", TestStatus::Incomplete, None),
                ] {
                    assert_result(result, method, status, trace);
                }
                let attachment = Attachment {
                    file: JavaPath::from_text("/log"),
                    kind: AttachmentType::Log,
                    name: None,
                };
                assert_eq!(l.attachments.get(&test("method2")), Some(vec![attachment.clone()]));
                assert_eq!(r.failed[1].attachments, vec![attachment]);
            },
        );
    }
    #[test]
    fn test_single_incomplete() {
        run_results(vec![test("method1")], vec![ended()], |r, _| {
            assert!(r.finished.is_empty() && r.failed.is_empty());
            assert_eq!(r.uncompleted.len(), 1);
            assert_result(
                &r.uncompleted[0],
                "method1",
                TestStatus::Incomplete,
                Some(
                    "Test didn't complete. Either the test results are missing due to timeout or testing runtime crashed",
                ),
            );
        });
    }
    fn parameter(events: Vec<E>, status: TestStatus, trace: Option<&'static str>) {
        run_results(vec![test("method")], events, move |r, _| {
            let list = match status {
                TestStatus::Failure => &r.failed,
                TestStatus::Incomplete => &r.uncompleted,
                _ => &r.finished,
            };
            assert_eq!(r.finished.len() + r.failed.len() + r.uncompleted.len(), 1);
            assert_result(&list[0], "method", status, trace);
        });
    }
    #[test]
    fn test_parameterized_success() {
        parameter(
            vec![
                end("method[0]"),
                start("method[1]"),
                end("method[1]"),
                start("method[2]"),
                end("method[2]"),
                ended(),
            ],
            TestStatus::Passed,
            None,
        );
    }
    fn failed_run() {
        run_results(
            vec![test("method1"), test("method2")],
            vec![
                E::TestRunStartedEvent { test_count: 3 },
                start("method1"),
                end("method1"),
                E::TestRunFailed {
                    error: "Problems".into(),
                },
            ],
            |r, _| {
                assert_eq!((r.finished.len(), r.failed.len(), r.uncompleted.len()), (1, 0, 1));
                assert_result(&r.finished[0], "method1", TestStatus::Passed, None);
                assert_result(&r.uncompleted[0], "method2", TestStatus::Incomplete, Some("Problems"));
            },
        );
    }
    #[test]
    fn test_run_failed() {
        failed_run();
    }
    // The Kotlin test named testRunStopped actually calls testRunFailed.
    #[test]
    fn test_run_stopped() {
        failed_run();
    }
    #[test]
    fn test_parameterized_failure() {
        parameter(
            vec![
                end("method[0]"),
                start("method[1]"),
                end("method[1]"),
                start("method[2]"),
                E::TestFailed {
                    id: id("method[2]"),
                    stack_trace: "trace".into(),
                },
                ended(),
            ],
            TestStatus::Failure,
            None,
        );
    }
    #[test]
    fn test_parameterized_failure_accumulation() {
        parameter(
            vec![
                E::TestFailed {
                    id: id("method[0]"),
                    stack_trace: "trace".into(),
                },
                start("method[1]"),
                E::TestFailed {
                    id: id("method[1]"),
                    stack_trace: "trace".into(),
                },
                start("method[2]"),
                E::TestFailed {
                    id: id("method[2]"),
                    stack_trace: "trace".into(),
                },
                ended(),
            ],
            TestStatus::Failure,
            Some("trace"),
        );
    }
    #[test]
    fn test_parameterized_incomplete_accumulation() {
        parameter(
            vec![start("method[0]"), start("method[1]"), start("method[2]"), ended()],
            TestStatus::Incomplete,
            None,
        );
    }
    #[test]
    fn test_parameterized_assumption_failure_accumulation() {
        parameter(
            vec![
                start("method[0]"),
                E::TestAssumptionFailed {
                    id: id("method[0]"),
                    stack_trace: "trace0".into(),
                },
                start("method[1]"),
                E::TestAssumptionFailed {
                    id: id("method[1]"),
                    stack_trace: "trace1".into(),
                },
                start("method[2]"),
                E::TestAssumptionFailed {
                    id: id("method[2]"),
                    stack_trace: "trace2".into(),
                },
                ended(),
            ],
            TestStatus::AssumptionFailure,
            Some("trace0"),
        );
    }
    #[test]
    fn test_parameterized_ignored_accumulation() {
        parameter(
            vec![
                start("method[0]"),
                E::TestIgnored { id: id("method[0]") },
                start("method[1]"),
                E::TestIgnored { id: id("method[1]") },
                start("method[2]"),
                E::TestIgnored { id: id("method[2]") },
                ended(),
            ],
            TestStatus::Ignored,
            None,
        );
    }
    #[test]
    fn metadata_lookup_bug_and_explicit_parameter_disables_all_merging() {
        let mut requested = test("method");
        requested.meta_properties = vec![MetaProperty {
            name: "Annotation".into(),
            values: Default::default(),
        }];
        run_results(
            vec![requested],
            vec![start("method"), end("method"), ended()],
            |r, _| assert!(r.finished[0].test.meta_properties.is_empty()),
        );
        run_results(
            vec![test("method[0]"), test("another")],
            vec![end("method[0]"), end("another[0]"), ended()],
            |r, _| {
                assert_eq!(r.finished[1].test.method, String::from("another[0]"));
                assert_eq!(r.uncompleted[0].test.method, String::from("another"));
            },
        );
    }
    #[test]
    fn stopped_and_unrequested_null_are_distinct() {
        run_results(
            vec![test("method")],
            vec![
                start("method"),
                E::TestFailed {
                    id: id("null"),
                    stack_trace: "beforeClass".into(),
                },
                E::TestRunStopped { elapsed_time_millis: 7 },
            ],
            |r, l| {
                assert_eq!(r.uncompleted.len(), 1);
                assert!(r.failed.is_empty());
                assert_eq!(l.accumulator.result.lock().unwrap().elapsed_time(), 7);
            },
        );
    }
}
#[derive(Default)]
struct Recording {
    events: Mutex<Vec<E>>,
    before: Mutex<usize>,
    after: Mutex<usize>,
    fail: bool,
}
impl AndroidTestRunListener for Recording {
    fn before_test_run<'a>(
        &'a self,
        _: Option<&'a marathon_dex::apk_parser::InstrumentationInfo>,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            *self.before.lock().unwrap() += 1;
            Ok(())
        })
    }
    fn on_event<'a>(&'a self, event: &'a E) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.events.lock().unwrap().push(event.clone());
            if self.fail {
                Err(VendorError::Other(Failure {
                    class: "Callback".into(),
                    message: None,
                    cause: None,
                    stacktrace: None,
                }))
            } else {
                Ok(())
            }
        })
    }
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            *self.after.lock().unwrap() += 1;
            Ok(())
        })
    }
}
mod composite_test_run_listener_test {
    use super::*;
    #[test]
    fn test_default() {
        run(configuration(), |_, _| {
            Box::pin(async {
                let r = Arc::new(Recording::default());
                let composite = CompositeTestRunListener {
                    listeners: vec![r.clone()],
                };
                let events = vec![
                    E::TestRunStartedEvent { test_count: 1 },
                    start("test"),
                    E::TestFailed {
                        id: id("test"),
                        stack_trace: "trace".into(),
                    },
                    end("test"),
                    E::TestAssumptionFailed {
                        id: id("test"),
                        stack_trace: "trace".into(),
                    },
                    E::TestIgnored { id: id("test") },
                    ended(),
                    E::TestRunStopped {
                        elapsed_time_millis: 300,
                    },
                    E::TestRunFailed { error: "error".into() },
                ];
                checked(composite.before_test_run(None).await);
                for e in &events {
                    checked(composite.on_event(e).await);
                }
                checked(composite.after_test_run().await);
                checked(
                    composite
                        .on_event(&E::TestRunFailing {
                            error: "lost".into(),
                            stack_trace: "lost".into(),
                        })
                        .await,
                );
                assert_eq!(*r.events.lock().unwrap(), events);
                assert_eq!(*r.before.lock().unwrap(), 1);
                assert_eq!(*r.after.lock().unwrap(), 1);
                Ok(())
            })
        });
    }
    #[test]
    fn stops_on_first_throw() {
        run(configuration(), |_, _| {
            Box::pin(async {
                let first = Arc::new(Recording {
                    fail: true,
                    ..Default::default()
                });
                let last = Arc::new(Recording::default());
                let c = CompositeTestRunListener {
                    listeners: vec![first, last.clone()],
                };
                assert!(c.on_event(&start("test")).await.is_err());
                assert!(last.events.lock().unwrap().is_empty());
                Ok(())
            })
        });
    }
}
#[derive(Default)]
struct Writer {
    saved: Mutex<Vec<(Option<Test>, Vec<String>)>>,
}
impl LogWriter for Writer {
    fn save_logs<'a>(&'a self, target: &'a LogTarget, logs: &'a [String]) -> ServiceFuture<'a, JavaPath> {
        Box::pin(async move {
            self.saved.lock().unwrap().push((target.test.clone(), logs.to_vec()));
            Ok(JavaPath::from_text(
                if target.test.as_ref().is_some_and(|t| t.method == "method1") {
                    "/log1"
                } else {
                    "/log0"
                },
            ))
        })
    }
    fn append_logs<'a>(
        &'a self,
        _: &'a DevicePoolId,
        _: &'a DeviceInfo,
        _: &'a Test,
        _: &'a str,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
mod log_listener_test {
    use super::*;
    fn exercise(isolation: bool, crash: bool) {
        run(configuration(), move |runtime, _| {
            Box::pin(async move {
                let mut server = server(base_script(30, vec![])).await;
                let device = device(&server, runtime.clone());
                let writer = Arc::new(Writer::default());
                let log = LogListener::new(
                    runtime.dependencies.io.clone(),
                    Arc::new(device.clone()),
                    writer.clone(),
                    LogTarget {
                        pool: DevicePoolId {
                            name: "testpool".into(),
                        },
                        batch_id: "batch".into(),
                        device: device.info(),
                        test: None,
                    },
                    None,
                );
                let provider: Arc<dyn AttachmentProvider> = log.clone();
                let collector = AttachmentCollector::new(std::slice::from_ref(&provider))?;
                let adapter = TestRunListenerAdapter { listener: log.clone() };
                if crash {
                    checked(adapter.before_test_run(None).await);
                    checked(device.on_line("Important crash-related information").await);
                    checked(adapter.after_test_run().await);
                    assert_eq!(
                        *writer.saved.lock().unwrap(),
                        vec![(None, vec!["Important crash-related information\n".into()])]
                    );
                } else {
                    checked(log.on_line("0").await);
                    checked(adapter.on_event(&start("method0")).await);
                    checked(log.on_line("1").await);
                    checked(log.on_line("2").await);
                    checked(adapter.on_event(&end("method0")).await);
                    checked(log.on_line("3").await);
                    assert_eq!(
                        collector.get(&test("method0")),
                        Some(vec![Attachment {
                            file: JavaPath::from_text("/log0"),
                            kind: AttachmentType::Log,
                            name: Some("log".into())
                        }])
                    );
                    if isolation {
                        checked(log.on_line("a").await);
                        checked(adapter.on_event(&start("method1")).await);
                        checked(log.on_line("b").await);
                        checked(log.on_line("c").await);
                        checked(adapter.on_event(&end("method1")).await);
                        checked(log.on_line("d").await);
                        assert_eq!(
                            collector.get(&test("method1")).unwrap()[0].file,
                            JavaPath::from_text("/log1")
                        );
                    }
                    let saved = writer.saved.lock().unwrap();
                    assert_eq!(saved[0].1, vec![String::from("1\n2\n")]);
                    if isolation {
                        assert_eq!(saved[1].1, vec![String::from("b\nc\n")]);
                    }
                }
                checked(server.shutdown().await);
                Ok(())
            })
        });
    }
    #[test]
    fn test_default() {
        exercise(false, false);
    }
    #[test]
    fn test_run_failed() {
        exercise(false, false);
    }
    #[test]
    fn test_batch_test_isolation() {
        exercise(true, false);
    }
    #[test]
    fn test_runner_crash() {
        exercise(false, true);
    }
}
