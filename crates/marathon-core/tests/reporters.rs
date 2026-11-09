//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/raw/RawJsonReporter.kt
//! and report/{device,bill,test,stdout}/ concrete reporters.
use marathon_error::{FailureKind, InjectedKind};
mod event_support;
mod reporter_support;
use reporter_support::*;
#[test]
fn all_six_reporters_match_jvm_bytes_and_complete_manifests() {
    for id in [
        "empty",
        "passed",
        "failed",
        "traces",
        "fail-pass",
        "pass-fail",
        "xml-edges",
        "metadata",
        "multi",
        "fractional",
        "overflow",
        "sort",
        "paths",
        "duration-negative",
        "duration-zero",
        "duration-positive",
        "duplicate-finals",
        "provider-only",
        "prepare-only",
        "fail-json",
        "rv-retried",
        "rv-unicode",
        "rv-no-tests",
    ] {
        compare_scenario(id);
    }
}
#[test]
fn parse_results_match_actual_command_emission() {
    use marathon_core::{model::*, report::projection::parse_results};
    let make = |p: &str, c: &str, m: &str| Test {
        pkg: p.into(),
        clazz: c.into(),
        method: m.into(),
        meta_properties: vec![],
    };
    let expected = row("parse");
    for (id, tests, flaky) in [
        ("empty", vec![], vec![]),
        ("one", vec![make("pkg", "Class", "method")], vec![]),
        ("several", vec![make("p", "C", "a"), make("p", "C", "a")], vec![]),
        ("flaky", vec![metadata_test()], vec![make("p", "C", "f")]),
        (
            "nonfinite",
            vec![Test {
                meta_properties: vec![MetaProperty {
                    name: "ann".into(),
                    values: [("nan".into(), MetaValue::Double(f64::NAN))].into_iter().collect(),
                }],
                ..make("p", "C", "nonfinite")
            }],
            vec![],
        ),
    ] {
        assert_eq!(
            parse_results(&tests, &flaky).as_bytes().to_vec(),
            bytes(&expected["files"][format!("{id}.yml")]),
            "{id}"
        );
    }
}

