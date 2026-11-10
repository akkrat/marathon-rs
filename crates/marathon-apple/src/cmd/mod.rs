//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/CommandExecutor.kt

use marathon_logging::Component;

use marathon_error::{AppleKind, FailureKind};
pub mod base_command;
pub mod channel;
pub mod command_session_output;
pub mod line_buffer;
pub mod local;
pub mod remote;
pub mod retained_tasks;
pub mod shared_transfers;
pub(crate) mod stall_record;
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod write_file;

use crate::extensions::Durations;
pub use base_command::{BaseCommand, CommandSession, ExitCode, SessionControl, SessionScope};
pub use command_session_output::consume_output;
use jcompat::jtime::Duration;
use marathon_core::{
    error::{Cancellation, Failure, VendorError, VendorResult},
    runtime::{CancellationSignal, VendorFuture},
    vendor::VendorContext,
};
use remote::ssh::sshj::error::{CONNECTION_EXCEPTION, TRANSPORT_EXCEPTION, is_instance};
use std::{collections::BTreeMap, future::Future, path::Path, pin::Pin, sync::Arc};

pub fn failure(kind: FailureKind, message: impl AsRef<str>) -> Failure {
    Failure::from_kind(kind, Some(message.as_ref().into()))
}
pub fn cancelled(message: &str) -> Cancellation {
    Cancellation::Requested(failure(FailureKind::Cancelled, message))
}
pub(crate) fn io_error(error: std::io::Error) -> VendorError {
    VendorError::Other(failure(FailureKind::Io, error.to_string()))
}
pub(crate) fn millis(duration: Duration) -> VendorResult<i64> {
    duration
        .to_millis()
        .ok_or_else(|| VendorError::Other(failure(FailureKind::Arithmetic, "long overflow")))
}

/// Port of cmd/CommandHost.kt; charset remains an API slot (UTF-8, approved P1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandHost {
    pub id: String,
    pub charset: String,
}

/// Port of cmd/CommandResult.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandResult {
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
    pub exit_code: Option<i32>,
}
impl CommandResult {
    pub fn successful(&self) -> bool {
        self.exit_code == Some(0)
    }
    pub fn combined_stdout(&self) -> String {
        self.stdout.join("\n")
    }
    pub fn combined_stderr(&self) -> String {
        self.stderr.join("\n")
    }
    pub fn successful_or_null(self) -> Option<Self> {
        self.successful().then_some(self)
    }
}

/// Port of cmd/CommandException.kt.
pub struct CommandException;
impl CommandException {
    pub fn for_result(command: &[String], result: &CommandResult) -> Failure {
        // trimMargin() removes no indentation here: the Kotlin string has no '|'.
        failure(
            FailureKind::Apple(AppleKind::Command),
            format!(
                "command {} failed with exit code {}:\n                   stdout: {}\n                   stderr: {}",
                command.join(" "),
                result.exit_code.map_or_else(|| "null".into(), |c| c.to_string()),
                result.combined_stdout(),
                result.combined_stderr()
            ),
        )
    }
}

#[derive(Clone, Debug)]
pub struct CommandRequest {
    pub command: Vec<String>,
    pub timeout: Duration,
    pub idle_timeout: Duration,
    pub env: BTreeMap<String, String>,
    pub workdir: Option<String>,
    pub charset: String,
}
impl CommandRequest {
    pub fn new(command: impl IntoIterator<Item = impl Into<String>>, timeout: Duration) -> Self {
        Self {
            command: command.into_iter().map(Into::into).collect(),
            timeout,
            idle_timeout: Durations::INFINITE,
            env: BTreeMap::new(),
            workdir: None,
            charset: "UTF-8".into(),
        }
    }
}

