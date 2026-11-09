//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AmInstrumentTestParser.kt
//! (derived integration corpus; no direct Kotlin tests exist).
mod support;
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
use std::path::PathBuf;
use std::sync::Arc;
use support::*;
fn config() -> marathon_config::Configuration {
    let mut c = configuration();
    let a = android(&mut c);
    a.test_application_output = Some(PathBuf::from(
        fixture("vendor-android/apk/app-debug-androidTest.apk")
            .to_string_lossy()
            .into_owned(),
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
            let probes = |server: &marathon_adb::server::FakeAdbServer| {
                request_services(server).iter().filter(|s| *s == "host:version").count()
            };
            let probes_before = probes(&server);
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
                    d.serial_number().to_owned()
                )))
            );
            assert_eq!(
                commands(&server)
                    .iter()
                    .filter(|s| s.starts_with("am instrument"))
                    .count(),
                3
            );
            // D22: output gone silent says nothing about the server, and the Kotlin transcripts have no such request.
            assert_eq!(probes(&server), probes_before);
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
/// D25 (N38): a producer failure used to end the attempt as a success with whatever tests it had found so far; the run
/// would then silently leave the rest out. It now fails the attempt, with the cause, and the retries apply.
#[test]
fn producer_failure_fails_the_parsing_attempt_d25() {
    run(config(), |runtime, context| {
        Box::pin(async move {
            let response = "INSTRUMENTATION_STATUS: numtests=many\nINSTRUMENTATION_STATUS_CODE: 1\n";
            let mut server = server(script(vec![json!({"prefix":"am instrument","stdout":response})])).await;
            let d = device(&server, runtime.clone());
            d.setup(context.clone()).await?;
            let provider = Arc::new(AdamDeviceProvider::new(runtime.clone())?);
            let parser = AmInstrumentTestParser {
                runtime: runtime.clone(),
                provider,
            };
            let error = parser
                .parse_tests(&d, runtime.bundles[0].clone(), false, context)
                .await
                .unwrap_err();
            assert!(matches!(error, VendorError::TestParsing(_)), "{error:?}");
            assert_eq!(
                error.failure().message,
                Some(
                    format!(
                        "Unable to parse test list using {}: java.lang.NumberFormatException: For input string: \"many\"",
                        d.serial_number().to_owned()
                    )
                    .as_str()
                    .into()
                )
            );
            assert_eq!(d.listener_counts(), (0, 0));
            d.dispose().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
/// Over shell v1 a command's end and a dropped connection look alike, which is how the fake gives a dropped socket.
fn v1_script(outputs: Vec<Value>) -> Value {
    let mut s = base_script(30, vec!["cmd"]);
    let list = s["shell"].as_array_mut().unwrap();
    list.push(json!({"exact":"pm list packages","stdout":""}));
    list.push(json!({"prefix":"cmd package install","service":"exec","stdout":"Success\n"}));
    list.extend(outputs);
    s
}
/// The output for these tests of the two the run has, with no end.
fn listed(methods: &[&str]) -> String {
    methods
        .iter()
        .map(|method| {
            format!(
                "INSTRUMENTATION_STATUS: class=pkg.Clazz\nINSTRUMENTATION_STATUS: test={method}\nINSTRUMENTATION_STATUS: numtests=2\nINSTRUMENTATION_STATUS_CODE: 1\nINSTRUMENTATION_STATUS: class=pkg.Clazz\nINSTRUMENTATION_STATUS: test={method}\nINSTRUMENTATION_STATUS: numtests=2\nINSTRUMENTATION_STATUS_CODE: 0\n"
            )
        })
        .collect()
}
fn whole() -> String {
    format!("{}INSTRUMENTATION_CODE: -1\n", listed(&["one", "two"]))
}
/// What `extract` returns for this script, and how many `am instrument` it took.
fn extracted(script: Value) -> (VendorResult<Vec<String>>, usize) {
    let outcome = std::sync::Arc::new(std::sync::Mutex::new(None));
    let slot = outcome.clone();
    run(config(), move |runtime, context| {
        Box::pin(async move {
            let mut server = server(script).await;
            let runtime = change_runtime(&runtime, &[server.port()]);
            let provider = Arc::new(AdamDeviceProvider::new(runtime.clone())?);
            provider.initialize(context.clone()).await?;
            let reference = provider.borrow(context.clone()).await?;
            let parser = AmInstrumentTestParser {
                runtime,
                provider: provider.clone(),
            };
            let tests = parser
                .extract(reference, context)
                .await
                .map(|tests| tests.iter().map(|t| t.method.to_owned()).collect());
            let attempts = commands(&server)
                .iter()
                .filter(|s| s.starts_with("am instrument"))
                .count();
            *slot.lock().unwrap() = Some((tests, attempts));
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
    let outcome = outcome.lock().unwrap().take();
    outcome.unwrap()
}
/// D25 (N38): the false green. A dropped socket used to end the attempt as a success with the tests listed so far, and
/// the run went on with only those: with a filter that matched none of them, a green run of nothing. Every point of the
/// cut is covered, down to a list that is in fact whole, as nothing can tell that without the instrumentation's end.
#[test]
fn a_stream_cut_off_is_retried_and_the_list_is_whole_d25() {
    for cut in [
        String::new(),
        "INSTRUMENTATION_STATUS: numtests=2\nINSTRU".to_owned(),
        listed(&["one"]),
        format!("{}INSTRUMENTATION_STATUS: cla", listed(&["one"])),
        listed(&["one", "two"]),
    ] {
        let (tests, attempts) = extracted(v1_script(vec![
            json!({"prefix":"am instrument","stdout":cut,"times":1}),
            json!({"prefix":"am instrument","stdout":whole()}),
        ]));
        assert_eq!(tests.unwrap(), ["one", "two"], "cut: {cut:?}");
        assert_eq!(attempts, 2, "cut: {cut:?}");
    }
}
/// D25: every attempt cut off is a parse failure with the cause, never the tests found so far.
#[test]
fn a_stream_cut_off_on_every_attempt_fails_and_gives_no_list_d25() {
    let (tests, attempts) = extracted(v1_script(vec![
        json!({"prefix":"am instrument","stdout":listed(&["one"])}),
    ]));
    let error = tests.unwrap_err();
    assert!(matches!(error, VendorError::TestParsing(_)), "{error:?}");
    let message = error.failure().message.as_ref().unwrap().to_owned();
    assert!(
        message.ends_with(": java.io.EOFException: the instrumentation stream ended before a result"),
        "{message}"
    );
    assert_eq!(attempts, 3);
}
/// `am` killed before it printed anything, as on a real device, ends with nothing but an exit code. The parse used to
/// say that there are no tests.
#[test]
fn am_killed_before_any_output_is_retried_d25() {
    let (tests, attempts) = extracted(script(vec![
        json!({"prefix":"am instrument","exit_code":137,"times":1}),
        json!({"prefix":"am instrument","stdout":whole()}),
    ]));
    assert_eq!(tests.unwrap(), ["one", "two"]);
    assert_eq!(attempts, 2);
}
/// `am` killed on the device ends the stream with an exit code from the shell, and no result.
#[test]
fn a_killed_am_fails_the_attempt_with_its_exit_code_d25() {
    let (tests, attempts) = extracted(script(vec![
        json!({"prefix":"am instrument","stdout":listed(&["one"]),"exit_code":137}),
    ]));
    let error = tests.unwrap_err();
    let message = error.failure().message.as_ref().unwrap().to_owned();
    assert!(message.ends_with("before a result, with exit code 137"), "{message}");
    assert_eq!(attempts, 3);
}
/// A stream cut inside the first word of the protocol has failed, and it would otherwise read as an empty list.
#[test]
fn a_stream_cut_inside_its_first_word_is_retried_d25() {
    for cut in ["I", "INSTRUMENTATION", "\n", "a warning\nINSTRU"] {
        let (tests, attempts) = extracted(v1_script(vec![json!({"prefix":"am instrument","stdout":cut})]));
        assert!(tests.is_err(), "{cut:?}");
        assert_eq!(attempts, 3, "{cut:?}");
    }
}
/// The ends that are no cut keep their one attempt and their result, as captured on real devices: `am` refusing to
/// start, and output that never was the protocol. What a failing session code means for its list is D42's, below.
#[test]
fn ordinary_ends_are_not_retried_d25() {
    let refused = "INSTRUMENTATION_STATUS: Error=Unable to find instrumentation info for: ComponentInfo{com.nope/com.nope.Runner}\nINSTRUMENTATION_STATUS: id=ActivityManagerService\nINSTRUMENTATION_STATUS_CODE: -1\nonError: commandError=true message=INSTRUMENTATION_FAILED: com.nope/com.nope.Runner\n";
    for (name, built) in [
        (
            "refused",
            script(vec![json!({"prefix":"am instrument","stdout":refused,"exit_code":1})]),
        ),
        (
            "refused v1",
            v1_script(vec![json!({"prefix":"am instrument","stdout":refused})]),
        ),
        (
            "not the protocol",
            v1_script(vec![json!({"prefix":"am instrument","stdout":"sh: am: not found\n"})]),
        ),
    ] {
        let (tests, attempts) = extracted(built);
        assert!(tests.unwrap().is_empty(), "{name}");
        assert_eq!(attempts, 1, "{name}");
    }
}
fn ended(end: &str) -> String {
    format!("{}{end}", listed(&["one"]))
}
/// D42: a session whose code says it failed, and that stopped short of what it announced, used to return the tests so
/// far, and the run went on with only those. It now takes the retry path. Each message and code ends the same way.
#[test]
fn a_session_that_fails_short_of_its_count_is_retried_and_the_list_is_whole_d42() {
    for end in [
        "INSTRUMENTATION_RESULT: shortMsg=Other\nINSTRUMENTATION_CODE: 0\n",
        "INSTRUMENTATION_RESULT: shortMsg=java.lang.RuntimeException\nINSTRUMENTATION_RESULT: longMsg=boom\nINSTRUMENTATION_CODE: 0\n",
        "INSTRUMENTATION_CODE: 0\n",
        "INSTRUMENTATION_CODE: 1\n",
        "INSTRUMENTATION_CODE: odd\n",
        // No line break: the transformer can't read these, and says the run failed to complete.
        "INSTRUMENTATION_CODE: 0",
        "INSTRUMENTATION_CODE: -1",
    ] {
        let (tests, attempts) = extracted(script(vec![
            json!({"prefix":"am instrument","stdout":ended(end),"times":1}),
            json!({"prefix":"am instrument","stdout":whole()}),
        ]));
        assert_eq!(tests.unwrap(), ["one", "two"], "{end:?}");
        assert_eq!(attempts, 2, "{end:?}");
    }
}
/// D42: a failure that repeats ends the attempts, with what the session said and how far it got. A session that got
/// through to its code says nothing against the server, so this asks it nothing more.
#[test]
fn a_session_that_keeps_failing_ends_in_a_parsing_error_with_its_reason_d42() {
    let outcome = Arc::new(std::sync::Mutex::new(None));
    let slot = outcome.clone();
    run(config(), move |runtime, context| {
        Box::pin(async move {
            let output = ended("INSTRUMENTATION_RESULT: shortMsg=Other\nINSTRUMENTATION_CODE: 0\n");
            let mut server = server(script(vec![json!({"prefix":"am instrument","stdout":output})])).await;
            let runtime = change_runtime(&runtime, &[server.port()]);
            let provider = Arc::new(AdamDeviceProvider::new(runtime.clone())?);
            provider.initialize(context.clone()).await?;
            let reference = provider.borrow(context.clone()).await?;
            let serial = provider.android_device(&reference).unwrap().serial_number();
            let probes = |server: &marathon_adb::server::FakeAdbServer| {
                request_services(server).iter().filter(|s| *s == "host:version").count()
            };
            let before = probes(&server);
            let parser = AmInstrumentTestParser {
                runtime,
                provider: provider.clone(),
            };
            let error = parser.extract(reference, context).await.unwrap_err();
            assert!(matches!(error, VendorError::TestParsing(_)), "{error:?}");
            assert_eq!(
                error.failure().class,
                js("com.malinskiy.marathon.exceptions.TestParsingException")
            );
            let attempts = commands(&server)
                .iter()
                .filter(|s| s.starts_with("am instrument"))
                .count();
            *slot.lock().unwrap() = Some((
                error.failure().message.to_owned(),
                serial,
                attempts,
                probes(&server) - before,
            ));
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
    let (message, serial, attempts, probes) = outcome.lock().unwrap().take().unwrap();
    assert_eq!(
        message.unwrap().to_owned(),
        format!(
            "Unable to parse test list using {}: Other, tests listed: 1 of 2",
            serial.to_owned()
        )
    );
    assert_eq!(attempts, 3);
    assert_eq!(probes, 0);
}
/// D42: a failing code on a whole list is no reason to fail, and would fail every time. adam's `log_6` is such a session,
/// from a real device. The list stands on its first attempt, whatever the message, as in Kotlin.
#[test]
fn a_failing_code_on_a_whole_list_keeps_the_list_d42() {
    let recording = checked(std::fs::read_to_string(fixture("adam/instrumentation/log_6.input")));
    assert!(recording.trim_end().ends_with("INSTRUMENTATION_CODE: 0"));
    let (tests, attempts) = extracted(script(vec![json!({"prefix":"am instrument","stdout":recording})]));
    assert_eq!(tests.unwrap().len(), 9);
    assert_eq!(attempts, 1);
    for end in [
        "INSTRUMENTATION_CODE: 0\n",
        "INSTRUMENTATION_RESULT: shortMsg=Other\nINSTRUMENTATION_CODE: 0\n",
    ] {
        let output = format!("{}{end}", listed(&["one", "two"]));
        let (tests, attempts) = extracted(script(vec![json!({"prefix":"am instrument","stdout":output})]));
        assert_eq!(tests.unwrap(), ["one", "two"], "{end:?}");
        assert_eq!(attempts, 1, "{end:?}");
    }
}
/// D42: with no count to reach, nothing shows that the list is whole. Each of these ended with a failing code and no
/// `numtests`, which only the first status atom can state.
#[test]
fn a_failing_code_with_no_count_fails_the_attempt_d42() {
    let no_count = "INSTRUMENTATION_STATUS: class=pkg.Clazz\nINSTRUMENTATION_STATUS: test=one\nINSTRUMENTATION_STATUS_CODE: 1\nINSTRUMENTATION_STATUS: class=pkg.Clazz\nINSTRUMENTATION_STATUS: test=one\nINSTRUMENTATION_STATUS_CODE: 0\n";
    for (end, reason) in [
        (
            "INSTRUMENTATION_RESULT: shortMsg=Other\nINSTRUMENTATION_CODE: 0\n".to_owned(),
            "Other, tests listed: 0",
        ),
        (
            "INSTRUMENTATION_CODE: 0\n".to_owned(),
            "Unexpected INSTRUMENTATION_CODE: 0, tests listed: 0",
        ),
        (
            format!("{no_count}INSTRUMENTATION_CODE: 0\n"),
            "Unexpected INSTRUMENTATION_CODE: 0, tests listed: 1",
        ),
        (
            "INSTRUMENTATION_STATUS: numtests=0\nINSTRUMENTATION_STATUS_CODE: 1\nINSTRUMENTATION_CODE: 0\n".to_owned(),
            "Unexpected INSTRUMENTATION_CODE: 0, tests listed: 0 of 0",
        ),
        (
            "INSTRUMENTATION_STATUS: stream=Bad argument\nINSTRUMENTATION_STATUS: stack=trace\nINSTRUMENTATION_STATUS_CODE: -1\nINSTRUMENTATION_CODE: 0\n".to_owned(),
            "Unexpected INSTRUMENTATION_CODE: 0: Bad argument, tests listed: 0",
        ),
    ] {
        let (tests, attempts) = extracted(script(vec![json!({"prefix":"am instrument","stdout":end})]));
        let error = tests.unwrap_err();
        let message = error.failure().message.as_ref().unwrap().to_owned();
        assert!(message.ends_with(&format!(": {reason}")), "{message}");
        assert_eq!(attempts, 3, "{reason}");
    }
}
/// D42 leaves code -1 alone: it is success, even with the list short of its count, and the end of a run needs no message.
#[test]
fn code_minus_one_stays_success_on_a_short_list_d42() {
    let output = format!("{}INSTRUMENTATION_CODE: -1\n", listed(&["one"]));
    let (tests, attempts) = extracted(script(vec![json!({"prefix":"am instrument","stdout":output})]));
    assert_eq!(tests.unwrap(), ["one"]);
    assert_eq!(attempts, 1);
}
/// D42 leaves `Process crashed` alone: it means the test annotation producer, and the next attempt drops that listener,
/// as in Kotlin, with or without a whole list.
#[test]
fn process_crashed_stays_the_fallback_path_d42() {
    for tests in [&["one"][..], &["one", "two"][..]] {
        let crash = format!(
            "{}INSTRUMENTATION_RESULT: shortMsg=Process crashed.\nINSTRUMENTATION_CODE: 0\n",
            listed(tests)
        );
        let (tests, attempts) = extracted(script(vec![
            json!({"regex":"^am instrument.*-e listener ","stdout":crash}),
            json!({"prefix":"am instrument","stdout":whole()}),
        ]));
        assert_eq!(tests.unwrap(), ["one", "two"]);
        assert_eq!(attempts, 2);
    }
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

/// x37: with no `numtests`, only the code line tells this list from a whole one, and here it was cut.
#[test]
fn a_parse_cut_inside_its_code_line_is_retried_d25() {
    let no_count = "INSTRUMENTATION_STATUS: class=pkg.Clazz\nINSTRUMENTATION_STATUS: test=one\nINSTRUMENTATION_STATUS_CODE: 1\nINSTRUMENTATION_STATUS: class=pkg.Clazz\nINSTRUMENTATION_STATUS: test=one\nINSTRUMENTATION_STATUS_CODE: 0\n";
    let cut = format!("{no_count}INSTRUMENTATION_CODE");
    let (tests, attempts) = extracted(script(vec![
        json!({"prefix":"am instrument","stdout":cut,"times":1}),
        json!({"prefix":"am instrument","stdout":whole()}),
    ]));
    assert_eq!(
        (tests.unwrap(), attempts),
        (vec!["one".to_owned(), "two".to_owned()], 2)
    );
}
