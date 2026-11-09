//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/StartupPhasesTest.kt and SkipCopyingDerivedDataTest.kt (symbol parsing rows).
use marathon_error::{FailureKind, SshKind};
#[path = "support/parser.rs"]
mod parser_support;
mod support;
#[path = "support/test_root_device.rs"]
mod test_root_device;
use marathon_apple::{
    cmd::{CommandResult, failure},
    nm_test_parser::parse_symbols,
};
use marathon_core::{
    error::VendorError,
    vendor::{Device, RemoteTestParser},
};
use parser_support::{configuration, input, nm};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use test_root_device::{Fake, TestResult, prepare, v};
use tokio::sync::Notify;
fn symbols(swift: bool) -> CommandResult {
    CommandResult {
        stdout: vec![
            if swift {
                "UITests.LoginTests.testLogin()"
            } else {
                "LoginTests testLogin"
            }
            .into(),
        ],
        stderr: vec![],
        exit_code: Some(0),
    }
}
async fn skip_case(reuse: bool, skip: bool, missing: Option<bool>) -> TestResult {
    let tmp = tempfile::tempdir()?;
    let mut c = input(reuse, skip);
    let remote = if reuse {
        "/tmp/marathon/shared/xctestRunner.app/PlugIns/UITests.xctest/UITests"
    } else {
        "/tmp/marathon/shared/marathon.xctest/UITests"
    };
    if let Some(unknown) = missing {
        c["missing"] = remote.into();
        c["unknown"] = unknown.into();
    }
    prepare(tmp.path(), &c)?;
    let device = Arc::new(Fake::new(c.clone(), tmp.path()));
    let old = device.recorder.worker.before.lock().unwrap().clone();
    *device.recorder.worker.before.lock().unwrap() = Arc::new(move |command| {
        let old = old.clone();
        Box::pin(async move {
            if command[0] == "sh" && command[2].contains("nm ") {
                return Ok(Some(symbols(true)));
            }
            old(command).await
        })
    });
    let (parser, id) = nm(&device, configuration(&c, tmp.path())?);
    let result = parser.extract(id, support::context()).await;
    v(device.dispose().await)?;
    let requests = device.recorder.requests.lock().unwrap();
    let commands = requests
        .iter()
        .map(|r| r["argv"].as_array().unwrap())
        .collect::<Vec<_>>();
    let checks = commands
        .iter()
        .filter(|c| c[0] == "test")
        .map(|c| c[2].as_str().unwrap())
        .collect::<Vec<_>>();
    let scans = commands
        .iter()
        .filter(|c| c[0] == "sh" && c[2].as_str().unwrap().contains("nm "))
        .collect::<Vec<_>>();
    if let Some(unknown) = missing {
        let error = result.unwrap_err();
        assert!(matches!(error, VendorError::TestParsing(_)));
        let msg = error.failure().message.as_ref().unwrap().to_owned();
        assert!(msg.contains(if unknown { "unable to check" } else { "is missing" }));
        assert!(msg.contains(remote));
        assert_eq!(checks, vec![remote; 3]);
        assert!(scans.is_empty());
    } else {
        let tests = v(result)?;
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].method, String::from("testLogin"));
        assert_eq!(scans.len(), 2);
        for scan in scans {
            assert!(scan[2].as_str().unwrap().contains(if skip {
                remote
            } else {
                "/tmp/marathon/shared/marathon.xctest/UITests"
            }));
        }
        if skip {
            assert!(device.pushes.lock().unwrap().is_empty());
            assert_eq!(
                checks,
                if reuse {
                    vec!["/tmp/marathon/shared/xctestRunner.app/UITests-Runner", remote]
                } else {
                    vec![remote]
                }
            );
        } else {
            assert_eq!(device.pushes.lock().unwrap().len(), 1);
            assert!(checks.is_empty());
        }
    }
    Ok(())
}
#[tokio::test]
async fn parsing_does_not_push_the_distributed_xctest() -> TestResult {
    skip_case(false, true, None).await
}
#[tokio::test]
async fn parsing_reads_the_xctest_inside_the_distributed_test_runner_application() -> TestResult {
    skip_case(true, true, None).await
}
#[tokio::test]
async fn parsing_fails_with_the_missing_remote_path() -> TestResult {
    skip_case(false, true, Some(false)).await
}
#[tokio::test]
async fn parsing_fails_if_the_remote_path_can_not_be_checked() -> TestResult {
    skip_case(false, true, Some(true)).await
}
#[tokio::test]
async fn parsing_pushes_the_xctest_without_the_flag() -> TestResult {
    skip_case(true, false, None).await
}
async fn scan_case(mode: u8) -> TestResult {
    let tmp = tempfile::tempdir()?;
    let c = input(false, false);
    prepare(tmp.path(), &c)?;
    let device = Arc::new(Fake::new(c.clone(), tmp.path()));
    let started = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(Notify::new());
    let old = device.recorder.worker.before.lock().unwrap().clone();
    let count = started.clone();
    let sessions = active.clone();
    let barrier = notify.clone();
    *device.recorder.worker.before.lock().unwrap() = Arc::new(move |command| {
        let old = old.clone();
        let count = count.clone();
        let sessions = sessions.clone();
        let barrier = barrier.clone();
        Box::pin(async move {
            if command[0] != "sh" || !command[2].contains("nm ") {
                return old(command).await;
            }
            let swift = command[2].contains("swift-demangle");
            let index = count.fetch_add(1, Ordering::SeqCst);
            if mode == 0 {
                let wait = barrier.notified();
                if index == 1 {
                    barrier.notify_waiters();
                } else {
                    wait.await;
                }
            }
            if mode == 1 {
                if sessions.fetch_add(1, Ordering::SeqCst) > 0 {
                    sessions.fetch_sub(1, Ordering::SeqCst);
                    barrier.notify_one();
                    return Err(VendorError::Other(failure(
                        FailureKind::Ssh(SshKind::OpenChannel),
                        "no more sessions",
                    )));
                }
                if index == 0 {
                    barrier.notified().await;
                }
                sessions.fetch_sub(1, Ordering::SeqCst);
            }
            if mode == 2 && !swift {
                return Err(VendorError::Other(failure(FailureKind::IllegalState, "Not connected")));
            }
            let mut result = symbols(swift);
            if mode == 3 && swift {
                result.exit_code = None;
            }
            Ok(Some(result))
        })
    });
    let (parser, id) = nm(&device, configuration(&c, tmp.path())?);
    let result = support::bounded(5, parser.extract(id, support::context())).await;
    v(device.dispose().await)?;
    match mode {
        0 | 1 => {
            let tests = v(result)?;
            assert_eq!(tests.len(), 2);
            assert_eq!(started.load(Ordering::SeqCst), if mode == 0 { 2 } else { 3 });
        }
        2 => {
            assert_eq!(started.load(Ordering::SeqCst), 6);
            assert_eq!(
                result.unwrap_err().failure().kind.description(),
                String::from("invalid state")
            );
        }
        3 => {
            assert_eq!(started.load(Ordering::SeqCst), 9);
            assert!(
                result
                    .unwrap_err()
                    .failure()
                    .message
                    .as_ref()
                    .unwrap()
                    .to_owned()
                    .contains("the command did not finish")
            );
        }
        _ => unreachable!(),
    };
    Ok(())
}
#[tokio::test]
async fn the_symbol_scans_of_test_parsing_run_at_the_same_time() -> TestResult {
    scan_case(0).await
}
#[tokio::test]
async fn a_symbol_scan_that_can_not_open_a_session_next_to_the_other_one_runs_after_it() -> TestResult {
    scan_case(1).await
}
#[tokio::test]
async fn a_symbol_scan_that_fails_for_another_reason_is_not_repeated() -> TestResult {
    scan_case(2).await
}
#[tokio::test]
async fn a_symbol_scan_that_does_not_finish_fails_parsing_instead_of_losing_its_tests() -> TestResult {
    scan_case(3).await
}
#[test]
fn symbol_edge_cases_keep_order_duplicates_and_empty_objc_package() {
    let swift = [
        "\u{a0}Tests.Class.testA(\u{a0}",
        "Tests.Class.testDISABLE()",
        "Too.Many.Class.testX()",
        "Tests.Class.testA(",
    ];
    let objc = ["Class testB", "Class  testC", "Class testB extra", "Class testB"];
    let tests = parse_symbols(&swift.map(str::to_owned), &objc.map(str::to_owned));
    assert_eq!(tests.len(), 4);
    let row = include_str!("../../../fixtures/oracle/test-runner/probes.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|r| r["case"] == "nm-symbols")
        .unwrap();
    assert_eq!(
        serde_json::json!(
            tests
                .iter()
                .map(|t| vec![t.pkg.to_owned(), t.clazz.to_owned(), t.method.to_owned()])
                .collect::<Vec<_>>()
        ),
        row["tests"]
    );
    assert_eq!(tests[0], tests[1]);
    assert_eq!(tests[2], tests[3]);
    assert_eq!(tests[2].pkg, String::default());
}

#[tokio::test]
async fn wrong_device_is_retried_three_times_with_the_jvm_logs() -> TestResult {
    use marathon_apple::{
        apple_test_bundle_identifier::AppleTestBundleIdentifier, cmd::remote::ssh::sshj::config::SshLogging,
        nm_test_parser::NmTestParser,
    };
    use marathon_core::vendor::DeviceRef;
    let tmp = tempfile::tempdir()?;
    let c = input(false, false);
    let device = Arc::new(Fake::new(c.clone(), tmp.path()));
    let logs = Arc::new(std::sync::Mutex::new(vec![]));
    let captured = logs.clone();
    let logging = SshLogging::new(
        device.logging.registry.clone(),
        Arc::new(move |logger, level, message, cause| {
            captured.lock().unwrap().push(serde_json::json!({"logger":logger.name(),"level":format!("{level:?}").to_uppercase(),"message":message,"throwable":cause.map(|f|f.stack_trace().to_owned())}));
        }),
    );
    let parser = NmTestParser::new(
        Arc::new(configuration(&c, tmp.path())?),
        Arc::default(),
        Arc::new(AppleTestBundleIdentifier::default()),
        vec![],
        Arc::new(support::Runtime),
        logging,
    );
    let error = parser
        .extract(DeviceRef(device.clone()), support::context())
        .await
        .unwrap_err();
    v(device.dispose().await)?;
    let row = include_str!("../../../fixtures/oracle/test-runner/probes.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|r| r["case"] == "nm-wrong-device")
        .unwrap();
    assert_eq!(
        error.failure().kind,
        marathon_test_support::recorded_errors::kind(row["error"]["class"].as_str().unwrap())
    );
    let mut expected_logs = row["logs"].clone();
    for log in expected_logs.as_array_mut().unwrap() {
        log["throwable"] = marathon_test_support::recorded_errors::rendered(log["throwable"].as_str().unwrap())
            .unwrap()
            .into();
    }
    assert_eq!(serde_json::json!(*logs.lock().unwrap()), expected_logs);
    Ok(())
}
#[tokio::test]
async fn filtered_tests_are_registered_before_filtering_and_keep_bundle_identity() -> TestResult {
    use marathon_config::vendor::TestClassRegex;
    let tmp = tempfile::tempdir()?;
    let c = input(false, false);
    prepare(tmp.path(), &c)?;
    let device = Arc::new(Fake::new(c.clone(), tmp.path()));
    let old = device.recorder.worker.before.lock().unwrap().clone();
    *device.recorder.worker.before.lock().unwrap() = Arc::new(move |command| {
        let old = old.clone();
        Box::pin(async move {
            if command[0] == "sh" && command[2].contains("nm ") {
                return Ok(Some(symbols(command[2].contains("swift-demangle"))));
            }
            old(command).await
        })
    });
    let (mut parser, id) = nm(&device, configuration(&c, tmp.path())?);
    parser.test_class_regexes = vec![TestClassRegex::Explicit(
        jcompat::JavaRegex::compile("NeverMatches", "testClassRegexes").unwrap(),
    )];
    let tests = v(parser.extract(id, support::context()).await)?;
    assert!(tests.is_empty());
    let test = marathon_core::model::Test {
        pkg: "UITests".into(),
        clazz: "LoginTests".into(),
        method: "testLogin".into(),
        meta_properties: vec![],
    };
    let swift = v(parser.identifier.bundle(&test))?;
    let mut objc = test;
    objc.pkg = "".into();
    let objc = v(parser.identifier.bundle(&objc))?;
    assert!(Arc::ptr_eq(&swift, &objc));
    v(device.dispose().await)?;
    Ok(())
}
