//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/SshjCommandExecutor.kt

use super::channel::{ExecChannel, Stream};
use super::client::{SshConnection, SshFuture};
use super::config::{PerformanceDefaultConfig, SshLogging};
use super::error::{IO_EXCEPTION, SshFailure};
use super::host::SshCommandHost;
use super::session::session_control;
use crate::cmd::{BaseCommand, CommandExecutor, CommandHost, CommandRequest, CommandSession, failure, millis};
use crate::extensions::{
    poll_backoff::PollBackoff,
    reader::{current_time_millis, produce_lines_manually},
};
use jcompat::logging::{Level, Logger};
use marathon_core::{
    error::{Cancellation, Failure, VendorError, VendorResult},
    runtime::{CancellationSignal, VendorClock, VendorFuture, VendorRuntime},
    vendor::VendorContext,
};
use russh::Sig;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI64, Ordering},
};
use tokio::sync::oneshot;

pub struct SshCommandExecutor {
    pub ssh_host: SshCommandHost,
    connection: Arc<SshConnection>,
    channel_capacity: usize,
    logger: Logger,
    logging: SshLogging,
    config: PerformanceDefaultConfig,
    runtime: Arc<dyn VendorRuntime>,
}
impl SshCommandExecutor {
    pub fn new(
        host: SshCommandHost,
        connection: SshConnection,
        channel_capacity: usize,
        config: PerformanceDefaultConfig,
    ) -> Self {
        let logging = config.logging.clone();
        Self {
            logger: logging.logger("com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.SshjCommandExecutor"),
            runtime: connection.runtime.clone(),
            connection: Arc::new(connection),
            ssh_host: host,
            channel_capacity,
            logging,
            config,
        }
    }
    pub fn connection(&self) -> &SshConnection {
        &self.connection
    }
}

