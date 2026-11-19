//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/SshjCommandExecutor.kt

use super::channel::{ExecChannel, Stream};
use super::client::{SshConnection, SshFuture};
use super::config::{PerformanceDefaultConfig, SshLogging};
use super::error::{IO_EXCEPTION, SshFailure};
use super::host::SshCommandHost;
use super::session::session_control;
use super::supervisor::{ControlDial, Proof, Supervisor, Warnings};
use crate::cmd::{BaseCommand, CommandExecutor, CommandHost, CommandRequest, CommandSession, failure, millis};
use crate::extensions::{
    poll_backoff::PollBackoff,
    reader::{OutputActivity, produce_lines},
};
use marathon_core::{
    error::{Cancellation, Failure, VendorError, VendorResult},
    runtime::{CancellationSignal, VendorClock, VendorFuture, VendorRuntime},
    vendor::VendorContext,
};
use marathon_error::FailureKind;
use marathon_logging::{Component, Level};
use russh::Sig;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::oneshot;

pub struct SshCommandExecutor {
    pub ssh_host: SshCommandHost,
    connection: Arc<SshConnection>,
    channel_capacity: usize,
    logger: Component,
    logging: SshLogging,
    config: PerformanceDefaultConfig,
    runtime: Arc<dyn VendorRuntime>,
    supervisor: Option<Arc<Supervisor>>,
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
            logger: logging.component("marathon_apple::cmd::remote::ssh::sshj::client"),
            runtime: connection.runtime.clone(),
            connection: Arc::new(connection),
            ssh_host: host,
            channel_capacity,
            logging,
            config,
            supervisor: None,
        }
    }
    pub(super) fn with_worker_supervisor(
        mut self,
        warnings: Warnings,
        interval: jcompat::jtime::Duration,
        count: i32,
        control: ControlDial,
    ) -> Self {
        self.supervisor = Some(Arc::new(Supervisor::new(
            (self.ssh_host.addr.clone(), self.ssh_host.port),
            warnings,
            self.logging.clone(),
            interval,
            count,
            control,
        )));
        self
    }
    pub(super) fn into_connection(self) -> Arc<SshConnection> {
        self.connection
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
    // WP 8.12: PollBackoff remains only for this SSH exit waiter; output reads await readiness.
    let mut backoff = PollBackoff::default();
    loop {
        if !command.is_open() {
            match command.join_poll() {
                Ok(()) => break,
                Err(error)
                    if error
                        .cause
                        .as_ref()
                        .is_some_and(|cause| cause.kind == super::error::TIMEOUT_EXCEPTION) => {}
                Err(error) => return Err(ExitWaitError::Failed(error)),
            }
        }
        if let Err(error) = clock.delay(backoff.next_millis(), cancellation.clone()).await {
            return Err(match error {
                VendorError::Cancelled(reason) => ExitWaitError::Cancelled(reason),
                other => ExitWaitError::Failed(SshFailure::new(IO_EXCEPTION, other.failure().to_string())),
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

/// D93: a failed execute, and whether it is known to have started nothing on the worker (no exec request reached a
/// shell: the channel was refused or never opened, or the exec was refused).
type ExecFailure = (VendorError, bool);
async fn execute(
    connection: Arc<SshConnection>,
    request: CommandRequest,
    context: VendorContext,
    capacity: usize,
    config: PerformanceDefaultConfig,
    admission: Option<Arc<crate::cmd::device_commands::CommandAdmission>>,
    supervisor: Option<Arc<Supervisor>>,
) -> Result<CommandSession, ExecFailure> {
    if let Some(reason) = context.cancellation.reason() {
        return Err((VendorError::Cancelled(reason), true));
    }
    let command = exec_string(&request);
    let lease = match supervisor {
        Some(supervisor) => supervisor
            .prepare(&connection, &command)
            .await
            .map_err(|error| (error, true))?,
        None => None,
    };
    let runtime = connection.runtime.clone();
    let channel = connection
        .start_session()
        .await
        .map_err(|error| (error.into_vendor(), true))?;
    // D93 (round 8): the last look before the command is sent. The channel can take long to be accepted, and the caller
    // (or its device's command gate, which the context carries) may have stopped meanwhile: then the channel is closed
    // and no exec is sent, which starts nothing.
    if let Some(reason) = context.cancellation.reason() {
        let _ = channel.close().await;
        return Err((VendorError::Cancelled(reason), true));
    }
    if let Some(lease) = &lease {
        channel.set_worker_lease(lease.clone());
    }
    let channel_logger = config.logger_for_class(Some("PatchedExecChannel"));
    config.logging.log(
        &channel_logger,
        Level::DEBUG,
        || format!("Will request to exec `{command}`"),
        None,
    );
    // D93 (round 9): the writer looks at the context (the caller's, with its device's gate) once more as it writes the exec.
    if let Err(error) = channel
        .exec_gated(command, Some(context.cancellation.clone()), admission)
        .await
    {
        if let Some(reason) = channel.exec_withheld() {
            return Err((VendorError::Cancelled(reason), true));
        }
        let refused = channel.exec_refused();
        return Err((error.into_vendor(), refused));
    }
    let base = BaseCommand::new(capacity, runtime.clone());
    let last_output = OutputActivity::new(context.clock.now_millis());
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
            produce_lines(reader, output, last, idle_timeout, reader_context, async move {
                readable.read_ended().await;
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
    let interrupt = channel.clone();
    let control = session_control(channel, base.scope.clone(), closing);
    Ok(CommandSession::new(base, control)
        .with_interrupt(move || interrupt.signal(Sig::INT).map_err(SshFailure::into_vendor)))
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
    let mut termination = None;
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
        let waited = waited.lock().unwrap_or_else(|e| e.into_inner()).take();
        match outcome {
            Some(()) => match waited {
                // D7: a null result without expiry (another signal, no status) sends no stale TERM; the
                // following close() is a no-op on the closed channel.
                Some(Ok(code)) => {
                    use crate::cmd::base_command::CommandTermination;
                    if let Some(lease) = channel.worker_lease() {
                        lease.channel_ended();
                        let proof = tokio::time::timeout(std::time::Duration::from_secs(8), lease.proof())
                            .await
                            .map_err(|_| {
                                VendorError::Other(failure(FailureKind::Io, "Worker exit proof timed out"))
                            })??;
                        if let Proof::Done(status) = proof {
                            termination = Some(CommandTermination::Status(status));
                            return Ok(Some(status));
                        }
                        return Err(VendorError::Other(failure(
                            FailureKind::Io,
                            "Worker command was not started",
                        )));
                    }
                    let detail = if let Some(status) = channel.exit_status() {
                        CommandTermination::Status(status)
                    } else if let Some(signal) = channel.exit_signal() {
                        CommandTermination::Signal(signal)
                    } else {
                        CommandTermination::NoStatus
                    };
                    termination = Some(detail);
                    Ok(code)
                }
                Some(Err(error)) => Err(error.into_vendor()),
                None => Err(VendorError::Other(failure(
                    FailureKind::IllegalState,
                    "no exit wait result",
                ))),
            },
            None => {
                // The expired wait left the channel alone. TERM, then the bounded close, which gives a process that TERM
                // does not end KILL, and fails if the peer never answers (T15h, D30).
                termination = Some(crate::cmd::base_command::CommandTermination::BatchTimeout);
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
    exit.complete_observed(result, termination);
    Ok(())
}

impl CommandExecutor for SshCommandExecutor {
    fn host(&self) -> &CommandHost {
        &self.ssh_host.host
    }
    fn component(&self) -> &Component {
        &self.logger
    }
    fn connected(&self) -> bool {
        self.connection.is_connected() && self.supervisor.as_ref().is_none_or(|supervisor| supervisor.connected())
    }
    fn set_provider_signal_deadline(&self, deadline: Arc<marathon_core::signal_finish::SignalDeadline>) {
        let _ = self.connection.shared.provider_signal.set(deadline);
    }
    fn worker_supervised(&self) -> VendorFuture<'_, bool> {
        Box::pin(async move {
            let Some(supervisor) = self.supervisor.clone() else {
                return Ok(false);
            };
            let connection = self.connection.clone();
            let (send, reply) = tokio::sync::oneshot::channel();
            self.runtime.spawn(Box::pin(async move {
                let _ = send.send(supervisor.available(&connection).await);
                Ok(())
            }));
            reply.await.map_err(|error| {
                VendorError::Other(failure(FailureKind::Io, format!("Worker supervisor probe: {error}")))
            })?
        })
    }
    fn shared_ssh_is_saturated(&self) -> bool {
        self.connection.shared.saturated.load(Ordering::SeqCst)
    }
    fn hold_shared_ssh_slot(
        &self,
        slot: tokio::sync::OwnedSemaphorePermit,
    ) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.connection.shared.hold_shared_slot(slot);
        None
    }
    fn connection_lost(&self) -> std::pin::Pin<Box<dyn Future<Output = Failure> + Send>> {
        let shared = self.connection.shared.clone();
        Box::pin(async move { shared.wait_lost().await.to_failure() })
    }
    fn log_error(&self, message: String) {
        self.logging.log(&self.logger, Level::ERROR, || message, None);
    }
    fn log_error_with(&self, message: String, cause: &Failure) {
        if self.logging.enabled(&self.logger, Level::ERROR) {
            (self.logging.sink)(&self.logger, Level::ERROR, message, Some(cause.clone()));
        }
    }
    fn execute(&self, request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandSession> {
        let connection = self.connection.clone();
        let capacity = self.channel_capacity;
        let config = self.config.clone();
        let runtime = self.runtime.clone();
        let supervisor = self.supervisor.clone();
        Box::pin(async move {
            // russh needs Tokio: the whole exchange runs on the vendor runtime, which owns the session until
            // the caller takes it (an abandoned execute closes it there).
            let (tx, rx) = oneshot::channel();
            let (accepted, acceptance) = oneshot::channel::<()>();
            // D93 (round 10): read in the caller's task, where its device set it.
            let admission = crate::cmd::device_commands::admission();
            runtime.spawn(Box::pin(async move {
                match execute(connection, request, context, capacity, config, admission, supervisor).await {
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
                .map_err(|e| VendorError::Other(failure(FailureKind::IllegalState, e.to_string())))?;
            // D93: in the caller's task, where an xcodebuild session's opener observes it.
            crate::cmd::exec_outcome::record(matches!(pending, Err((_, true))));
            let pending = pending.map_err(|(error, _)| error)?;
            let session = pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .ok_or_else(|| VendorError::Other(failure(FailureKind::IllegalState, "session already delivered")))?;
            let _ = accepted.send(());
            Ok(session)
        })
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if let Err(error) = self.connection.close().await
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
    fn abandon(&self) {
        self.connection.abandon();
    }
}
