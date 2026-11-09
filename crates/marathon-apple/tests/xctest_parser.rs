//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/XCTestParserTest.kt
#[path = "support/parser.rs"]
mod parser_support;
#[path = "support/runner_device.rs"]
mod runner_device;
mod support;
#[path = "support/test_root_device.rs"]
mod test_root_device;
use marathon_apple::{
    apple_application_installer::AppleApplicationInstaller,
    apple_test_bundle_identifier::AppleTestBundleIdentifier,
    cmd::cancelled,
    test::TestEvent,
    xctest_parser::{PARSER_DIR_VARIABLE, ParserLibraries, XCTestParser},
};
use marathon_core::{
    error::VendorError,
    model::Test,
    vendor::{Device, TestBundleIdentifier},
};
use runner_device::{Plan, RunnerDevice};
use std::sync::Arc;
use test_root_device::{Fake, TestResult, prepare, v};
fn test() -> Test {
    Test {
        pkg: "Tests".into(),
        clazz: "SampleTests".into(),
        method: "testSample".into(),
        meta_properties: vec![],
    }
}
async fn parse(timeout: bool, cancel: bool) -> TestResult {
    let tmp = tempfile::tempdir()?;
    let c = parser_support::input(false, false);
    prepare(tmp.path(), &c)?;
    let inner = Fake::new(c.clone(), tmp.path());
    let bundle = parser_support::bundle(&inner, &c).await?;
    let logging = inner.logging.clone();
    let device = RunnerDevice::new(inner, logging.clone());
    *device.plan.lock().unwrap() = Plan {
        events: vec![vec![
            TestEvent::TestStarted { id: test() },
            TestEvent::TestStarted { id: test() },
            TestEvent::TestRunFailed {
                message: "ignored".into(),
                reason: marathon_apple::logparser::parser::DeviceFailureReason::Unknown,
            },
        ]],
        wait_close: true,
        output_failure: cancel.then(|| VendorError::Cancelled(cancelled("parse cancelled"))),
        ..Default::default()
    };
    let config = Arc::new(parser_support::configuration(&c, tmp.path())?);
    let identifier = Arc::new(AppleTestBundleIdentifier::default());
    let runtime = Arc::new(support::Runtime);
    let installer = AppleApplicationInstaller::new(&config, runtime.clone(), logging.clone());
    let mut parser = XCTestParser::new(
        config.clone(),
        Arc::default(),
        identifier.clone(),
        installer,
        runtime,
        logging,
        ParserLibraries::default(),
    );
    parser.session_close_timeout = support::duration(200);
    let ctx = support::context();
    let collect = parser.collect_tests(&device, bundle.clone(), &ctx);
    tokio::pin!(collect);
    tokio::select! {result=&mut collect=>panic!("returned before close {result:?}"),_=device.closing.notified()=>()};
    if !timeout {
        device.release.notify_one();
    }
    let tests = v(collect.await)?;
    device.calls.lock().unwrap().push("parsed".into());
    let calls = device.calls.lock().unwrap().clone();
    let session = device.session.lock().unwrap().clone().unwrap();
    let was_closed = session.closed();
    let abandoned = session.abandoned();
    v(device.dispose().await)?;
    assert_eq!(tests.len(), 1);
    assert_eq!(tests[0].method, test().method);
    assert_eq!(tests[0].pkg, String::from(bundle.test_bundle_id()));
    assert!(identifier.identify(&tests[0]).is_ok());
    assert_eq!(was_closed, !timeout);
    assert_eq!(abandoned, timeout);
    assert!(session.closed());
    assert_eq!(
        calls,
        if timeout {
            vec!["add listener", "remove listener", "parsed"]
        } else {
            vec!["add listener", "closed", "remove listener", "parsed"]
        }
    );
    Ok(())
}
#[tokio::test]
async fn parsing_ends_only_after_its_xcodebuild_session_has_closed() -> TestResult {
    parse(false, false).await
}
#[tokio::test]
async fn parsing_keeps_its_tests_when_its_xcodebuild_session_does_not_close_in_time() -> TestResult {
    parse(true, false).await
}
#[tokio::test]
async fn cancellation_is_swallowed_and_partial_tests_are_registered() -> TestResult {
    parse(false, true).await
}
#[tokio::test]
async fn identifier_ignores_metadata_overwrites_and_preserves_identity() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let c = parser_support::input(false, false);
    prepare(tmp.path(), &c)?;
    let device = Fake::new(c.clone(), tmp.path());
    let first = parser_support::bundle(&device, &c).await?;
    let second = parser_support::bundle(&device, &c).await?;
    let identifier = AppleTestBundleIdentifier::default();
    let test = test();
    assert!(identifier.identify(&test).is_err());
    identifier.put(test.clone(), first.clone());
    let before = v(identifier.identify(&test))?.unwrap();
    identifier.put(test.clone(), second.clone());
    let after = v(identifier.identify(&test))?.unwrap();
    assert!(before != after);
    assert!(Arc::ptr_eq(&v(identifier.bundle(&test))?, &second));
    v(device.dispose().await)?;
    Ok(())
}

