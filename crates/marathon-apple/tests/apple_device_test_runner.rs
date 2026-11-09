//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/AppleDeviceTestRunnerTest.kt
use marathon_error::{FailureKind, FatalKind, InjectedKind};
#[path = "support/parser.rs"]
mod parser_support;
#[path = "support/runner_device.rs"]
mod runner_device;
mod support;
#[path = "support/test_root_device.rs"]
mod test_root_device;
use marathon_logging::LogSettings;
use marathon_apple::{
    apple_device_test_runner::{AppleDeviceTestRunner, RunError},
    cmd::{SessionScope, cancelled, failure},
    listener::{AppleTestRunListener, CompositeTestRunListener},
    logparser::parser::DeviceFailureReason,
    test::{TestEvent, TestRequest},
};
use marathon_core::{error::VendorError, runtime::VendorFuture, vendor::Device};
use runner_device::{Plan, RunnerDevice};
use std::sync::{Arc, Mutex};
use test_root_device::{Fake, TestResult, v};
fn failed() -> TestEvent {
    TestEvent::TestRunFailed {
        message: "Test runner exited".into(),
        reason: DeviceFailureReason::Unknown,
    }
}
fn setup(plan: Plan) -> (Arc<RunnerDevice>, AppleDeviceTestRunner) {
    let inner = Fake::new(test_root_device::input(), std::path::Path::new("/tmp"));
    let device = Arc::new(RunnerDevice::new(inner, logging()));
    *device.plan.lock().unwrap() = plan;
    let mut runner = AppleDeviceTestRunner::new(device.clone(), Arc::new(support::Runtime), device.logging.clone());
    runner.session_close_timeout = support::duration(200);
    (device, runner)
}
fn logging() -> marathon_apple::cmd::remote::ssh::sshj::config::SshLogging {
    let mut registry = LogSettings::default();
    registry.debug = true;
    marathon_apple::cmd::remote::ssh::sshj::config::SshLogging::new(
        Arc::new(Mutex::new(registry)),
        Arc::new(|_, _, _, _| {}),
    )
}
struct Listener {
    calls: Arc<Mutex<Vec<String>>>,
    name: &'static str,
    after: bool,
    started: tokio::sync::Notify,
}
impl Listener {
    fn new(calls: Arc<Mutex<Vec<String>>>, name: &'static str) -> Self {
        Self {
            calls,
            name,
            after: false,
            started: tokio::sync::Notify::new(),
        }
    }
}
impl AppleTestRunListener for Listener {
    fn test_run_started(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.started.notify_one();
            Ok(())
        })
    }
    fn test_run_failed<'a>(&'a self, _: &'a str, reason: DeviceFailureReason) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{}:testRunFailed:{}", self.name, reason.name()));
            Ok(())
        })
    }
    fn test_run_ended(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(format!("{}:ended", self.name));
            Ok(())
        })
    }
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(format!("{}:afterTestRun", self.name));
            if self.after {
                return Err(VendorError::Other(failure(
                    FailureKind::IllegalState,
                    "no result bundle",
                )));
            }
            Ok(())
        })
    }
}
async fn execute(runner: &AppleDeviceTestRunner, l: &dyn AppleTestRunListener) -> Result<(), RunError> {
    runner
        .execute_request(
            TestRequest::new("/tmp", "/tmp/test.xctestrun", false),
            &[],
            l,
            &support::context(),
        )
        .await
}
#[tokio::test]
async fn test_run_failure_reaches_every_listener_and_fails_the_run_with_its_reason() -> TestResult {
    let (device, runner) = setup(Plan {
        events: vec![vec![TestEvent::TestRunStartedEvent, failed(), TestEvent::TestRunEnded]],
        ..Default::default()
    });
    let calls = Arc::new(Mutex::new(vec![]));
    let listener = CompositeTestRunListener::new(vec![
        Arc::new(Listener::new(calls.clone(), "first")),
        Arc::new(Listener::new(calls.clone(), "second")),
    ]);
    let result = execute(&runner, &listener).await;
    v(device.dispose().await)?;
    assert_eq!(result.unwrap_err().reason, Some(DeviceFailureReason::Unknown));
    assert_eq!(
        *calls.lock().unwrap(),
        [
            "first:testRunFailed:Unknown",
            "second:testRunFailed:Unknown",
            "first:afterTestRun",
            "second:afterTestRun"
        ]
    );
    Ok(())
}
#[tokio::test]
async fn failure_after_the_test_run_keeps_the_run_failure_and_stops_reading_events() -> TestResult {
    let (device, runner) = setup(Plan {
        events: vec![vec![failed()], vec![TestEvent::TestRunEnded]],
        ..Default::default()
    });
    let mut l = Listener::new(device.calls.clone(), "one");
    l.after = true;
    let error = execute(&runner, &l).await.unwrap_err();
    v(device.dispose().await)?;
    assert_eq!(error.reason, Some(DeviceFailureReason::Unknown));
    assert_eq!(error.suppressed.len(), 1);
    assert!(!device.calls.lock().unwrap().iter().any(|c| c.ends_with(":ended")));
    assert!(
        error
            .into_vendor()
            .failure()
            .stack_trace()
            .to_owned()
            .contains("Suppressed: invalid state: no result bundle")
    );
    Ok(())
}
async fn closing_case(
    failure: bool,
    timeout: bool,
    close_error: Option<FailureKind>,
    output_error: Option<FailureKind>,
) -> TestResult {
    let error = |class: FailureKind| VendorError::Other(failure_fn(class));
    let (device, runner) = setup(Plan {
        events: vec![
            vec![TestEvent::TestRunStartedEvent],
            vec![if failure { failed() } else { TestEvent::TestRunEnded }],
        ],
        closing_failure: close_error.clone().map(error),
        output_failure: output_error.clone().map(error),
        wait_close: close_error.is_none(),
        ..Default::default()
    });
    let listener = Listener::new(device.calls.clone(), "one");
    let run = execute(&runner, &listener);
    tokio::pin!(run);
    // Poll the run while its producer reaches close, then explicitly release only in the in-time case.
    if close_error.is_none() {
        tokio::select! {result=&mut run=>panic!("run returned before close began: {result:?}"),_ = device.closing.notified()=>()}
        if !timeout {
            device.release.notify_one();
        }
    }
    let result = run.await;
    device.calls.lock().unwrap().push("run ended".into());
    let calls = device.calls.lock().unwrap().clone();
    v(device.dispose().await)?;
    if timeout || close_error.is_some() {
        let result = result.unwrap_err();
        assert!(matches!(result.error, VendorError::DeviceLost(_)));
        let flattened = result.into_vendor();
        let not_closed = flattened.failure().cause.as_ref().unwrap();
        let expected = close_error.unwrap_or(FailureKind::Timeout);
        assert_eq!(not_closed.cause.as_ref().unwrap().kind, expected);
        if let Some(class) = output_error {
            assert!(not_closed.stack_trace().to_owned().contains(class.description()));
        }
        assert_eq!(calls.last().unwrap(), "run ended");
        assert!(!calls.contains(&"closed".to_owned()));
    } else {
        if failure {
            assert_eq!(result.unwrap_err().reason, Some(DeviceFailureReason::Unknown));
        } else {
            assert!(result.is_ok());
        }
        let closed = calls.iter().position(|c| c == "closed").unwrap();
        assert!(closed < calls.len() - 1);
    }
    Ok(())
}
fn failure_fn(class: FailureKind) -> marathon_core::error::Failure {
    failure(class, "channel close not confirmed")
}
#[tokio::test]
async fn failed_run_ends_only_after_its_xcodebuild_session_has_closed() -> TestResult {
    closing_case(true, false, None, None).await
}
#[tokio::test]
async fn finished_run_ends_only_after_its_xcodebuild_session_has_closed() -> TestResult {
    closing_case(false, false, None, None).await
}
#[tokio::test]
async fn device_whose_xcodebuild_session_has_not_closed_in_time_is_lost() -> TestResult {
    closing_case(true, true, None, None).await
}
#[tokio::test]
async fn device_whose_xcodebuild_session_failed_to_close_is_lost() -> TestResult {
    closing_case(true, false, Some(FailureKind::Io), None).await
}
#[tokio::test]
async fn device_whose_xcodebuild_session_failed_to_close_with_an_error_is_lost() -> TestResult {
    closing_case(true, false, Some(FailureKind::Fatal(FatalKind::Assertion)), None).await
}
#[tokio::test]
async fn failed_output_reaches_the_run_before_its_xcodebuild_session_is_closed() -> TestResult {
    closing_case(false, true, None, Some(FailureKind::Timeout)).await
}
#[tokio::test]
async fn output_failing_with_an_error_reaches_the_run_before_its_xcodebuild_session_is_closed() -> TestResult {
    closing_case(false, true, None, Some(FailureKind::Fatal(FatalKind::Assertion))).await
}
#[tokio::test]
async fn cancelled_run_waits_for_its_xcodebuild_session_and_stays_cancelled_when_it_does_not_close_in_time()
-> TestResult {
    let (device, runner) = setup(Plan {
        events: vec![vec![TestEvent::TestRunStartedEvent]],
        wait_close: true,
        hang_output: true,
        ..Default::default()
    });
    let l = Listener::new(device.calls.clone(), "one");
    let signal = Arc::new(SessionScope::default());
    let mut ctx = support::context();
    ctx.cancellation = signal.clone();
    let run = runner.execute_request(TestRequest::new("/tmp", "test", false), &[], &l, &ctx);
    tokio::pin!(run);
    tokio::select! {result=&mut run=>panic!("early {result:?}"),_=l.started.notified()=>()};
    signal.cancel(cancelled("cancelled"));
    let start = tokio::time::Instant::now();
    let error = run.await.unwrap_err();
    assert!(start.elapsed() >= std::time::Duration::from_millis(200));
    assert!(matches!(error.error, VendorError::Cancelled(_)));
    v(device.dispose().await)?;
    Ok(())
}
#[test]
fn unchanged_kotlin_tests_passed() {
    let rows = include_str!("../../../fixtures/oracle/test-runner/probes.jsonl");
    assert_eq!(
        rows.lines()
            .filter(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["passed"] == true)
            .count(),
        21
    );
}