/// The command line that the shell of the user on the worker runs for `command` (A12: kept byte-identical).
pub fn shell_command(command: &[String]) -> String {
    command
        .iter()
        .map(|arg| {
            if is_shellscaped(arg) {
                arg.clone()
            } else if arg.chars().any(|c| SHELL_SPECIAL_CHARS.contains(&c)) {
                format!("'{}'", arg.replace('\'', "\\'"))
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
/// The exec string for `request`: its command line (A2: env and workdir never reach the worker), except for D32's one
/// variable, which goes in the string because no env request is sent. `env` rather than a `VAR=x` prefix because a login
/// shell can be csh, where `VAR=x cmd` is a syntax error. The command is `xcrun xcodebuild …`, not a compound one, so the
/// variable reaches xcodebuild; `xcrun` passes its environment on.
pub fn exec_string(request: &CommandRequest) -> String {
    let command = shell_command(&request.command);
    if request.env.get("NSUnbufferedIO").is_some_and(|value| value == "YES") {
        format!("env NSUnbufferedIO=YES {command}")
    } else {
        command
    }
}
/// Verifies only fully escaped strings without verifying special characters.
fn is_shellscaped(arg: &str) -> bool {
    (arg.starts_with('\'') && arg.ends_with('\'')) || (arg.starts_with('"') && arg.ends_with('"'))
}
const SHELL_SPECIAL_CHARS: [char; 22] = [
    '"', ' ', '$', '\'', '\\', '#', '=', '[', ']', '!', '>', '<', '|', ';', '{', '}', '(', ')', '*', '?', '~', '&',
];

/// The `Session.Command` surface that `suspendFor` uses; the Kotlin tests mock it.
pub trait ExitState: Send + Sync {
    fn is_open(&self) -> bool;
    /// `join(1, NANOSECONDS)`.
    fn join_poll(&self) -> Result<(), SshFailure>;
    fn close(&self) -> SshFuture<'_, ()>;
    fn exit_status(&self) -> Option<i32>;
    fn exit_signal(&self) -> Option<String>;
}
impl ExitState for ExecChannel {
    fn is_open(&self) -> bool {
        ExecChannel::is_open(self)
    }
    fn join_poll(&self) -> Result<(), SshFailure> {
        ExecChannel::join_poll(self)
    }
    fn close(&self) -> SshFuture<'_, ()> {
        Box::pin(ExecChannel::close(self))
    }
    fn exit_status(&self) -> Option<i32> {
        ExecChannel::exit_status(self)
    }
    fn exit_signal(&self) -> Option<String> {
        ExecChannel::exit_signal(self)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExitWaitError {
    Cancelled(Cancellation),
    Failed(SshFailure),
}
impl ExitWaitError {
    pub fn into_vendor(self) -> VendorError {
        match self {
            Self::Cancelled(reason) => VendorError::Cancelled(reason),
            Self::Failed(failure) => failure.into_vendor(),
        }
    }
}

/// `Command.suspendFor()`: poll `isOpen` with PollBackoff; `join` only once the channel stopped being open,
/// retrying its poll timeout and rethrowing any other ConnectionException; then `close()` and the exit code.
pub async fn suspend_for<C: ExitState + ?Sized>(
    command: &C,
    clock: &dyn VendorClock,
    cancellation: Arc<dyn CancellationSignal>,
) -> Result<Option<i32>, ExitWaitError> {
    let mut backoff = PollBackoff::default();
    loop {
        if !command.is_open() {
            match command.join_poll() {
                Ok(()) => break,
                Err(error)
                    if error
                        .cause
                        .as_ref()
                        .is_some_and(|cause| cause.class == super::error::TIMEOUT_EXCEPTION) => {}
                Err(error) => return Err(ExitWaitError::Failed(error)),
            }
        }
        if let Err(error) = clock.delay(backoff.next_millis(), cancellation.clone()).await {
            return Err(match error {
                VendorError::Cancelled(reason) => ExitWaitError::Cancelled(reason),
                other => ExitWaitError::Failed(SshFailure::new(IO_EXCEPTION, format!("{other:?}"))),
            });
        }
    }
    command.close().await.map_err(ExitWaitError::Failed)?;
    Ok(command
        .exit_status()
        .or_else(|| autodetect_exit_code(command.exit_signal().as_deref())))
}

pub fn autodetect_exit_code(exit_signal: Option<&str>) -> Option<i32> {
    match exit_signal {
        Some("TERM") => Some(143),
        _ => None,
    }
}

async fn execute(
    connection: Arc<SshConnection>,
    request: CommandRequest,
    context: VendorContext,
    capacity: usize,
    config: PerformanceDefaultConfig,
) -> VendorResult<CommandSession> {
    if let Some(reason) = context.cancellation.reason() {
        return Err(VendorError::Cancelled(reason));
    }
    let runtime = connection.runtime.clone();
    let channel = connection.start_session().await.map_err(SshFailure::into_vendor)?;
    let command = exec_string(&request);
    let channel_logger = config.logger_for_class(Some("PatchedExecChannel"));
    config.logging.log(
        &channel_logger,
        Level::Debug,
        || format!("Will request to exec `{command}`"),
        None,
    );
    channel.exec(command).await.map_err(SshFailure::into_vendor)?;
    let base = BaseCommand::new(capacity, runtime.clone());
    let initial = match current_time_millis(&context) {
        Ok(value) => value,
        Err(error) => {
            let _ = channel.signal(Sig::TERM);
            let _ = channel.close_escalating().await;
            return Err(error);
        }
    };
    let last_output = Arc::new(AtomicI64::new(initial));
    let own_context = VendorContext {
        cancellation: base.scope.clone(),
        ..context.clone()
    };
    for (stream, output) in [
        (Stream::Stdout, base.stdout.clone()),
        (Stream::Stderr, base.stderr.clone()),
    ] {
        let reader = channel.reader(stream);
        let readable = channel.clone();
        let last = last_output.clone();
        let reader_context = own_context.clone();
        let idle_timeout = request.idle_timeout;
        base.scope.track(runtime.spawn(Box::pin(async move {
            produce_lines_manually(reader, output, last, idle_timeout, reader_context, move || {
                readable.is_open() && !readable.is_eof()
            })
            .await
        })));
    }
    let closing = Arc::new(AtomicBool::new(false));
    base.scope.track(runtime.spawn(Box::pin(exit_code(
        channel.clone(),
        base.exit_code.clone(),
        base.scope.clone(),
        own_context.clock.clone(),
        request.timeout,
        closing.clone(),
    ))));
    let control = session_control(channel, base.scope.clone(), closing);
    Ok(CommandSession::new(base, control))
}

type ExitWait = Result<Option<i32>, ExitWaitError>;

/// `async(job) { withTimeoutOrNull(timeout) { cmd.suspendFor() } … }`.
async fn exit_code(
    channel: Arc<ExecChannel>,
    exit: crate::cmd::ExitCode,
    scope: Arc<crate::cmd::SessionScope>,
    clock: Arc<dyn VendorClock>,
    timeout: jcompat::jtime::Duration,
    closing: Arc<AtomicBool>,
) -> VendorResult<()> {
    let result = async {
        let timeout_millis = millis(timeout)?;
        let waited: Arc<Mutex<Option<ExitWait>>> = Arc::new(Mutex::new(None));
        let cell = waited.clone();
        let command = channel.clone();
        let wait_clock = clock.clone();
        let outcome = clock
            .with_timeout_or_null(
                scope.clone(),
                timeout_millis,
                Box::new(move |child| {
                    Box::pin(async move {
                        let result = suspend_for(command.as_ref(), wait_clock.as_ref(), child).await;
                        // Expiry reaches the waiter as cancellation; withTimeoutOrNull recognises its own.
                        let propagate = match &result {
                            Err(ExitWaitError::Cancelled(reason)) => Err(VendorError::Cancelled(reason.clone())),
                            _ => Ok(()),
                        };
                        *cell.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
                        propagate
                    })
                }),
            )
            .await?;
        match outcome {
            Some(()) => match waited.lock().unwrap_or_else(|e| e.into_inner()).take() {
                // D7: a null result without expiry (another signal, no status) sends no stale TERM; the
                // following close() is a no-op on the closed channel.
                Some(Ok(code)) => Ok(code),
                Some(Err(error)) => Err(error.into_vendor()),
                None => Err(VendorError::Other(failure(
                    "java.lang.IllegalStateException",
                    "no exit wait result",
                ))),
            },
            None => {
                // The expired wait left the channel alone. TERM, then the bounded close, which gives a process that TERM
                // does not end KILL, and fails if the peer never answers (T15h, D30).
                channel.signal(Sig::TERM).map_err(SshFailure::into_vendor)?;
                channel.close_escalating().await.map_err(SshFailure::into_vendor)?;
                Ok(None)
            }
        }
    }
    .await;
    // Approved P2: once session close has begun, the exit code is left to the session's cancellation.
    if closing.load(Ordering::SeqCst) {
        let _ = scope.cancelled().await;
        return Ok(());
    }
    exit.complete(result);
    Ok(())
}

impl CommandExecutor for SshCommandExecutor {
    fn host(&self) -> &CommandHost {
        &self.ssh_host.host
    }
    fn logger(&self) -> &Logger {
        &self.logger
    }
    fn connected(&self) -> bool {
        self.connection.is_connected()
    }
    fn log_error(&self, message: String) {
        self.logging.log(&self.logger, Level::Error, || message, None);
    }
    fn log_error_with(&self, message: String, cause: &Failure) {
        if self.logging.enabled(&self.logger, Level::Error) {
            (self.logging.sink)(&self.logger, Level::Error, message, Some(cause.clone()));
        }
    }
    fn execute(&self, request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandSession> {
        let connection = self.connection.clone();
        let capacity = self.channel_capacity;
        let config = self.config.clone();
        let runtime = self.runtime.clone();
        Box::pin(async move {
            // russh needs Tokio: the whole exchange runs on the vendor runtime, which owns the session until
            // the caller takes it (an abandoned execute closes it there).
            let (tx, rx) = oneshot::channel();
            let (accepted, acceptance) = oneshot::channel::<()>();
            runtime.spawn(Box::pin(async move {
                match execute(connection, request, context, capacity, config).await {
                    Ok(session) => {
                        let pending = Arc::new(Mutex::new(Some(session)));
                        if tx.send(Ok(pending.clone())).is_ok() {
                            let _ = acceptance.await;
                        }
                        let undelivered = pending.lock().unwrap_or_else(|e| e.into_inner()).take();
                        if let Some(session) = undelivered {
                            session.close().await?;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error));
                    }
                }
                Ok(())
            }));
            let pending = rx
                .await
                .map_err(|e| VendorError::Other(failure("java.lang.IllegalStateException", e.to_string())))??;
            let session = pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .ok_or_else(|| {
                    VendorError::Other(failure("java.lang.IllegalStateException", "session already delivered"))
                })?;
            let _ = accepted.send(());
            Ok(session)
        })
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        let connection = self.connection.clone();
        let runtime = self.runtime.clone();
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            runtime.spawn(Box::pin(async move {
                let _ = tx.send(connection.close().await);
                Ok(())
            }));
            if let Ok(Err(error)) = rx.await
                && error.is_io()
            {
                // Kotlin's message really lacks a separator between address and port (kept under D5).
                self.log_error_with(
                    format!("Error disconnecting {}{}", self.ssh_host.addr, self.ssh_host.port),
                    &error.to_failure(),
                );
            }
            Ok(())
        })
    }
}