#[tokio::test]
async fn full_extract_pushes_supplied_bytes_cleans_temp_files_and_keeps_the_push_error_typo() -> TestResult {
    use marathon_core::vendor::RemoteTestParser;
    for succeeds in [true, false] {
        let tmp = tempfile::tempdir()?;
        let c = parser_support::input(false, false);
        prepare(tmp.path(), &c)?;
        let inner = Fake::new(c.clone(), tmp.path());
        let logging = inner.logging.clone();
        let mut raw = RunnerDevice::new(inner, logging.clone());
        raw.dylib_push_succeeds = succeeds;
        *raw.plan.lock().unwrap() = Plan {
            events: vec![vec![TestEvent::TestStarted { id: test() }]],
            ..Default::default()
        };
        let device = Arc::new(raw);
        let registry = Arc::new(marathon_apple::apple_device_registry::AppleDeviceRegistry::default());
        let id = registry.register(device.clone());
        let config = Arc::new(parser_support::configuration(&c, tmp.path())?);
        let runtime = Arc::new(support::Runtime);
        let installer = AppleApplicationInstaller::new_owned((*config).clone(), runtime.clone(), logging.clone());
        let libraries = parser_support::parser_libraries(tmp.path())?;
        let parser = XCTestParser::new(config, registry, Arc::default(), installer, runtime, logging, libraries);
        // This is the concrete type a vendor factory stores in VendorServices.
        let parser: Arc<dyn RemoteTestParser> = Arc::new(parser);
        let result = parser.extract(id, support::context()).await;
        v(device.dispose().await)?;
        if succeeds {
            assert_eq!(v(result)?.len(), 1);
            let bridge = device.inner.bridge.lock().unwrap();
            assert!(
                bridge
                    .iter()
                    .any(|(path, bytes)| path.ends_with("libxctest-parser.dylib")
                        && bytes == parser_support::STAND_IN_LIBRARY)
            );
            let request = device.requests.lock().unwrap()[0].clone();
            assert!(!request.coverage);
            assert!(request.tests.is_none());
            assert!(request.xcresult.is_none());
            assert!(request.test_target_name.is_none());
        } else {
            let error = result.unwrap_err();
            assert!(matches!(error, VendorError::TestParsing(_)));
            let row = include_str!("../../../fixtures/oracle/test-runner/probes.jsonl")
                .lines()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                .find(|r| r["case"] == "xctest-push-false")
                .unwrap();
            assert_eq!(
                error.failure().message.as_ref().unwrap().to_owned(),
                row["error"]["message"].as_str().unwrap()
            );
        }
        let staged = device.staged.lock().unwrap().clone();
        assert_eq!(staged.len(), if succeeds { 1 } else { 3 });
        for path in staged.iter() {
            assert!(tokio::fs::metadata(path).await.is_err());
        }
    }
    Ok(())
}