#[tokio::test]
async fn execute_builds_the_request_and_logs_remote_path_and_tests() -> TestResult {
    use marathon_logging::Level;
    use marathon_apple::{apple_device::AppleDevice, cmd::remote::ssh::sshj::config::SshLogging};
    use marathon_core::model::{Test, TestBatch};
    let tmp = tempfile::tempdir()?;
    let input = parser_support::input(false, false);
    test_root_device::prepare(tmp.path(), &input)?;
    let inner = Fake::new(input.clone(), tmp.path());
    let bundle = parser_support::bundle(&inner, &input).await?;
    let logs = Arc::new(Mutex::new(vec![]));
    let captured = logs.clone();
    let logging = SshLogging::new(
        logging().registry,
        Arc::new(move |logger, level, message, _| {
            captured
                .lock()
                .unwrap()
                .push((logger.name().to_owned(), level, message));
        }),
    );
    let device = Arc::new(RunnerDevice::new(inner, logging.clone()));
    let runner = AppleDeviceTestRunner::new(device.clone(), Arc::new(support::Runtime), logging);
    let mut configuration = marathon_test_support::factory::ConfigurationFactory::new();
    configuration.configuration.is_code_coverage_enabled = true;
    let batch = TestBatch {
        id: "batch-id".into(),
        tests: vec![Test {
            pkg: "UITests".into(),
            clazz: "SampleTests".into(),
            method: "testSample".into(),
            meta_properties: vec![],
        }],
    };
    let listener = Listener::new(device.calls.clone(), "one");
    v(runner
        .execute(
            &configuration.configuration,
            &bundle,
            &batch,
            &listener,
            &support::context(),
        )
        .await)?;
    v(device.dispose().await)?;
    let requests = device.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.workdir, "/tmp/marathon/udid");
    assert_eq!(request.remote_xctestrun, "/tmp/marathon/udid/marathon.xctestrun");
    assert_eq!(
        request.xcresult.as_deref(),
        Some("/tmp/marathon/udid/udid.batch-id.xcresult")
    );
    assert!(request.coverage);
    assert_eq!(request.test_target_name.as_deref(), Some("UITests"));
    assert_eq!(request.test_target_name.as_deref(), Some(bundle.test_bundle_id()));
    assert_eq!(request.tests.as_ref(), Some(&batch.tests));
    assert_eq!(
        device.remote_file_manager().remote_xctestrun_file(),
        request.remote_xctestrun
    );
    let logs = logs.lock().unwrap();
    assert_eq!(
        &logs[..2],
        &[
            (
                "marathon_apple::apple_device_test_runner".into(),
                Level::DEBUG,
                "Remote xctestrun = /tmp/marathon/udid/marathon.xctestrun".into()
            ),
            (
                "marathon_apple::apple_device_test_runner".into(),
                Level::DEBUG,
                "Tests = [Test(pkg=UITests, clazz=SampleTests, method=testSample, metaProperties=[])]".into()
            ),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn after_run_error_replaces_failure_but_exception_named_error_is_suppressed() -> TestResult {
    struct After(FailureKind);
    impl AppleTestRunListener for After {
        fn after_test_run(&self) -> VendorFuture<'_, ()> {
            Box::pin(async move { Err(VendorError::Other(failure(self.0.clone(), "after failed"))) })
        }
    }
    for (class, is_error) in [
        (FailureKind::Fatal(FatalKind::Assertion), true),
        (FailureKind::Fatal(FatalKind::ThreadDeath), true),
        (FailureKind::Injected(InjectedKind::ProbeExceptionNamedError), false),
    ] {
        let (device, runner) = setup(Plan {
            events: vec![vec![failed()]],
            ..Default::default()
        });
        let result = execute(&runner, &After(class.clone())).await.unwrap_err();
        v(device.dispose().await)?;
        if is_error {
            assert_eq!(result.error.failure().kind, class);
            assert!(result.suppressed.is_empty());
        } else {
            assert_eq!(result.reason, Some(DeviceFailureReason::Unknown));
            assert_eq!(result.suppressed.len(), 1);
            assert_eq!(result.suppressed[0].kind.description(), class.description().to_owned());
        }
    }
    Ok(())
}
