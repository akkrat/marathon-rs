//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AndroidDeviceTestRunner.kt
//! (derived runner/argument/cleanup integration corpus; this class has no Kotlin tests).
mod support;
use jcompat::{file_manager::JavaPath, logging::Level};
use marathon_adb::testrunner::TestEvent as E;
use marathon_android::{arguments, device::AndroidBatchExecutor, listener_factory::*, listeners::*, runner::*};
use marathon_config::vendor::TestAccessConfiguration;
use marathon_core::{
    error::*,
    executor::{
        Signal,
        shared::{SharedDeferred, requested},
    },
    model::*,
    runtime::*,
    vendor::*,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use support::*;
fn oracle(name: &str) -> Value {
    include_str!("../../../fixtures/oracle/android-runner-probes.jsonl")
        .lines()
        .map(|l| checked(serde_json::from_str::<Value>(l)))
        .find(|v| v["case"] == name)
        .unwrap()["value"]
        .clone()
}
#[test]
fn argument_corpus() {
    for serial in [
        "emulator-5554",
        "emulator- +42 ",
        "emulator-2147483647",
        "emulator-2147483648",
        "emulator-bad",
        "physical",
        "127.0.0.1:5555",
    ] {
        for mask in 0..8 {
            let access = TestAccessConfiguration {
                adb: mask & 1 != 0,
                console: mask & 2 != 0,
                grpc: mask & 4 != 0,
                console_token: " token ".into(),
            };
            let args = vec![
                ("coverage".to_owned(), "user".into()),
                (marathon_adb::instrument_request::ADB_PORT.to_owned(), "user".into()),
                ("class".to_owned(), "user".into()),
            ];
            let map = arguments::generate(serial, 12345, None, true, &args, &access, |_, _| {});
            let pairs: Vec<_> = map
                .entries()
                .iter()
                .map(|(k, v)| vec![k.to_string_lossy(), v.to_string_lossy()])
                .collect();
            assert_eq!(json!(pairs), oracle(&format!("args:{serial}:{mask}")));
        }
    }
    let access = TestAccessConfiguration {
        adb: true,
        console: false,
        grpc: false,
        console_token: "".into(),
    };
    let map = arguments::generate("physical", 12345, Some(24680), false, &[], &access, |_, _| {});
    assert_eq!(
        json!(
            map.entries()
                .iter()
                .map(|(k, v)| vec![k.to_string_lossy(), v.to_string_lossy()])
                .collect::<Vec<_>>()
        ),
        oracle("args:reverse")
    );
    let mut logs = vec![];
    let mut access = access;
    access.grpc = true;
    access.console = true;
    arguments::generate("physical", 12345, None, false, &[], &access, |l, s| logs.push((l, s)));
    assert_eq!(logs.len(), 4);
    assert!(logs.iter().all(|(l, _)| *l == Level::Warn));
}
#[derive(Default)]
struct Events {
    values: Mutex<Vec<String>>,
    fail_event: bool,
    fail_after: bool,
    cancel_on_start: Option<Arc<Signal>>,
}
impl AndroidTestRunListener for Events {
    fn before_test_run<'a>(
        &'a self,
        info: Option<&'a marathon_dex::apk_parser::InstrumentationInfo>,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.values.lock().unwrap().push(format!(
                "before:{}",
                info.map(|i| i.instrumentation_package.to_string_lossy())
                    .unwrap_or_else(|| "null".into())
            ));
            Ok(())
        })
    }
    fn on_event<'a>(&'a self, event: &'a E) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let value = match event {
                E::TestRunStartedEvent { test_count } => format!("run:AdamTestRun:{test_count}"),
                E::TestStarted { id } => format!("start:{}", id.test_name.to_string_lossy()),
                E::TestIgnored { id } => format!("ignored:{}", id.test_name.to_string_lossy()),
                E::TestEnded { id, .. } => format!("end:{}", id.test_name.to_string_lossy()),
                E::TestRunFailed { error } => format!("failed:{}", error.to_string_lossy()),
                E::TestRunEnded {
                    elapsed_time_millis, ..
                } => format!("ended:{elapsed_time_millis}"),
                _ => "other".into(),
            };
            self.values.lock().unwrap().push(value);
            if matches!(event, E::TestStarted { .. }) {
                if let Some(signal) = &self.cancel_on_start {
                    signal.cancel(requested("external"));
                }
                if self.fail_event {
                    return Err(VendorError::Other(Failure {
                        class: "Callback".into(),
                        message: Some("callback".into()),
                        cause: None,
                        stacktrace: None,
                    }));
                }
            }
            Ok(())
        })
    }
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.values.lock().unwrap().push("after".into());
            if self.fail_after {
                Err(VendorError::Other(Failure {
                    class: "After".into(),
                    message: Some("after".into()),
                    cause: None,
                    stacktrace: None,
                }))
            } else {
                Ok(())
            }
        })
    }
}
fn test(method: &str) -> Test {
    Test {
        pkg: "pkg".into(),
        clazz: "Clazz".into(),
        method: method.into(),
        meta_properties: vec![],
    }
}
fn ignored() -> Test {
    let mut t = test("ignored");
    t.meta_properties.push(MetaProperty {
        name: "org.junit.Ignore".into(),
        values: Default::default(),
    });
    t
}
fn config() -> marathon_config::Configuration {
    let mut c = configuration();
    let a = android(&mut c);
    a.application_pm_clear = false;
    a.test_application_pm_clear = false;
    a.screen_record_configuration.video_configuration.enabled = false;
    a.screen_record_configuration.screenshot_configuration.enabled = false;
    a.test_application_output = Some(JavaPath::from_text(
        &fixture("vendor-android/apk/app-debug-androidTest.apk").to_string_lossy(),
    ));
    c
}
fn script(output: &str, hang: bool) -> Value {
    let mut s = base_script(30, vec!["shell_v2", "cmd", "abb_exec", "sendrecv_v2"]);
    s["shell"]
        .as_array_mut()
        .unwrap()
        .push(json!({"prefix":"am instrument","stdout":output,"hang":hang}));
    s
}
fn register(runtime: &Arc<marathon_android::services::AndroidRuntime>, tests: &[Test]) {
    for t in tests {
        runtime.identifier.put(t, &runtime.bundles[0]);
    }
}
fn stream() -> &'static str {
    "INSTRUMENTATION_STATUS: class=pkg.Clazz\nINSTRUMENTATION_STATUS: test=method\nINSTRUMENTATION_STATUS: numtests=1\nINSTRUMENTATION_STATUS_CODE: 1\n"
}
#[test]
fn empty_and_ignored_never_instrument() {
    run(config(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(30, vec![])).await;
            let d = device(&server, runtime);
            let e = Events::default();
            AndroidDeviceTestRunner { device: &d }
                .execute(
                    d.runtime().dependencies.configuration.clone(),
                    Arc::new(TestBatch {
                        tests: vec![ignored()],
                        id: "batch".into(),
                    }),
                    &e,
                    context,
                )
                .await?;
            assert_eq!(json!(*e.values.lock().unwrap()), oracle("runner:all-ignored"));
            assert!(commands(&server).is_empty());
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
#[test]
fn request_escapes_only_ascii_spaces_and_keeps_null() {
    let mut c = config();
    c.is_code_coverage_enabled = true;
    run(c, |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(30, vec![])).await;
            let d = device(&server, runtime.clone());
            let bundle = runtime.bundles[0].clone();
            let info = tokio::task::spawn_blocking(move || bundle.instrumentation_info())
                .await
                .unwrap()?;
            let request = AndroidDeviceTestRunner { device: &d }.prepare_request(
                &runtime.dependencies.configuration,
                &info,
                &TestBatch {
                    tests: vec![test("null"), test("space name")],
                    id: "batch".into(),
                },
            )?;
            let text = request.command().to_string_lossy();
            assert!(text.contains("-e class pkg.Clazz,pkg.Clazz#space\\ name"));
            assert!(text.contains("/coverage/coverage-batch.ec"));
            assert_eq!(request.socket_idle_timeout, Some(i64::MAX));
            d.dispose().await?;
            checked(server.shutdown().await);
            let _ = context;
            Ok(())
        })
    });
}
#[test]
fn after_exception_wins_and_cleanup_still_closes_producer() {
    run(config(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(script(stream(), true)).await;
            let (runtime, active) = counted_runtime(runtime);
            let d = device(&server, runtime.clone());
            d.setup(context.clone()).await?;
            register(&runtime, &[test("method")]);
            let e = Events {
                fail_event: true,
                fail_after: true,
                ..Default::default()
            };
            let error = AndroidDeviceTestRunner { device: &d }
                .execute(
                    runtime.dependencies.configuration.clone(),
                    Arc::new(TestBatch {
                        tests: vec![test("method")],
                        id: "batch".into(),
                    }),
                    &e,
                    context,
                )
                .await
                .unwrap_err();
            assert_eq!(error.failure().message, Some("after".into()));
            // Only the device's logcat producer survives; instrumentation was joined.
            assert_eq!(active.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(e.values.lock().unwrap().last().unwrap(), "after");
            d.dispose().await?;
            checked(server.shutdown().await);
            assert_eq!(server.warnings(), 0);
            Ok(())
        })
    });
}
#[test]
fn output_and_batch_timeout_and_external_cancel() {
    for (output, batch, external) in [(10, 1000, false), (1000, 10, false), (1000, 1000, true)] {
        let mut c = config();
        c.test_output_timeout_millis = output;
        c.test_batch_timeout_millis = batch;
        run(c, move |runtime, context| {
            Box::pin(async move {
                let mut server = server(script(if external { stream() } else { "" }, true)).await;
                let d = device(&server, runtime.clone());
                d.setup(context.clone()).await?;
                register(&runtime, &[test("method")]);
                let signal = Signal::new();
                let e = Events {
                    cancel_on_start: external.then_some(signal.clone()),
                    ..Default::default()
                };
                let context = VendorContext {
                    cancellation: signal,
                    ..context
                };
                let result = AndroidDeviceTestRunner { device: &d }
                    .execute(
                        runtime.dependencies.configuration.clone(),
                        Arc::new(TestBatch {
                            tests: vec![test("method")],
                            id: "batch".into(),
                        }),
                        &e,
                        context,
                    )
                    .await;
                if external {
                    assert!(matches!(result, Err(VendorError::Cancelled(_))));
                } else {
                    checked(result);
                    assert!(
                        e.values
                            .lock()
                            .unwrap()
                            .iter()
                            .any(|s| s == &format!("failed:{ERROR_STUCK}"))
                    );
                }
                assert_eq!(e.values.lock().unwrap().last().unwrap(), "after");
                d.dispose().await?;
                checked(server.shutdown().await);
                Ok(())
            })
        });
    }
}
struct AfterFailure;
impl AndroidListenerFactory for AfterFailure {
    fn create(
        &self,
        _: &marathon_android::device::AndroidDevice,
        _: &marathon_config::Configuration,
        _: &DevicePoolId,
        _: &BatchRef,
    ) -> VendorResult<InstalledListener> {
        Ok(InstalledListener {
            listener: Arc::new(Events {
                fail_after: true,
                ..Default::default()
            }),
            attachments: None,
        })
    }
}
#[test]
fn result_completion_survives_later_after_failure() {
    run(config(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(30, vec![])).await;
            let d = device(&server, runtime.clone());
            let deferred = Arc::new(SharedDeferred::default());
            let executor = AndroidRunnerExecutor {
                slots: ListenerSlots {
                    screen_capture: Some(Arc::new(AfterFailure)),
                    ..Default::default()
                },
            };
            let result = executor
                .execute(
                    &d,
                    runtime.dependencies.configuration.clone(),
                    DevicePoolId { name: "pool".into() },
                    Arc::new(TestBatch {
                        tests: vec![ignored()],
                        id: "batch".into(),
                    }),
                    deferred.clone(),
                    context,
                )
                .await;
            assert_eq!(result.unwrap_err().failure().message, Some("after".into()));
            assert_eq!(checked(deferred.wait().await).finished[0].status, TestStatus::Ignored);
            assert!(!deferred.cancel(requested("late")));
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
#[test]
fn explicitly_removed_enabled_listener_fails() {
    let mut c = config();
    android(&mut c).allure_configuration.enabled = true;
    run(c, |runtime, _| {
        Box::pin(async move {
            let mut server = server(base_script(30, vec![])).await;
            let d = device(&server, runtime.clone());
            let error = match create(
                &d,
                &runtime.dependencies.configuration,
                DevicePoolId { name: "pool".into() },
                Arc::new(TestBatch {
                    tests: vec![],
                    id: "batch".into(),
                }),
                Arc::new(SharedDeferred::default()),
                &ListenerSlots {
                    file_sync: None,
                    ..Default::default()
                },
            ) {
                Ok(_) => panic!("missing file sync accepted"),
                Err(e) => e,
            };
            assert_eq!(
                error.failure().message,
                Some("Android file sync listener is not installed".into())
            );
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
#[test]
fn producer_parser_failure_fails_the_run_and_keeps_the_device_d25() {
    run(config(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(script(
                "INSTRUMENTATION_STATUS: numtests=many\nINSTRUMENTATION_STATUS_CODE: 1\n",
                false,
            ))
            .await;
            let d = device(&server, runtime.clone());
            d.setup(context.clone()).await?;
            register(&runtime, &[test("method")]);
            let e = Events::default();
            AndroidDeviceTestRunner { device: &d }
                .execute(
                    runtime.dependencies.configuration.clone(),
                    Arc::new(TestBatch {
                        tests: vec![test("method")],
                        id: "batch".into(),
                    }),
                    &e,
                    context,
                )
                .await?;
            // The JVM row `runner:handshake-eof-events` shows no failure at all (N38).
            assert_eq!(
                *e.values.lock().unwrap(),
                vec![
                    "before:com.example.test",
                    "failed:Instrumentation failed: java.lang.NumberFormatException: For input string: \"many\"",
                    "after"
                ]
            );
            d.dispose().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
/// D25: a rejected handshake reaches the listener with its cause and then means what it means in a synchronous request:
/// the device is lost, through `execution_error`.
#[test]
fn producer_handshake_rejection_fails_the_run_and_loses_the_device_d25() {
    run(config(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(script("INSTRUMENTATION_CODE: -1\n", false)).await;
            let d = device(&server, runtime.clone());
            d.setup(context.clone()).await?;
            register(&runtime, &[test("method")]);
            checked(server.disconnect_device("emulator-5554"));
            let e = Events::default();
            let error = AndroidDeviceTestRunner { device: &d }
                .execute(
                    runtime.dependencies.configuration.clone(),
                    Arc::new(TestBatch {
                        tests: vec![test("method")],
                        id: "batch".into(),
                    }),
                    &e,
                    context,
                )
                .await
                .unwrap_err();
            let values = e.values.lock().unwrap().clone();
            assert_eq!(values.len(), 3, "{values:?}");
            assert!(
                values[1].starts_with(
                    "failed:Instrumentation failed: com.malinskiy.adam.exception.RequestRejectedException: "
                ),
                "{values:?}"
            );
            assert_eq!(values[2], "after");
            assert!(matches!(
                marathon_android::execution_error(error),
                VendorError::DeviceLost(_)
            ));
            d.dispose().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
/// D25: a refused connection is an I/O failure, which fails the run and leaves the device, as a synchronous one does.
#[test]
fn producer_connection_failure_fails_the_run_and_keeps_the_device_d25() {
    run(config(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(script("INSTRUMENTATION_CODE: -1\n", false)).await;
            let d = device(&server, runtime.clone());
            d.setup(context.clone()).await?;
            register(&runtime, &[test("method")]);
            logcat_streams(&[(&server, 1)]).await;
            checked(server.shutdown().await);
            let e = Events::default();
            AndroidDeviceTestRunner { device: &d }
                .execute(
                    runtime.dependencies.configuration.clone(),
                    Arc::new(TestBatch {
                        tests: vec![test("method")],
                        id: "batch".into(),
                    }),
                    &e,
                    context,
                )
                .await?;
            let values = e.values.lock().unwrap().clone();
            assert_eq!(values.len(), 3, "{values:?}");
            assert!(values[1].contains("Connection refused"), "{values:?}");
            d.dispose().await?;
            Ok(())
        })
    });
}
/// D25: what the run's accounting does with it: the unfinished test is uncompleted, carrying the real cause.
#[test]
fn producer_failure_leaves_the_tests_uncompleted_with_the_cause_d25() {
    run(config(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(script(
                "INSTRUMENTATION_STATUS: numtests=many\nINSTRUMENTATION_STATUS_CODE: 1\n",
                false,
            ))
            .await;
            let d = device(&server, runtime.clone());
            d.setup(context.clone()).await?;
            register(&runtime, &[test("method")]);
            let batch = Arc::new(TestBatch {
                tests: vec![test("method")],
                id: "batch".into(),
            });
            let deferred = Arc::new(SharedDeferred::default());
            let accumulator = marathon_core::execution::result::TestRunResultsAccumulator::new(
                runtime.dependencies.timer.clone(),
                Arc::new(marathon_core::logging::host_millis),
            )
            .unwrap();
            let listener = TestResultsListener {
                accumulator: AccumulatingResultTestRunListener {
                    result: Mutex::new(accumulator),
                    logs: Arc::new(|_| {}),
                },
                batch: batch.clone(),
                device: d.reference(),
                deferred: deferred.clone(),
                attachments: TestResultsListener::collector(&[])?,
                log: Arc::new(|_, _| {}),
            };
            AndroidDeviceTestRunner { device: &d }
                .execute(runtime.dependencies.configuration.clone(), batch, &listener, context)
                .await?;
            let results = deferred.completed().unwrap().unwrap();
            assert_eq!(results.uncompleted.len(), 1);
            assert_eq!(
                results.uncompleted[0].stacktrace.as_ref().unwrap().to_string_lossy(),
                "Instrumentation failed: java.lang.NumberFormatException: For input string: \"many\""
            );
            d.dispose().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
#[test]
fn multiple_instrumentations_each_receive_the_entire_nonignored_batch() {
    run(config(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(script("INSTRUMENTATION_CODE: -1\n", false)).await;
            let d = device(&server, runtime.clone());
            d.setup(context.clone()).await?;
            let one = test("one");
            let two = test("two");
            runtime.identifier.put(&one, &runtime.bundles[0]);
            let second = Arc::new(marathon_android::models::AndroidTestBundle::new(
                marathon_android::dex_test_parser::DexTestBundleFiles {
                    application: None,
                    test_application: JavaPath::from_text(
                        &fixture("dex/mutants/manifest-relative-bare.apk").to_string_lossy(),
                    ),
                    extra_applications: vec![],
                    split_apks: vec![],
                },
                &JavaPath::from_text("/"),
            ));
            runtime.identifier.put(&two, &second);
            let events = Events::default();
            AndroidDeviceTestRunner { device: &d }
                .execute(
                    runtime.dependencies.configuration.clone(),
                    Arc::new(TestBatch {
                        tests: vec![one, two, ignored()],
                        id: "batch".into(),
                    }),
                    &events,
                    context,
                )
                .await?;
            let commands: Vec<_> = commands(&server)
                .into_iter()
                .filter(|s| s.starts_with("am instrument"))
                .collect();
            assert_eq!(commands.len(), 2);
            assert_ne!(commands[0], commands[1]);
            for command in commands {
                assert!(command.contains(" -e class pkg.Clazz#one,pkg.Clazz#two "));
            }
            assert_eq!(
                events
                    .values
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|s| s.as_str() == "ignored:ignored")
                    .count(),
                2
            );
            d.dispose().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
struct InjectLineFactory;
struct InjectLine(marathon_android::device::AndroidDevice);
impl AndroidListenerFactory for InjectLineFactory {
    fn create(
        &self,
        device: &marathon_android::device::AndroidDevice,
        _: &marathon_config::Configuration,
        _: &DevicePoolId,
        _: &BatchRef,
    ) -> VendorResult<InstalledListener> {
        Ok(InstalledListener {
            listener: Arc::new(InjectLine(device.clone())),
            attachments: None,
        })
    }
}
impl AndroidTestRunListener for InjectLine {
    fn on_event<'a>(&'a self, event: &'a E) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if matches!(event, E::TestStarted { .. }) {
                self.0.on_line(&"hello".into()).await?;
            }
            Ok(())
        })
    }
}
#[test]
fn logcat_attachment_is_saved_before_result_completion_and_listener_is_removed() {
    let temp = checked(tempfile::tempdir());
    let mut c = config();
    c.output_dir = JavaPath::from_text(&temp.path().to_string_lossy());
    run(c, move |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(30, vec![])).await;
            let d = device(&server, runtime.clone());
            let deferred = Arc::new(SharedDeferred::default());
            AndroidRunnerExecutor {
                slots: ListenerSlots {
                    screen_capture: Some(Arc::new(InjectLineFactory)),
                    ..Default::default()
                },
            }
            .execute(
                &d,
                runtime.dependencies.configuration.clone(),
                DevicePoolId { name: "pool".into() },
                Arc::new(TestBatch {
                    tests: vec![ignored()],
                    id: "batch".into(),
                }),
                deferred.clone(),
                context,
            )
            .await?;
            let result = checked(deferred.wait().await);
            let attachment = &result.finished[0].attachments[0];
            assert_eq!(attachment.name, Some("logcat".into()));
            assert_eq!(attachment.kind, AttachmentType::Log);
            assert_eq!(
                checked(tokio::fs::read_to_string(checked(attachment.file.to_file_path_buf())).await),
                "hello\n"
            );
            assert_eq!(d.listener_counts(), (0, 0));
            checked(server.shutdown().await);
            drop(temp);
            Ok(())
        })
    });
}
struct RejectFactory;
impl AndroidListenerFactory for RejectFactory {
    fn create(
        &self,
        _: &marathon_android::device::AndroidDevice,
        _: &marathon_config::Configuration,
        _: &DevicePoolId,
        _: &BatchRef,
    ) -> VendorResult<InstalledListener> {
        Err(VendorError::Other(Failure {
            class: "com.malinskiy.adam.exception.RequestRejectedException".into(),
            message: Some("rejected".into()),
            cause: None,
            stacktrace: None,
        }))
    }
}
#[test]
fn exception_wrapper_includes_listener_creation_and_preserves_cause() {
    run(config(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(30, vec![])).await;
            let d = device(&server, runtime.clone());
            let deferred = Arc::new(SharedDeferred::default());
            let executor = AndroidRunnerExecutor {
                slots: ListenerSlots {
                    recorder: Some(Arc::new(RejectFactory)),
                    ..Default::default()
                },
            };
            let error = executor
                .execute(
                    &d,
                    runtime.dependencies.configuration.clone(),
                    DevicePoolId { name: "pool".into() },
                    Arc::new(TestBatch {
                        tests: vec![],
                        id: "batch".into(),
                    }),
                    deferred.clone(),
                    context,
                )
                .await
                .unwrap_err();
            assert!(matches!(error, VendorError::DeviceLost(_)));
            assert_eq!(
                error.failure().cause.as_ref().unwrap().class,
                js("com.malinskiy.adam.exception.RequestRejectedException")
            );
            assert!(deferred.completed().is_none());
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
#[test]
fn closed_client_producer_error_matches_jvm_and_does_not_become_device_loss() {
    run(config(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(30, vec![])).await;
            let d = device(&server, runtime.clone());
            d.setup(context.clone()).await?;
            register(&runtime, &[test("method")]);
            d.client().close();
            let events = Events::default();
            AndroidDeviceTestRunner { device: &d }
                .execute(
                    runtime.dependencies.configuration.clone(),
                    Arc::new(TestBatch {
                        tests: vec![test("method")],
                        id: "batch".into(),
                    }),
                    &events,
                    context,
                )
                .await?;
            assert_eq!(*events.values.lock().unwrap(), vec!["before:com.example.test", "after"]);
            d.dispose().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

struct CountedRuntime {
    inner: Arc<dyn VendorRuntime>,
    active: Arc<std::sync::atomic::AtomicUsize>,
}
impl VendorRuntime for CountedRuntime {
    fn spawn_cancellable(&self, body: VendorTaskBody) -> Arc<dyn VendorTask> {
        let active = self.active.clone();
        active.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.spawn_cancellable(Box::new(move |signal| {
            Box::pin(async move {
                let result = body(signal).await;
                active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                result
            })
        }))
    }
    fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn VendorTask> {
        self.inner.spawn(body)
    }
    fn spawn_blocking(&self, body: Box<dyn FnOnce() -> VendorResult<()> + Send>) -> Arc<dyn VendorTask> {
        self.inner.spawn_blocking(body)
    }
}
fn counted_runtime(
    runtime: Arc<marathon_android::services::AndroidRuntime>,
) -> (
    Arc<marathon_android::services::AndroidRuntime>,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut deps = runtime.dependencies.clone();
    deps.io = Arc::new(CountedRuntime {
        inner: deps.io,
        active: active.clone(),
    });
    (
        Arc::new(marathon_android::services::AndroidRuntime {
            dependencies: deps,
            configuration: runtime.configuration.clone(),
            file_sync: runtime.file_sync.clone(),
            bundles: runtime.bundles.clone(),
            identifier: runtime.identifier.clone(),
            executor: runtime.executor.clone(),
            log: runtime.log.clone(),
        }),
        active,
    )
}
struct SuspendingHook {
    events: Mutex<Vec<String>>,
}
impl AndroidTestRunListener for SuspendingHook {
    fn before_test_run_in<'a>(
        &'a self,
        _: Option<&'a marathon_dex::apk_parser::InstrumentationInfo>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.events.lock().unwrap().push("before".into());
            let result = context.clock.delay(10_000, context.cancellation).await;
            self.events.lock().unwrap().push("before-cleanup".into());
            result
        })
    }
    fn on_event<'a>(&'a self, event: &'a E) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if let E::TestRunFailed { error } = event {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("failed:{}", error.to_string_lossy()));
            }
            Ok(())
        })
    }
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.events.lock().unwrap().push("after".into());
            Ok(())
        })
    }
}
#[test]
fn batch_timeout_cancels_suspended_hook_then_awaits_cleanup_before_after() {
    let mut c = config();
    c.test_batch_timeout_millis = 10;
    run(c, |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(30, vec![])).await;
            let d = device(&server, runtime.clone());
            register(&runtime, &[test("method")]);
            let listener = Arc::new(SuspendingHook {
                events: Mutex::new(vec![]),
            });
            let composite = CompositeTestRunListener {
                listeners: vec![listener.clone()],
            };
            AndroidDeviceTestRunner { device: &d }
                .execute(
                    runtime.dependencies.configuration.clone(),
                    Arc::new(TestBatch {
                        tests: vec![test("method")],
                        id: "batch".into(),
                    }),
                    &composite,
                    context,
                )
                .await?;
            assert_eq!(
                json!(*listener.events.lock().unwrap()),
                oracle("runner:suspended-hook-timeout")
            );
            assert!(commands(&server).is_empty());
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
#[test]
fn ignored_metadata_removes_equal_unannotated_duplicate() {
    run(config(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(30, vec![])).await;
            let d = device(&server, runtime.clone());
            let ignored = ignored();
            let mut plain = ignored.clone();
            plain.meta_properties.clear();
            let events = Events::default();
            AndroidDeviceTestRunner { device: &d }
                .execute(
                    runtime.dependencies.configuration.clone(),
                    Arc::new(TestBatch {
                        tests: vec![ignored, plain],
                        id: "batch".into(),
                    }),
                    &events,
                    context,
                )
                .await?;
            assert_eq!(json!(*events.values.lock().unwrap()), oracle("runner:ignored-equality"));
            assert!(commands(&server).is_empty());
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

#[test]
fn external_storage_pull_grants_legacy_storage_only_at_api29() {
    for sdk in [28, 29] {
        use marathon_config::vendor::{AggregationMode, FileSyncEntry, PathRoot};
        let mut c = config();
        android(&mut c).file_sync_configuration.pull.push(FileSyncEntry {
            relative_path: "reports".into(),
            path_root: PathRoot::ExternalStorage,
            aggregation_mode: AggregationMode::Device,
        });
        run(c, move |runtime, context| {
            Box::pin(async move {
                let mut s = script(stream(), false);
                s["devices"][0]["getprop"]["ro.build.version.sdk"] = json!(sdk);
                s["shell"].as_array_mut().unwrap().push(json!({
                    "exact": "appops set --uid com.example LEGACY_STORAGE allow",
                    "stdout": ""
                }));
                let mut server = server(s).await;
                let d = device(&server, runtime.clone());
                d.setup(context.clone()).await?;
                assert_eq!(d.api_level(), sdk);
                register(&runtime, &[test("method")]);
                AndroidDeviceTestRunner { device: &d }
                    .execute(
                        runtime.dependencies.configuration.clone(),
                        Arc::new(TestBatch {
                            tests: vec![test("method")],
                            id: "batch".into(),
                        }),
                        &Events::default(),
                        context,
                    )
                    .await?;
                let commands = commands(&server);
                assert!(commands.iter().any(|c| c.starts_with("am instrument")));
                let appops: Vec<_> = commands
                    .iter()
                    .filter(|c| c.starts_with("appops"))
                    .map(String::as_str)
                    .collect();
                assert_eq!(
                    appops,
                    if sdk == 29 {
                        vec!["appops set --uid com.example LEGACY_STORAGE allow"]
                    } else {
                        vec![]
                    }
                );
                d.dispose().await?;
                checked(server.shutdown().await);
                Ok(())
            })
        });
    }
}