// Static dispatch within Apple; concrete sessions also suit the future SSH backend.
pub trait CommandExecutor: Send + Sync {
    fn host(&self) -> &CommandHost;
    fn component(&self) -> &Component;
    fn connected(&self) -> bool;
    fn log_error(&self, message: String);
    /// `logger.error(e) { message }`; executors whose sink keeps the throwable override this.
    fn log_error_with(&self, message: String, cause: &Failure) {
        let _ = cause;
        self.log_error(message);
    }
    fn execute(&self, request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandSession>;
    fn close(&self) -> VendorFuture<'_, ()>;
    /// D54: ready, with why, once this connection is lost, and never if `close` ends it or nothing can lose it, as for a
    /// local executor.
    fn connection_lost(&self) -> Pin<Box<dyn Future<Output = Failure> + Send>> {
        Box::pin(std::future::pending())
    }
    fn critical_execute(&self, request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandResult> {
        Box::pin(async move {
            let session = self.execute(request.clone(), context.clone()).await?;
            let result = session.await_result(&context).await?;
            if result.successful() {
                Ok(result)
            } else {
                Err(VendorError::Other(CommandException::for_result(
                    &request.command,
                    &result,
                )))
            }
        })
    }
    fn safe_execute(&self, request: CommandRequest, context: VendorContext) -> VendorFuture<'_, Option<CommandResult>> {
        Box::pin(async move {
            let result = async {
                let session = self.execute(request.clone(), context.clone()).await?;
                session.await_result(&context).await
            }
            .await;
            // Kotlin catches the two sshj classes and their subclasses (OpenFailException is a
            // ConnectionException), in this order.
            let class = |f: &Failure| f.kind.to_owned();
            match result {
                Ok(value) => Ok(Some(value)),
                Err(VendorError::Other(ref f)) if is_instance(&class(f), CONNECTION_EXCEPTION) => {
                    self.log_error_with(
                        format!(
                            "Unable to start a remote shell session [{}]",
                            request.command.join(", ")
                        ),
                        f,
                    );
                    Ok(None)
                }
                Err(VendorError::Other(ref f)) if is_instance(&class(f), TRANSPORT_EXCEPTION) => {
                    self.log_error_with(
                        format!("Error starting a remote shell session [{}]", request.command.join(", ")),
                        f,
                    );
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        })
    }
    fn safe_execute_nohup(
        &self,
        pidfile: &str,
        mut request: CommandRequest,
        context: VendorContext,
    ) -> VendorFuture<'_, Option<CommandResult>> {
        request.command = vec![
            "sh".into(),
            "-c".into(),
            format!(
                "nohup {} > /dev/null 2>&1 & echo $! > {pidfile}",
                request.command.join(" ")
            ),
        ];
        self.safe_execute(request, context)
    }
}

/// Port of cmd/FileBridge.kt.
pub trait FileBridge: Send + Sync {
    fn send<'a>(&'a self, src: &'a Path, dst: &'a str) -> VendorFuture<'a, bool>;
    fn receive<'a>(&'a self, src: &'a str, dst: &'a Path) -> VendorFuture<'a, bool>;
    /// D54: `send`, ended early by `stop`. A bridge that cannot stop a transfer, such as a local copy, ignores it, as
    /// Kotlin's every bridge does.
    fn send_with<'a>(&'a self, src: &'a Path, dst: &'a str, stop: TransferStop) -> VendorFuture<'a, bool> {
        let _ = stop;
        self.send(src, dst)
    }
    /// D54: `receive`, ended early by `stop`.
    fn receive_with<'a>(&'a self, src: &'a str, dst: &'a Path, stop: TransferStop) -> VendorFuture<'a, bool> {
        let _ = stop;
        self.receive(src, dst)
    }
}

