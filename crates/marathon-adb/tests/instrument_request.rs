//! Port of adam 0.5.8 adam/src/test/kotlin/com/malinskiy/adam/request/testrunner/InstrumentOptionsTest.kt
//! and TestRunnerRequestTest.kt. Copyright (C) 2020 Anton Malinskiy; Apache-2.0.

use marathon_adb::{
    client::{
        AndroidDebugBridgeClient, Feature, SocketFactory,
        shell::compat::{FallibleAsyncCompatShellCommandRequest, FallibleCompatShellStream},
    },
    instrument_request::*,
    server::{FakeAdbServer, Script, ServerOptions},
    testrunner::*,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
fn checked<T, E: std::fmt::Debug>(r: Result<T, E>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("fixture: {e:?}"),
    }
}
fn text(s: String) -> String {
    s.to_owned()
}
fn oracle(name: &str) -> Value {
    include_str!("../../../fixtures/oracle/android-runner-probes.jsonl")
        .lines()
        .map(|l| checked(serde_json::from_str::<Value>(l)))
        .find(|r| r["case"] == name)
        .unwrap()["value"]
        .clone()
}
mod instrument_options_test {
    use super::*;
    fn all() -> InstrumentOptions {
        InstrumentOptions {
            pkg: vec!["com.example".into()],
            clazz: vec!["com.example.TestClass".into()],
            functional: Some(true),
            unit: Some(true),
            filter_size: Some(InstrumentationSizeOption::Medium),
            performance: Some(true),
            debug: Some(true),
            log: Some(true),
            emma: Some(true),
            coverage_file: Some("/sdcard/codecov".into()),
            overrides: [("test".into(), "test".into())].into_iter().collect(),
        }
    }
    #[test]
    fn test_default() {
        assert_eq!(
            text(InstrumentOptions::default().to_java_string()),
            oracle("options:default")
        );
    }
    #[test]
    fn test_all() {
        assert_eq!(text(all().to_java_string()), oracle("options:all"));
    }
    #[test]
    fn test_all_with_properties() {
        let o = all();
        assert_eq!(o.pkg, vec![String::from("com.example")]);
        assert_eq!(o.clazz, vec![String::from("com.example.TestClass")]);
        assert_eq!(o.overrides.entries(), &[("test".into(), "test".into())]);
        assert_eq!(o.coverage_file, Some("/sdcard/codecov".into()));
        assert_eq!(o.filter_size, Some(InstrumentationSizeOption::Medium));
        for value in [o.debug, o.emma, o.functional, o.log, o.performance, o.unit] {
            assert_eq!(value, Some(true));
        }
    }
    #[test]
    fn test_multiple_overrides() {
        let o = InstrumentOptions {
            overrides: [("param1".into(), "value1".into()), ("param2".into(), "value2".into())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        assert_eq!(text(o.to_java_string()), " -e param1 value1 -e param2 value2");
    }
    #[test]
    fn lists_false_and_duplicate_builtins() {
        let o = InstrumentOptions {
            pkg: vec!["p".into(), "q".into()],
            clazz: vec!["C#m[0]".into(), "C#space\\ name".into()],
            functional: Some(false),
            unit: Some(false),
            filter_size: Some(InstrumentationSizeOption::Large),
            performance: Some(false),
            debug: Some(false),
            log: Some(false),
            emma: Some(false),
            overrides: [("class".into(), "override".into()), ("log".into(), "false".into())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        assert_eq!(text(o.to_java_string()), oracle("options:lists-false-collision"));
    }
}
mod test_runner_request_test {
    use super::*;
    fn request(features: Vec<Feature>) -> TestRunnerRequest {
        TestRunnerRequest::new("com.example.test".into(), InstrumentOptions::default(), features)
    }
    #[test]
    fn test_serialize() {
        let mut r = request(vec![]);
        r.no_window_animations = true;
        r.no_hidden_api_checks = true;
        r.user_id = Some(1000);
        r.abi = Some("x86".into());
        r.profiling_output_path = Some("/sdcard/profiling".into());
        r.output_log_path = Some("/sdcard/log".into());
        r.runner_class = "com.example.test.AndroidTestRunner".into();
        assert_eq!(
            text(r.command()),
            "am instrument -w -r --no-hidden-api-checks --no-window-animation --user 1000 --abi x86 -p /sdcard/profiling -f /sdcard/log com.example.test/com.example.test.AndroidTestRunner"
        );
        r.no_isolated_storage = true;
        assert_eq!(text(r.command()), oracle("request:all"));
    }
    #[test]
    fn test_default_serialize() {
        let r = request(vec![]);
        assert_eq!(text(r.command()), oracle("request:default"));
        assert_eq!(r.socket_idle_timeout, Some(i64::MAX));
    }
    async fn collect(features: Vec<Feature>, stdout: &str, stderr: &str, exit: i32) -> (Vec<TestEvent>, String) {
        let cmd = request(features.clone()).cmd();
        let script = json!({"server":{"strict":true},"devices":[{"serial":"serial"}],"shell":[{"exact":cmd,"stdout":stdout,"stderr":stderr,"exit_code":exit}]});
        let script = checked(checked(Script::from_yaml(&script.to_string())).compile(Path::new(".")));
        let mut server = checked(FakeAdbServer::start(script, ServerOptions::default()).await);
        let client = AndroidDebugBridgeClient::new(
            server.addr().ip(),
            server.port(),
            Arc::new(SocketFactory::new(1000, 30000)),
            None,
        );
        let mut stream = FallibleCompatShellStream::start(&client, request(features), Some("serial"));
        let mut events = vec![];
        while let Some(chunk) = stream.next().await {
            events.extend(checked(chunk));
        }
        let service = server
            .transcript()
            .iter()
            .find(|r| r["type"] == "request" && r["service"].as_str().is_some_and(|s| s.starts_with("shell")))
            .unwrap()["service"]
            .as_str()
            .unwrap()
            .to_owned();
        checked(server.shutdown().await);
        (events, service)
    }
    fn expected() -> Vec<TestEvent> {
        let id = TestIdentifier {
            class_name: "com.example.MainActivityAllureTest".into(),
            test_name: "testText".into(),
        };
        vec![
            TestEvent::TestRunStartedEvent { test_count: 1 },
            TestEvent::TestStarted { id: id.clone() },
            TestEvent::TestEnded {
                id,
                metrics: MetricMap::new(),
            },
            TestEvent::TestRunEnded {
                elapsed_time_millis: 0,
                metrics: MetricMap::new(),
            },
        ]
    }
    const INPUT: &str = include_str!("../../../fixtures/adam/testrunner-request-test.input");
    #[tokio::test]
    async fn test_v1() {
        let (events, service) = collect(vec![], INPUT, "", 0).await;
        assert_eq!(events, expected());
        assert_eq!(service, format!("shell:{}", request(vec![]).cmd()));
    }
    #[tokio::test]
    async fn test_v2() {
        let (events, service) = collect(vec![Feature::ShellV2], INPUT, "", 0).await;
        assert_eq!(events, expected());
        assert_eq!(service, format!("shell,v2,raw:{}", request(vec![]).cmd()));
    }
    #[tokio::test]
    async fn test_v2_ignores_stderr() {
        assert_eq!(
            collect(vec![Feature::ShellV2], INPUT, "INSTRUMENTATION_FAILED: stderr", 17)
                .await
                .0,
            expected()
        );
    }
    #[tokio::test]
    async fn test_returns_content_on_failure() {
        assert_eq!(
            collect(vec![], "something-something", "", 0).await.0,
            vec![TestEvent::TestRunFailed {
                error: "No test results\nsomething-something".into()
            }]
        );
    }
    async fn ended(features: Vec<Feature>, stdout: &str, exit: i32) -> (Vec<TestEvent>, bool, Option<i32>) {
        let cmd = request(features.clone()).cmd();
        let script = json!({"server":{"strict":true},"devices":[{"serial":"serial"}],"shell":[{"exact":cmd,"stdout":stdout,"exit_code":exit}]});
        let script = checked(checked(Script::from_yaml(&script.to_string())).compile(Path::new(".")));
        let mut server = checked(FakeAdbServer::start(script, ServerOptions::default()).await);
        let client = AndroidDebugBridgeClient::new(
            server.addr().ip(),
            server.port(),
            Arc::new(SocketFactory::new(1000, 30000)),
            None,
        );
        let mut stream = FallibleCompatShellStream::start(&client, request(features), Some("serial"));
        let mut events = vec![];
        while let Some(chunk) = stream.next().await {
            events.extend(checked(chunk));
        }
        let request = stream.request().unwrap();
        let outcome = (events, request.ended_without_result(), request.exit_code());
        checked(server.shutdown().await);
        outcome
    }
    /// D25: over shell v1 nothing says how a command ended, so a dropped connection looks like this. No final event
    /// claims a result: `transform` would call this first one a success, every test having been reported.
    #[tokio::test]
    async fn a_stream_that_stops_before_its_code_has_no_final_events_d25() {
        let cut = INPUT.split("INSTRUMENTATION_RESULT").next().unwrap();
        let (events, ended, exit) = ended(vec![], cut, 0).await;
        assert!(ended);
        assert_eq!(exit, None);
        assert_eq!(events, expected()[..3]);
    }
    /// `am` killed on the device: the shell reports it, and there is still no result.
    #[tokio::test]
    async fn a_killed_am_reports_its_exit_code_d25() {
        let cut = INPUT.split("INSTRUMENTATION_RESULT").next().unwrap();
        let (_, ended, exit) = ended(vec![Feature::ShellV2], cut, 137).await;
        assert!(ended);
        assert_eq!(exit, Some(137));
    }
    /// `am` killed before it printed anything is no ordinary end either: nothing but its exit code came.
    #[tokio::test]
    async fn no_output_at_all_is_no_result_d25() {
        let (events, ended_early, exit) = ended(vec![Feature::ShellV2], "", 137).await;
        assert!(events.is_empty());
        assert!(ended_early);
        assert_eq!(exit, Some(137));
    }
    /// A non-zero session code is an ordinary end, and stray output keeps its answer: the Kotlin test above.
    #[tokio::test]
    async fn a_crash_keeps_its_final_events_d25() {
        let crash = "INSTRUMENTATION_RESULT: shortMsg=Process crashed.\nINSTRUMENTATION_CODE: 0\n";
        let (events, ended_early, _) = ended(vec![Feature::ShellV2], crash, 0).await;
        assert!(!ended_early);
        assert!(events.contains(&TestEvent::TestRunFailed {
            error: "Process crashed.".into()
        }));
    }
    #[tokio::test]
    async fn feature_matrix_uses_only_shell_v2() {
        for mask in 0..16 {
            let features = [Feature::ShellV2, Feature::Cmd, Feature::AbbExec, Feature::SendrecvV2]
                .into_iter()
                .enumerate()
                .filter_map(|(i, f)| (mask & (1 << i) != 0).then_some(f))
                .collect();
            let (events, service) = collect(features, INPUT, "", 0).await;
            assert_eq!(events, expected());
            assert!(service.starts_with(if mask & 1 != 0 { "shell,v2,raw:" } else { "shell:" }));
        }
    }
}
#[test]
fn fallible_conversion_preserves_jvm_error_classes_and_warnings() {
    use marathon_adb::client::shell::v2::ShellCommandResultChunk;
    for (name, input) in [
        (
            "numtests",
            "INSTRUMENTATION_STATUS: numtests=many\nINSTRUMENTATION_STATUS_CODE: 1\n",
        ),
        (
            "malformed",
            "garbage line\nINSTRUMENTATION_STATUS: numtests=1\nINSTRUMENTATION_STATUS_CODE: 1\n",
        ),
    ] {
        let mut request = TestRunnerRequest::new("test".into(), InstrumentOptions::default(), vec![]);
        let error = request
            .convert_chunk(ShellCommandResultChunk {
                stdout: Some(input.as_bytes().to_vec()),
                ..Default::default()
            })
            .unwrap_err();
        let expected = oracle(&format!("parser-error:{name}"));
        assert_eq!(
            error.failure_kind(),
            marathon_test_support::recorded_errors::kind(expected[0].as_str().unwrap())
        );
        assert_eq!(json!(error.message()), expected[1]);
        assert_eq!(
            error.to_failure().cause.map(|c| c.kind),
            expected[2].as_str().map(marathon_test_support::recorded_errors::kind)
        );
    }
    let mut request = TestRunnerRequest::new("test".into(), InstrumentOptions::default(), vec![]);
    assert!(request.convert_chunk(ShellCommandResultChunk{stdout:Some(b"INSTRUMENTATION_STATUS_CODE: 666\ngarbage line\nINSTRUMENTATION_STATUS: numtests=1\nINSTRUMENTATION_STATUS_CODE: 1\n".to_vec()),..Default::default()}).is_err());
    assert_eq!(request.take_warnings().len(), 1);
    assert!(request.take_warnings().is_empty());
}