use marathon_core::{
    analytics::{DelegatingTrackerInternal, ExecutionReportGenerator, TrackerInternal, TrackerInternalAdapter},
    error::*,
    executor::CoreExecutor,
    model::*,
    output::*,
    report::*,
    runtime::*,
};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct FailFiles {
    inner: Arc<dyn OutputFiles>,
    calls: AtomicUsize,
    error: OperationError,
}
impl OutputFiles for FailFiles {
    fn output_dir(&self) -> &std::path::Path {
        self.inner.output_dir()
    }
    fn create_file<'a>(&'a self, target: &'a FileTarget) -> ServiceFuture<'a, PathBuf> {
        Box::pin(async move {
            assert_ne!(std::thread::current().name(), Some("marathon-core"));
            if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
                return Err(self.error.clone());
            }
            self.inner.create_file(target).await
        })
    }
    fn create_folder<'a>(
        &'a self,
        kind: FolderType,
        pool: Option<&'a DevicePoolId>,
        device: Option<&'a DeviceInfo>,
    ) -> ServiceFuture<'a, PathBuf> {
        self.inner.create_folder(kind, pool, device)
    }
    fn create_test_result_file<'a>(&'a self, name: &'a str) -> ServiceFuture<'a, PathBuf> {
        self.inner.create_test_result_file(name)
    }
}
struct Never(Arc<AtomicUsize>);
impl Reporter for Never {
    fn generate<'a>(&'a self, _: &'a ExecutionReport) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
impl TrackerInternalAdapter for Never {
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
#[test]
fn concrete_reporter_failure_keeps_partial_writes_and_only_cancellation_stops_later_closes() {
    for cancelled in [false, true] {
        let temp = Temp::new();
        let path = temp.path();
        let later = Arc::new(AtomicUsize::new(0));
        let remote = Arc::new(AtomicUsize::new(0));
        let (later_copy, remote_copy) = (later.clone(), remote.clone());
        checked(CoreExecutor::run(move |core| {
            Box::pin(async move {
                let mut output = files(core.vendor(), path, 0);
                let failure = Failure {
                    kind: FailureKind::Io,
                    message: Some("second file".into()),
                    cause: Some(Box::new(marathon_core::executor::shared::failure(
                        FailureKind::Socket,
                        "cause retained",
                    ))),
                    stacktrace: None,
                };
                let error = if cancelled {
                    OperationError::Cancelled(Cancellation::Requested(failure))
                } else {
                    OperationError::Failed(failure)
                };
                let file_service = Arc::new(FailFiles {
                    inner: output.files.clone(),
                    calls: AtomicUsize::new(0),
                    error: error.clone(),
                });
                output.files = file_service.clone();
                let generator = Arc::new(ExecutionReportGenerator::new(vec![
                    Arc::new(DeviceInfoJsonReporter { output }),
                    Arc::new(Never(later_copy)),
                ]));
                let pipeline = DelegatingTrackerInternal::new(vec![generator.clone(), Arc::new(Never(remote_copy))]);
                let task = core.vendor().spawn(Box::pin(async move {
                    let first = reporter_support::report(&row("passed")["input"]).device_connected_events()[0].clone();
                    for serial in ["a", "b", "c"] {
                        let mut event = first.clone();
                        event.device.serial_number = serial.into();
                        generator.track(&Event::DeviceConnected(event)).await?;
                    }
                    let actual = pipeline.close().await;
                    assert_eq!(actual, Err(error));
                    assert_eq!(file_service.calls.load(Ordering::SeqCst), 2);
                    Ok(())
                }));
                task.join()
                    .await
                    .map_err(|e| OperationError::Failed(e.failure().clone()))?;
                Ok(())
            })
        }));
        // D51: Kotlin stops at the first failure (C11). Cancellation still stops the rest.
        let expected = usize::from(!cancelled);
        assert_eq!(later.load(Ordering::SeqCst), expected);
        assert_eq!(remote.load(Ordering::SeqCst), expected);
        assert_eq!(
            manifest(&temp.0).keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["devices/myPool/a.json"]
        );
    }
}
#[test]
fn exhausted_timer_propagates_without_writing_stdout_and_empty_pools_never_read_it() {
    use marathon_core::{
        executor::hooks::{ClockMode, RuntimeHooks, Tape},
        time::{ReplayHooks, SystemTimer},
    };
    checked(CoreExecutor::run(move |core| {
        Box::pin(async move {
            let mut tape = Tape {
                wall: ClockMode::Frozen(0),
                ..Tape::default()
            };
            tape.reads.insert("core.di.Clock".into(), vec![0]);
            let hooks = RuntimeHooks::taped(tape, &core.clock());
            let timer = Arc::new(checked(SystemTimer::new(hooks.clone())));
            let sink = Arc::new(Sink::default());
            let reporter = StdoutReporter {
                io: core.vendor(),
                timer,
                sink: sink.clone(),
            };
            reporter
                .generate(&ExecutionReport::new(vec![], vec![], vec![], vec![], vec![]))
                .await?;
            checked(hooks.verify_consumed());
            let r = reporter_support::report(&row("passed")["input"]);
            assert!(matches!(reporter.generate(&r).await, Err(OperationError::Failed(_))));
            assert!(sink.0.lock().unwrap().is_empty());
            Ok(())
        })
    }));
}

#[test]
fn stdout_awaits_async_sink_off_core_without_using_the_blocking_pool() {
    use marathon_core::{
        executor::hooks::{ClockMode, RuntimeHooks, Tape},
        logging::OutputRecord,
        time::SystemTimer,
    };
    struct AsyncSink;
    impl ConsoleSink for AsyncSink {
        fn write(&self, records: Vec<OutputRecord>) -> VendorFuture<'_, ()> {
            Box::pin(async move {
                assert_ne!(std::thread::current().name(), Some("marathon-core"));
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                assert!(matches!(&records[..], [OutputRecord::Raw(_)]));
                Err(VendorError::Other(marathon_core::executor::shared::failure(
                    FailureKind::Injected(InjectedKind::SinkFailure),
                    "after suspension",
                )))
            })
        }
    }
    checked(CoreExecutor::run(move |core| {
        Box::pin(async move {
            let hooks = RuntimeHooks::taped(
                Tape {
                    wall: ClockMode::Frozen(0),
                    ..Tape::default()
                },
                &core.clock(),
            );
            let io = event_support::CountingRuntime::new(core.vendor());
            let reporter = StdoutReporter::new(
                io.clone(),
                Arc::new(checked(SystemTimer::new(hooks))),
                Arc::new(AsyncSink),
            );
            let report = reporter_support::report(&row("passed")["input"]);
            let Err(OperationError::Failed(error)) = reporter.generate(&report).await else {
                panic!("sink failure was lost");
            };
            assert_eq!(error.kind.legacy_class(), String::from("SinkFailure"));
            assert_eq!(error.message, Some("after suspension".into()));
            assert_eq!(io.spawns.load(Ordering::SeqCst), 1);
            assert_eq!(io.blocking.load(Ordering::SeqCst), 0);
            Ok(())
        })
    }));
}

#[test]
fn malformed_primitive_array_metadata_falls_back_to_generic_projection() {
    use marathon_core::report::projection::{meta, parse_results};
    for class in ["[B", "[C"] {
        let values = vec![
            MetaValue::Byte(-1),
            MetaValue::Char('x' as u16),
            MetaValue::Double(f64::NAN),
        ];
        let array = MetaValue::Array(Arc::new(MetaArray {
            jvm_class: class.into(),
            elements: values.clone(),
        }));
        let list = MetaValue::List(values);
        assert_eq!(meta(&array), meta(&list));
        let make = |value| Test {
            pkg: "p".into(),
            clazz: "C".into(),
            method: "m".into(),
            meta_properties: vec![MetaProperty {
                name: "ann".into(),
                values: [("value".into(), value)].into_iter().collect(),
            }],
        };
        assert_eq!(parse_results(&[make(array)], &[]), parse_results(&[make(list)], &[]));
    }
}

#[test]
fn stdout_summary_contract() {
    let (_, output, _) = generate("failed");
    assert_eq!(output, b"Marathon run finished:\nDevice pool myPool:\n\t0 passed, 3 failed, 0 ignored tests\n\tFailed tests:\n\t\tcom.example#test1\n\t\tcom.example#test2\n\t\tcom.example#test3\n\tFlakiness overhead: 0H 00m 11s\n\tRaw: 0 passed, 3 failed, 0 ignored, 1 incomplete tests\n\tFailed tests:\n\t\tcom.example#test1 failed 1 time(s)\n\t\tcom.example#test2 failed 1 time(s)\n\t\tcom.example#test3 failed 1 time(s)\n\tIncomplete tests:\n\t\tcom.example#test2 incomplete 1 time(s)\nTotal time: 0H 00m 00s\n\n");
}
