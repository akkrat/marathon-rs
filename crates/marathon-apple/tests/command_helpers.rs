//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/CommandExecutor.kt
//! Additional contract tests; there are no Kotlin matrix rows for these helpers.
use marathon_error::{AppleKind, FailureKind, SshKind};
mod support;
use marathon_apple::{
    cmd::{
        BaseCommand, CommandExecutor, CommandHost, CommandRequest, CommandResult, CommandSession, SessionControl,
        failure, local::KotlinProcessHost,
    },
    extensions::{Durations, bash_escape},
};
use marathon_core::{
    error::{Failure, VendorError},
    runtime::VendorFuture,
    vendor::VendorContext,
};
use marathon_logging::{Component, LogSettings};
use std::sync::{Arc, Mutex};
use support::*;

struct FaultExecutor {
    host: CommandHost,
    logger: Component,
    detail: Failure,
    during_await: bool,
    logs: Mutex<Vec<String>>,
}
impl FaultExecutor {
    fn new(class: FailureKind, during_await: bool) -> Self {
        Self {
            host: KotlinProcessHost::host(),
            logger: LogSettings::default().component("fault"),
            detail: failure(class, "detail"),
            during_await,
            logs: Mutex::new(Vec::new()),
        }
    }
}
impl CommandExecutor for FaultExecutor {
    fn host(&self) -> &CommandHost {
        &self.host
    }
    fn component(&self) -> &Component {
        &self.logger
    }
    fn connected(&self) -> bool {
        true
    }
    fn log_error(&self, message: String) {
        self.logs.lock().unwrap().push(message);
    }
    fn execute(&self, _: CommandRequest, _: VendorContext) -> VendorFuture<'_, CommandSession> {
        Box::pin(async move {
            if !self.during_await {
                return Err(VendorError::Other(self.detail.clone()));
            }
            let base = fake();
            base.stdout.close(None);
            base.stderr.close(None);
            base.exit_code.complete(Err(VendorError::Other(self.detail.clone())));
            Ok(CommandSession::new(
                base,
                SessionControl {
                    is_alive: Box::new(|| false),
                    terminate: Box::new(|| Ok(())),
                    kill: Box::new(|| Ok(())),
                    close: Box::new(|| Box::pin(async { Ok(()) })),
                },
            ))
        })
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
#[tokio::test]
async fn safe_execute_catches_only_the_two_transport_classes_at_both_boundaries() {
    for during_await in [false, true] {
        for (class, prefix) in [
            (FailureKind::Ssh(SshKind::Connection), "Unable to start"),
            (FailureKind::Ssh(SshKind::Transport), "Error starting"),
            // WP 4.2: subclasses are caught too (Kotlin `catch`), e.g. a MaxSessions open failure.
            (FailureKind::Ssh(SshKind::OpenChannel), "Unable to start"),
        ] {
            let executor = FaultExecutor::new(class, during_await);
            assert_eq!(
                executor
                    .safe_execute(CommandRequest::new(["echo", "hello"], duration(5000)), context())
                    .await
                    .unwrap(),
                None
            );
            assert_eq!(
                *executor.logs.lock().unwrap(),
                [format!("{prefix} a remote shell session [echo, hello]")]
            );
        }
        // Neither a superclass (SSHException, UserAuthException's parent) nor an unrelated class is caught.
        for class in [FailureKind::IllegalState, FailureKind::Ssh(SshKind::General)] {
            let executor = FaultExecutor::new(class, during_await);
            assert_eq!(
                executor
                    .safe_execute(CommandRequest::new(["echo"], duration(5000)), context())
                    .await,
                Err(VendorError::Other(executor.detail.clone()))
            );
            assert!(executor.logs.lock().unwrap().is_empty());
        }
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn failed_session_close_is_shared_then_retried_without_overlap() {
    // R2-wonder-4 (WP 4.2): callers during a failing close share its error, and the retry starts only after
    // that attempt's work has finished.
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let running = Arc::new(AtomicBool::new(false));
    let (c, r) = (calls.clone(), running.clone());
    let session = CommandSession::new(
        fake(),
        SessionControl {
            is_alive: Box::new(|| true),
            terminate: Box::new(|| Ok(())),
            kill: Box::new(|| Ok(())),
            close: Box::new(move || {
                let first = c.fetch_add(1, Ordering::SeqCst) == 0;
                assert!(!r.swap(true, Ordering::SeqCst), "two backend closes overlap");
                let r = r.clone();
                Box::pin(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    r.store(false, Ordering::SeqCst);
                    if first {
                        Err(VendorError::Other(failure(FailureKind::Io, "close")))
                    } else {
                        Ok(())
                    }
                })
            }),
        },
    );
    let (a, b) = tokio::join!(session.close(), session.close());
    assert_eq!(a, b);
    assert!(a.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(session.close().await, Ok(()));
    assert_eq!(session.close().await, Ok(()));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn critical_execute_preserves_trim_margin_indentation_and_nonzero_output() {
    let executor = executor();
    let error = executor
        .critical_execute(
            CommandRequest::new(["sh", "-c", "echo out; echo err >&2; exit 1"], duration(5000)),
            context(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        VendorError::Other(failure(
            FailureKind::Apple(AppleKind::Command),
            "command sh -c echo out; echo err >&2; exit 1 failed with exit code 1:\n                   stdout: out\n                   stderr: err"
        ))
    );
}
#[test]
fn duration_and_shell_helpers_match_reference_values() {
    assert_eq!(Durations::INFINITE.to_millis(), Some(604800000));
    assert_eq!(duration(i64::MAX).to_millis(), Some(i64::MAX));
    assert_eq!(jcompat::jtime::duration_of(-1, 999999999).unwrap().to_millis(), Some(0));
    assert_eq!(jcompat::jtime::Duration::of_seconds(i64::MAX).to_millis(), None);
    assert_eq!(bash_escape("it's a file"), "'it'\\''s a file'");
    let result = CommandResult {
        stdout: vec!["a".into(), "b".into()],
        stderr: vec![],
        exit_code: Some(0),
    };
    assert_eq!(result.combined_stdout(), "a\nb");
    assert!(result.successful_or_null().is_some());
}
#[cfg(feature = "test-support")]
#[tokio::test]
async fn fake_records_timeout_and_the_exact_unquoted_nohup_command() {
    let executor = marathon_apple::cmd::test_support::FakeCommandExecutor::new(
        Arc::new(Runtime),
        CommandResult {
            stdout: vec!["fixed".into()],
            stderr: vec![],
            exit_code: Some(0),
        },
        &mut LogSettings::default(),
    );
    let result = executor
        .safe_execute_nohup(
            "pid file",
            CommandRequest::new(["echo", "some value"], duration(100)),
            context(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.stdout, ["fixed"]);
    assert_eq!(*executor.timeout.lock().unwrap(), Some(duration(100)));
    assert_eq!(
        *executor.commands.lock().unwrap(),
        [vec![
            "sh",
            "-c",
            "nohup echo some value > /dev/null 2>&1 & echo $! > pid file"
        ]]
    );
}
#[tokio::test]
async fn explicit_close_is_joinable_after_its_awaiter_is_dropped() {
    let base = BaseCommand::new(1, Arc::new(Runtime));
    let scope = base.scope.clone();
    let ready = Arc::new(tokio::sync::Notify::new());
    let started = ready.clone();
    let release = Arc::new(tokio::sync::Notify::new());
    let waiting = release.clone();
    let session = Arc::new(CommandSession::new(
        base,
        SessionControl {
            is_alive: Box::new(|| false),
            terminate: Box::new(|| Ok(())),
            kill: Box::new(|| Ok(())),
            close: Box::new(move || {
                let started = started.clone();
                let waiting = waiting.clone();
                Box::pin(async move {
                    started.notify_one();
                    waiting.notified().await;
                    Ok(())
                })
            }),
        },
    ));
    let closing = session.clone();
    let task = tokio::spawn(async move { closing.close().await });
    ready.notified().await;
    task.abort();
    let _ = task.await;
    release.notify_one();
    session.close().await.unwrap();
    assert!(marathon_core::runtime::CancellationSignal::reason(scope.as_ref()).is_some());
}
#[tokio::test]
async fn failed_backend_close_can_be_retried() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let attempts = Arc::new(AtomicUsize::new(0));
    let called = attempts.clone();
    let session = CommandSession::new(
        fake(),
        SessionControl {
            is_alive: Box::new(|| false),
            terminate: Box::new(|| Ok(())),
            kill: Box::new(|| Ok(())),
            close: Box::new(move || {
                let attempt = called.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if attempt == 0 {
                        Err(VendorError::Other(failure(FailureKind::Timeout, "close wait")))
                    } else {
                        Ok(())
                    }
                })
            }),
        },
    );
    assert!(session.close().await.is_err());
    session.close().await.unwrap();
    session.close().await.unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}