/// D54: what ends a transfer before it ends by itself. Kotlin's transfer blocks in a suspend function, so neither its caller's
/// cancellation nor the loss of its device's connection ends it (A104).
pub struct TransferStop {
    cancellation: Arc<dyn CancellationSignal>,
    lost: Option<Pin<Box<dyn Future<Output = Failure> + Send>>>,
    /// What `lost` gave once it was ready, since a finished future may not be polled again.
    lost_with: Option<Failure>,
}
/// Why a transfer was stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferStopped {
    /// The call was cancelled; the transfer returns that cancellation.
    Cancelled(Cancellation),
    /// The device's own connection was lost; the transfer fails with this.
    Lost(Failure),
}
impl TransferStopped {
    /// What a send returns for it: the cancellation as one, a loss as an ordinary failure.
    pub fn into_error(self) -> VendorError {
        match self {
            Self::Cancelled(reason) => VendorError::Cancelled(reason),
            Self::Lost(failure) => VendorError::Other(failure),
        }
    }
}
impl TransferStop {
    /// Stops on the caller's cancellation only.
    pub fn new(cancellation: Arc<dyn CancellationSignal>) -> Self {
        Self {
            cancellation,
            lost: None,
            lost_with: None,
        }
    }
    /// Never stops: the transfer ends only by itself, or by D31's and D43's limits.
    pub fn never() -> Self {
        Self::new(Arc::new(SessionScope::default()))
    }
    /// Also stops once `lost` is ready, failing with what it gives.
    pub fn or_lost(mut self, lost: impl Future<Output = Failure> + Send + 'static) -> Self {
        self.lost = Some(Box::pin(lost));
        self
    }
    /// The stop that has already come, if any, without waiting. A cancellation comes first.
    pub fn now(&mut self) -> Option<TransferStopped> {
        if let Some(reason) = self.cancellation.reason() {
            return Some(TransferStopped::Cancelled(reason));
        }
        if self.lost_with.is_none()
            && let Some(lost) = &mut self.lost
            && let std::task::Poll::Ready(failure) = lost
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        {
            self.lost = None;
            self.lost_with = Some(failure);
        }
        self.lost_with.clone().map(TransferStopped::Lost)
    }
    /// Waits for a stop; never returns if none comes.
    pub async fn stopped(&mut self) -> TransferStopped {
        if let Some(stopped) = self.now() {
            return stopped;
        }
        let cancellation = self.cancellation.clone();
        match &mut self.lost {
            Some(lost) => tokio::select! {
                biased;
                reason = cancellation.cancelled() => TransferStopped::Cancelled(reason),
                failure = lost.as_mut() => {
                    self.lost = None;
                    self.lost_with = Some(failure.clone());
                    TransferStopped::Lost(failure)
                }
            },
            None => TransferStopped::Cancelled(cancellation.cancelled().await),
        }
    }
}
/// D54: the stop for a send to the device that `executor` is the connection of: the call's cancellation, or the loss of that
/// connection, which fails the send as a connection failure naming the worker. The loss is the one that the provider sees as
/// a disconnect (D36), such as keep-alive's; closing the connection ourselves is not one.
pub fn send_stop(executor: &dyn CommandExecutor, context: &VendorContext) -> TransferStop {
    TransferStop::new(context.cancellation.clone()).or_lost(connection_loss(executor))
}
/// D54: the stop for a receive from that device: only the loss of its connection. Every receive collects an artifact, and
/// its callers are cancelled on purpose while it runs, as the artifact queue is 300 s after a run, before a 60 s grace to
/// finish. Kotlin's receive finishes then, and so does this one.
pub fn receive_stop(executor: &dyn CommandExecutor) -> TransferStop {
    TransferStop::never().or_lost(connection_loss(executor))
}
fn connection_loss(executor: &dyn CommandExecutor) -> impl Future<Output = Failure> + Send + 'static {
    let lost = executor.connection_lost();
    let worker = executor.host().id.clone();
    async move {
        let mut lost_with = failure(
            remote::ssh::sshj::error::CONNECTION_EXCEPTION,
            format!("Lost the connection to {worker} during a transfer"),
        );
        lost_with.cause = Some(Box::new(lost.await));
        lost_with
    }
}

pub(crate) async fn blocking<T: Send + 'static>(
    runtime: &dyn marathon_core::runtime::VendorRuntime,
    body: impl FnOnce() -> VendorResult<T> + Send + 'static,
) -> VendorResult<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = runtime.spawn_blocking(Box::new(move || {
        let _ = tx.send(body());
        Ok(())
    }));
    task.join().await?;
    rx.await
        .map_err(|e| VendorError::Other(failure(FailureKind::IllegalState, e.to_string())))?
}