#[tokio::test]
async fn empty_parse_returns_empty_and_warns_with_the_jvm_text() -> TestResult {
    use marathon_apple::cmd::remote::ssh::sshj::config::SshLogging;
    let tmp = tempfile::tempdir()?;
    let c = parser_support::input(false, false);
    prepare(tmp.path(), &c)?;
    let inner = Fake::new(c.clone(), tmp.path());
    let bundle = parser_support::bundle(&inner, &c).await?;
    let logs = Arc::new(std::sync::Mutex::new(vec![]));
    let captured = logs.clone();
    let logging = SshLogging::new(
        inner.logging.registry.clone(),
        Arc::new(move |logger, level, message, _| {
            if logger.name() == "marathon_apple::xctest_parser" {
                captured.lock().unwrap().push(serde_json::json!({"logger":logger.name(),"level":format!("{level:?}").to_uppercase(),"message":message}));
            }
        }),
    );
    let device = RunnerDevice::new(inner, logging.clone());
    let config = Arc::new(parser_support::configuration(&c, tmp.path())?);
    let runtime = Arc::new(support::Runtime);
    let installer = AppleApplicationInstaller::new_owned((*config).clone(), runtime.clone(), logging.clone());
    let parser = XCTestParser::new(
        config,
        Arc::default(),
        Arc::default(),
        installer,
        runtime,
        logging,
        ParserLibraries::default(),
    );
    let result = parser.collect_tests(&device, bundle, &support::context()).await;
    v(device.dispose().await)?;
    assert!(v(result)?.is_empty());
    let row = include_str!("../../../fixtures/oracle/test-runner/probes.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|r| r["case"] == "xctest-empty")
        .unwrap();
    assert_eq!(serde_json::json!(*logs.lock().unwrap()), marathon_test_support::recorded_logs::native_logs(&row["logs"]));
    Ok(())
}

#[tokio::test]
async fn physical_sdk_extract_names_the_unsupported_platform_and_attempts_three_times() -> TestResult {
    use marathon_core::vendor::RemoteTestParser;
    let tmp = tempfile::tempdir()?;
    let mut input = parser_support::input(false, false);
    input["sdk"] = "TV".into();
    prepare(tmp.path(), &input)?;
    let inner = Fake::new(input.clone(), tmp.path());
    let logs = Arc::new(std::sync::Mutex::new(vec![]));
    let captured = logs.clone();
    let logging = marathon_apple::cmd::remote::ssh::sshj::config::SshLogging::new(
        inner.logging.registry.clone(),
        Arc::new(move |logger, level, message, cause| {
            if logger.name() == "marathon_apple::xctest_parser" {
                captured.lock().unwrap().push((level, message, cause));
            }
        }),
    );
    let device = Arc::new(RunnerDevice::new(inner, logging.clone()));
    let registry = Arc::new(marathon_apple::apple_device_registry::AppleDeviceRegistry::default());
    let id = registry.register(device.clone());
    let config = Arc::new(parser_support::configuration(&input, tmp.path())?);
    let runtime = Arc::new(support::Runtime);
    let installer = AppleApplicationInstaller::new_owned((*config).clone(), runtime.clone(), logging.clone());
    let libraries = parser_support::parser_libraries(tmp.path())?;
    let parser = XCTestParser::new(config, registry, Arc::default(), installer, runtime, logging, libraries);
    let error = parser.extract(id, support::context()).await.unwrap_err();
    v(device.dispose().await)?;
    assert!(matches!(error, VendorError::TestParsing(_)), "{error:?}");
    assert_eq!(error.failure().kind.description().to_owned(), "test discovery failure");
    assert_eq!(
        error.failure().message.as_ref().unwrap().to_owned(),
        "No libxctest-parser.dylib for platform AppleTVOS"
    );
    let logs = logs.lock().unwrap();
    assert_eq!(logs.len(), 3);
    for (level, message, cause) in logs.iter() {
        assert_eq!(*level, marathon_logging::Level::DEBUG);
        assert_eq!(message, "Remote parsing failed. Retrying");
        assert_eq!(cause.as_ref(), Some(error.failure()));
    }
    assert_eq!(
        device
            .inner
            .bridge
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _)| path.ends_with("marathon.xctestrun"))
            .count(),
        3
    );
    assert!(device.requests.lock().unwrap().is_empty());
    assert!(device.staged.lock().unwrap().is_empty());
    Ok(())
}

