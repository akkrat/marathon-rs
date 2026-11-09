//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/NmTestParser.kt
//! and XCTestParser.kt (Exception-only retry logging).
use marathon_error::{FailureKind, FatalKind, InjectedKind};
#[path = "support/parser.rs"]
mod parser_support;
mod support;
#[path = "support/test_root_device.rs"]
mod test_root_device;
use marathon_apple::{
    apple_application_installer::AppleApplicationInstaller,
    apple_device_registry::AppleDeviceRegistry,
    cmd::{failure, remote::ssh::sshj::config::SshLogging},
    nm_test_parser::NmTestParser,
    xctest_parser::XCTestParser,
};
use marathon_core::{
    error::VendorError,
    vendor::{Device, RemoteTestParser},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use test_root_device::{Fake, TestResult, prepare, v};

#[tokio::test]
async fn both_parsers_retry_errors_without_logging_and_log_exceptions_named_error() -> TestResult {
    for xctest in [false, true] {
        for (class, is_error) in [
            (FailureKind::Fatal(FatalKind::Assertion), true),
            (FailureKind::Fatal(FatalKind::ThreadDeath), true),
            (FailureKind::Injected(InjectedKind::ProbeExceptionNamedError), false),
        ] {
            let tmp = tempfile::tempdir()?;
            let input = parser_support::input(false, true);
            prepare(tmp.path(), &input)?;
            let device = Arc::new(Fake::new(input.clone(), tmp.path()));
            let attempts = Arc::new(AtomicUsize::new(0));
            let count = attempts.clone();
            let old = device.recorder.worker.before.lock().unwrap().clone();
            let failure_kind = class.clone();
            *device.recorder.worker.before.lock().unwrap() = Arc::new(move |command| {
                let old = old.clone();
                let count = count.clone();
                let class = failure_kind.clone();
                Box::pin(async move {
                    if command[0] == "test" {
                        count.fetch_add(1, Ordering::SeqCst);
                        return Err(VendorError::Other(failure(class, "remote failed")));
                    }
                    old(command).await
                })
            });
            let logs = Arc::new(Mutex::new(vec![]));
            let captured = logs.clone();
            let logging = SshLogging::new(
                device.logging.registry.clone(),
                Arc::new(move |logger, level, message, cause| {
                    if message == "Remote parsing failed. Retrying" {
                        captured.lock().unwrap().push((logger.name().to_owned(), level, cause));
                    }
                }),
            );
            let config = Arc::new(parser_support::configuration(&input, tmp.path())?);
            let runtime = Arc::new(support::Runtime);
            let registry = Arc::new(AppleDeviceRegistry::default());
            let id = registry.register(device.clone());
            let parser: Box<dyn RemoteTestParser> = if xctest {
                let installer =
                    AppleApplicationInstaller::new_owned((*config).clone(), runtime.clone(), logging.clone());
                Box::new(XCTestParser::new(
                    config,
                    registry,
                    Arc::default(),
                    installer,
                    runtime,
                    logging,
                    parser_support::parser_libraries(tmp.path())?,
                ))
            } else {
                Box::new(NmTestParser::new(
                    config,
                    registry,
                    Arc::default(),
                    vec![],
                    runtime,
                    logging,
                ))
            };
            let error = parser.extract(id, support::context()).await.unwrap_err();
            v(device.dispose().await)?;
            assert_eq!(error.failure().kind, class);
            assert_eq!(attempts.load(Ordering::SeqCst), 3);
            let logs = logs.lock().unwrap();
            assert_eq!(logs.len(), if is_error { 0 } else { 3 });
            for (logger, level, cause) in logs.iter() {
                assert_eq!(logger, if xctest { "marathon_apple::xctest_parser" } else { "marathon_apple::nm_test_parser" });
                assert_eq!(*level, marathon_logging::Level::DEBUG);
                assert_eq!(cause.as_ref(), Some(error.failure()));
            }
        }
    }
    Ok(())
}
