//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AmInstrumentTestParser.kt
//! (derived integration corpus; no direct Kotlin tests exist).
mod support;
use jcompat::file_manager::JavaPath;
use marathon_android::{
    provider::AdamDeviceProvider,
    remote_parser::{AmInstrumentTestParser, TEST_ANNOTATION_PRODUCER},
};
use marathon_config::vendor::AndroidTestParserConfiguration;
use marathon_core::{
    error::*,
    executor::{Signal, shared::requested},
    runtime::{CancellationSignal, VendorClock, VendorFuture, VendorTimeoutBody},
    vendor::*,
};
use serde_json::{Value, json};
use std::sync::Arc;
use support::*;
fn config() -> marathon_config::Configuration {
    let mut c = configuration();
    let a = android(&mut c);
    a.test_application_output = Some(JavaPath::from_text(
        &fixture("vendor-android/apk/app-debug-androidTest.apk").to_string_lossy(),
    ));
    a.test_parser_configuration = AndroidTestParserConfiguration::Remote {
        instrumentation_args: vec![
            ("listener".into(), TEST_ANNOTATION_PRODUCER.into()),
            ("other".into(), "value".into()),
        ],
    };
    // Real time between output chunks of the fake instrumentation: only a hang guard here; the
    // timeout test sets its own short value.
    c.test_output_timeout_millis = 60_000;
    c
}
fn script(outputs: Vec<Value>) -> Value {
    let mut s = base_script(30, vec!["shell_v2", "cmd"]);
    let list = s["shell"].as_array_mut().unwrap();
    list.push(json!({"exact":"pm list packages","stdout":""}));
    list.push(json!({"prefix":"cmd package install","service":"exec","stdout":"Success\n"}));
    list.extend(outputs);
    s
}
fn stream(method: &str, version: u8) -> String {
    let annotation = match version {
        2 => "[org.junit.Ignore()]",
        3 => "[org.junit.Ignore()]",
        4 => "[18Lorg.junit.Ignore()]",
        _ => "",
    };
    format!(
        "INSTRUMENTATION_STATUS: class=pkg.Clazz\nINSTRUMENTATION_STATUS: test={method}\nINSTRUMENTATION_STATUS: numtests=1\nINSTRUMENTATION_STATUS_CODE: 1\nINSTRUMENTATION_STATUS: class=pkg.Clazz\nINSTRUMENTATION_STATUS: test={method}\nINSTRUMENTATION_STATUS: numtests=1\n{}INSTRUMENTATION_STATUS_CODE: 0\nINSTRUMENTATION_CODE: -1\n",
        if version > 0 {
            format!(
                "INSTRUMENTATION_STATUS: com.malinskiy.adam.junit4.android.listener.TestAnnotationProducer.v{version}={annotation}\nINSTRUMENTATION_STATUS_CODE: 2\nINSTRUMENTATION_STATUS: class=pkg.Clazz\nINSTRUMENTATION_STATUS: test={method}\n"
            )
        } else {
            String::new()
        }
    )
}
#[test]
fn annotations_versions_null_and_no_annotations() {
    for (version, method) in [(2, "method"), (3, "method"), (4, "method"), (0, "null")] {
        run(config(), move |runtime, context| {
            Box::pin(async move {
                let mut server = server(script(vec![
                    json!({"prefix":"am instrument","stdout":stream(method,version)}),
                ]))
                .await;
                let runtime = change_runtime(&runtime, &[server.port()]);
                let provider = Arc::new(AdamDeviceProvider::new(runtime.clone())?);
                provider.initialize(context.clone()).await?;
                let reference = provider.borrow(context.clone()).await?;
                let d = provider.android_device(&reference).unwrap();
                let parser = AmInstrumentTestParser {
                    runtime: runtime.clone(),
                    provider: provider.clone(),
                };
                let tests = parser.extract(reference, context).await?;
                assert_eq!(tests.len(), 1);
                assert_eq!(tests[0].method, js(method));
                assert_eq!(tests[0].meta_properties.len(), usize::from(version > 0));
                assert!(Arc::ptr_eq(
                    &runtime.identifier.android_bundle(&tests[0])?,
                    &runtime.bundles[0]
                ));
                assert_eq!(d.listener_counts(), (0, 0));
                provider.terminate().await?;
                checked(server.shutdown().await);
                assert_eq!(server.warnings(), 0);
                Ok(())
            })
        });
    }
}
#[test]
fn fallback_removes_only_exact_listener_pair_and_retries() {
    for failure in [
        format!(
            "INSTRUMENTATION_STATUS: stream=Could not find extra class {TEST_ANNOTATION_PRODUCER}\nINSTRUMENTATION_STATUS: stack=trace\nINSTRUMENTATION_STATUS_CODE: -1\nINSTRUMENTATION_CODE: 0\n"
        ),
        "INSTRUMENTATION_RESULT: shortMsg=Process crashed\nINSTRUMENTATION_CODE: 0\n".into(),
    ] {
        run(config(), move |runtime, context| {
            Box::pin(async move {
                let mut server = server(script(vec![
                    json!({"regex":"^am instrument.*-e listener ","stdout":failure,"times":1}),
                    json!({"prefix":"am instrument","stdout":stream("method",4)}),
                ]))
                .await;
                let runtime = change_runtime(&runtime, &[server.port()]);
                let provider = Arc::new(AdamDeviceProvider::new(runtime.clone())?);
                provider.initialize(context.clone()).await?;
                let reference = provider.borrow(context.clone()).await?;
                let d = provider.android_device(&reference).unwrap();
                let parser = AmInstrumentTestParser {
                    runtime: runtime.clone(),
                    provider: provider.clone(),
                };
                let tests = parser.extract(reference, context).await?;
                assert_eq!(tests.len(), 1, "failure={failure}; transcript={:?}", commands(&server));
                let requests: Vec<_> = commands(&server)
                    .into_iter()
                    .filter(|s| s.starts_with("am instrument"))
                    .collect();
                assert_eq!(requests.len(), 2, "{requests:?}");
                assert!(requests[0].contains(TEST_ANNOTATION_PRODUCER));
                assert!(!requests[1].contains(" -e listener "));
                assert!(requests[1].contains(" -e log true -e other value"));
                assert_eq!(d.listener_counts(), (0, 0));
                provider.terminate().await?;
                checked(server.shutdown().await);
                assert_eq!(server.warnings(), 0);
                Ok(())
            })
        });
    }
}
// Arm the receive timeout only after the fake has recorded this attempt's
// command. Instrumentation starts on another task, so wall time alone cannot
// guarantee that even a single command reaches the fake before cancellation.
struct HandshakeClock {
    inner: Arc<dyn VendorClock>,
    attempts: tokio::sync::Semaphore,
}
impl VendorClock for HandshakeClock {
    fn delay(&self, millis: i64, signal: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
        self.inner.delay(millis, signal)
    }
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, ()> {
        self.inner.with_timeout(parent, millis, body)
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, Option<()>> {
        Box::pin(async move {
            if millis == 30 {
                checked(self.attempts.acquire().await).forget();
            }
            self.inner.with_timeout_or_null(parent, millis, body).await
        })
    }
}
#[test]
fn timeout_has_three_inner_attempts_and_unregisters_every_attempt() {
    let mut c = config();
    // The fake never answers, so each inner attempt ends on this timeout.
    c.test_output_timeout_millis = 30;
    run(c, |runtime, context| {
        Box::pin(async move {
            let mut server = server(script(vec![json!({"prefix":"am instrument","hang":true})])).await;
            let runtime = change_runtime(&runtime, &[server.port()]);
            let provider = Arc::new(AdamDeviceProvider::new(runtime.clone())?);
            provider.initialize(context.clone()).await?;
            let reference = provider.borrow(context.clone()).await?;
            let d = provider.android_device(&reference).unwrap();
            let parser = AmInstrumentTestParser {
                runtime,
                provider: provider.clone(),
            };
            let clock = Arc::new(HandshakeClock {
                inner: context.clock.clone(),
                attempts: tokio::sync::Semaphore::new(0),
            });
            let context = VendorContext {
                clock: clock.clone(),
                ..context
            };
            let handshake = async {
                for attempt in 1..=3 {
                    while commands(&server)
                        .iter()
                        .filter(|s| s.starts_with("am instrument"))
                        .count()
                        < attempt
                    {
                        // Polls: a yield loop would keep the vendor worker busy while the fake server needs it.
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                    clock.attempts.add_permits(1);
                }
            };
            let (result, ()) = checked(
                tokio::time::timeout(std::time::Duration::from_secs(30), async {
                    tokio::join!(parser.extract(reference, context), handshake)
                })
                .await,
            );
            let e = result.unwrap_err();
            assert!(matches!(e, VendorError::TestParsing(_)));
            assert_eq!(
                e.failure().message,
                Some(js(&format!(
                    "Unable to parse test list using {}",
                    d.serial_number().to_string_lossy()
                )))
            );
            assert_eq!(
                commands(&server)
                    .iter()
                    .filter(|s| s.starts_with("am instrument"))
                    .count(),
                3
            );
            assert_eq!(d.listener_counts(), (0, 0));
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
#[test]
fn cancellation_is_not_retried_and_unregisters() {
    run(config(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(script(vec![json!({"prefix":"am instrument","hang":true})])).await;
            let runtime = change_runtime(&runtime, &[server.port()]);
            let provider = Arc::new(AdamDeviceProvider::new(runtime.clone())?);
            provider.initialize(context.clone()).await?;
            let reference = provider.borrow(context.clone()).await?;
            let d = provider.android_device(&reference).unwrap();
            let signal = Signal::new();
            let child = VendorContext {
                cancellation: signal.clone(),
                ..context
            };
            let parser = AmInstrumentTestParser {
                runtime,
                provider: provider.clone(),
            };
            let parse = parser.extract(reference, child);
            tokio::pin!(parse);
            let error = loop {
                tokio::select! {result=&mut parse=>break result.unwrap_err(),_ = tokio::time::sleep(std::time::Duration::from_millis(5))=>{if commands(&server).iter().any(|s|s.starts_with("am instrument")){signal.cancel(requested("cancel parse"));}}}
            };
            assert!(matches!(error, VendorError::Cancelled(_)));
            assert_eq!(
                commands(&server)
                    .iter()
                    .filter(|s| s.starts_with("am instrument"))
                    .count(),
                1
            );
            assert_eq!(d.listener_counts(), (0, 0));
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
#[test]
fn other_run_failure_returns_partial_tests_and_wrong_device_is_configuration_error() {
    run(config(), |runtime, context| {
        Box::pin(async move {
            let response = format!("{}INSTRUMENTATION_FAILED: Other failure\n", stream("method", 0));
            let mut server = server(script(vec![json!({"prefix":"am instrument","stdout":response})])).await;
            let d = device(&server, runtime.clone());
            d.setup(context.clone()).await?;
            let provider = Arc::new(AdamDeviceProvider::new(runtime.clone())?);
            let parser = AmInstrumentTestParser {
                runtime: runtime.clone(),
                provider,
            };
            let tests = parser
                .parse_tests(&d, runtime.bundles[0].clone(), false, context.clone())
                .await?;
            assert_eq!(tests.len(), 1);
            assert_eq!(d.listener_counts(), (0, 0));
            let error = parser.extract(d.reference(), context).await.unwrap_err();
            assert_eq!(
                error.failure().message,
                Some("Unexpected device type for remote test parsing".into())
            );
            d.dispose().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
#[test]
fn duplicate_test_retains_first_metadata_but_last_bundle_registration() {
    let mut c = config();
    let a = android(&mut c);
    let path = a.test_application_output.take().unwrap();
    let bundle = marathon_config::vendor::AndroidTestBundleConfiguration {
        application: None,
        test_application: path,
        extra_applications: None,
        split_apks: None,
    };
    a.outputs = Some(vec![bundle.clone(), bundle]);
    run(c, |runtime, context| {
        Box::pin(async move {
            let first = stream("method", 4);
            let second = first.replace("18Lorg.junit.Ignore()", "16Lorg.junit.Test()");
            let mut server = server(script(vec![
                json!({"prefix":"am instrument","stdout":first,"times":1}),
                json!({"prefix":"am instrument","stdout":second}),
            ]))
            .await;
            let runtime = change_runtime(&runtime, &[server.port()]);
            let provider = Arc::new(AdamDeviceProvider::new(runtime.clone())?);
            provider.initialize(context.clone()).await?;
            let reference = provider.borrow(context.clone()).await?;
            let parser = AmInstrumentTestParser {
                runtime: runtime.clone(),
                provider: provider.clone(),
            };
            let tests = parser.extract(reference, context).await?;
            assert_eq!(tests.len(), 1);
            assert_eq!(tests[0].meta_properties[0].name, js("org.junit.Ignore"));
            assert!(Arc::ptr_eq(
                &runtime.identifier.android_bundle(&tests[0])?,
                &runtime.bundles[1]
            ));
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