/// D20: `extract` with `libraries` on an iPhone simulator; returns the single error and what reached the device.
async fn extract_failure(libraries: ParserLibraries, tmp: &std::path::Path) -> TestResult<(VendorError, usize, usize)> {
    use marathon_core::vendor::RemoteTestParser;
    let c = parser_support::input(false, false);
    prepare(tmp, &c)?;
    let inner = Fake::new(c.clone(), tmp);
    let logging = inner.logging.clone();
    let device = Arc::new(RunnerDevice::new(inner, logging.clone()));
    let registry = Arc::new(marathon_apple::apple_device_registry::AppleDeviceRegistry::default());
    let id = registry.register(device.clone());
    let config = Arc::new(parser_support::configuration(&c, tmp)?);
    let runtime = Arc::new(support::Runtime);
    let installer = AppleApplicationInstaller::new_owned((*config).clone(), runtime.clone(), logging.clone());
    let parser = XCTestParser::new(config, registry, Arc::default(), installer, runtime, logging, libraries);
    let error = parser.extract(id, support::context()).await.unwrap_err();
    v(device.dispose().await)?;
    let transferred = device.inner.bridge.lock().unwrap().len();
    let staged = device.staged.lock().unwrap().len();
    assert!(device.requests.lock().unwrap().is_empty());
    Ok((error, transferred, staged))
}
fn assert_explains_the_supply(error: &VendorError, names: &str) {
    assert!(matches!(error, VendorError::TestParsing(_)), "{error:?}");
    let message = error.failure().message.as_ref().unwrap().to_owned();
    for part in [
        names,
        PARSER_DIR_VARIABLE,
        "<platform name>/libxctest-parser.dylib",
        "EULA",
        "sends error reports to its server",
        "nm test parser",
    ] {
        assert!(message.contains(part), "{part} missing from {message}");
    }
}
#[tokio::test]
async fn extract_with_the_variable_unset_fails_once_before_installing_anything() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let (error, transferred, staged) = extract_failure(ParserLibraries::default(), tmp.path()).await?;
    assert_explains_the_supply(&error, "is not set");
    assert_eq!((transferred, staged), (0, 0));
    Ok(())
}
#[tokio::test]
async fn extract_without_the_devices_platform_file_names_it_and_does_not_retry() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let supplied = tmp.path().join("only-macos");
    std::fs::create_dir_all(supplied.join("MacOSX"))?;
    std::fs::write(
        supplied.join("MacOSX/libxctest-parser.dylib"),
        parser_support::STAND_IN_LIBRARY,
    )?;
    let (error, transferred, staged) = extract_failure(ParserLibraries::new(&supplied), tmp.path()).await?;
    assert_explains_the_supply(&error, "iPhoneSimulator/libxctest-parser.dylib cannot be read");
    assert_eq!((transferred, staged), (0, 0));
    Ok(())
}
#[tokio::test]
async fn extract_with_an_unreadable_platform_file_fails_before_installing_anything() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir()?;
    let supplied = tmp.path().join("not-a-file");
    std::fs::create_dir_all(supplied.join("iPhoneSimulator/libxctest-parser.dylib"))?;
    let (error, transferred, staged) = extract_failure(ParserLibraries::new(&supplied), tmp.path()).await?;
    assert_explains_the_supply(&error, "is not a file");
    assert_eq!((transferred, staged), (0, 0));

    let tmp = tempfile::tempdir()?;
    let libraries = parser_support::parser_libraries(tmp.path())?;
    let file = tmp
        .path()
        .join("supplied-parser/iPhoneSimulator/libxctest-parser.dylib");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000))?;
    // Root reads a file of any mode, which leaves nothing to observe.
    if std::fs::File::open(&file).is_err() {
        let (error, transferred, staged) = extract_failure(libraries, tmp.path()).await?;
        assert_explains_the_supply(&error, "cannot be read");
        assert_eq!((transferred, staged), (0, 0));
    }
    Ok(())
}
#[tokio::test]
async fn extract_with_a_fifo_as_the_platform_file_fails_instead_of_blocking() -> TestResult {
    use std::sync::atomic::{AtomicBool, Ordering};
    let tmp = tempfile::tempdir()?;
    let supplied = tmp.path().join("fifo");
    std::fs::create_dir_all(supplied.join("iPhoneSimulator"))?;
    let fifo = supplied.join("iPhoneSimulator/libxctest-parser.dylib");
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status()?.success());
    // A blocked open cannot be cancelled, so a rescuer opens the write side at the 60 s hang deadline, which ends the
    // block and records that it had to. It leaves at once when the parser has finished.
    let finished = Arc::new(AtomicBool::new(false));
    let rescued = Arc::new(AtomicBool::new(false));
    let rescuer = {
        let (finished, rescued, fifo) = (finished.clone(), rescued.clone(), fifo.clone());
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while !finished.load(Ordering::SeqCst) {
                if std::time::Instant::now() > deadline {
                    rescued.store(true, Ordering::SeqCst);
                    let _ = std::fs::OpenOptions::new().write(true).open(&fifo);
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        })
    };
    let result = extract_failure(ParserLibraries::new(&supplied), tmp.path()).await;
    finished.store(true, Ordering::SeqCst);
    assert!(rescuer.join().is_ok());
    assert!(!rescued.load(Ordering::SeqCst), "extract blocked on the FIFO");
    let (error, transferred, staged) = result?;
    assert_explains_the_supply(&error, "is not a file");
    assert_eq!((transferred, staged), (0, 0));
    Ok(())
}
