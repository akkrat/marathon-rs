//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/CommandExecutor.kt

use marathon_logging::Component;

use marathon_error::{AppleKind, FailureKind};
mod artifact_stage;
pub mod base_command;
pub mod channel;
pub mod command_session_output;
pub mod device_commands;
pub mod exec_outcome;
pub mod line_buffer;
pub mod local;
pub mod remote;
pub mod retained_tasks;
pub mod shared_transfers;
pub(crate) mod stall_record;
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod transfer_receipt;
pub mod write_file;

use crate::extensions::Durations;
pub use base_command::{BaseCommand, CommandSession, ExitCode, SessionControl, SessionScope};
pub use command_session_output::consume_output;
pub use device_commands::{DeviceCommands, SealCause, ShutdownWindow, Termination};
use jcompat::jtime::Duration;
use marathon_core::{
    error::{Cancellation, Failure, VendorError, VendorResult},
    runtime::{CancellationSignal, VendorFuture},
    vendor::VendorContext,
};
use remote::ssh::sshj::error::{CONNECTION_EXCEPTION, TRANSPORT_EXCEPTION, is_instance};
use std::{collections::BTreeMap, future::Future, path::Path, pin::Pin, sync::Arc};
pub use transfer_receipt::{DevicePulls, TransferCleanup, TransferReceipt};

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
    fn worker_supervised(&self) -> VendorFuture<'_, bool> {
        Box::pin(async { Ok(false) })
    }
    #[doc(hidden)]
    fn prepare_worker_rsync<'a>(
        &'a self,
        _program: &'a str,
    ) -> VendorFuture<'a, Option<remote::ssh::sshj::WorkerRsync>> {
        Box::pin(async { Ok(None) })
    }
    fn log_error(&self, message: String);
    /// `logger.error(e) { message }`; executors whose sink keeps the throwable override this.
    fn log_error_with(&self, message: String, cause: &Failure) {
        let _ = cause;
        self.log_error(message);
    }
    fn execute(&self, request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandSession>;
    fn close(&self) -> VendorFuture<'_, ()>;
    /// The provider also owns commands that run before a device admits work on this connection.
    fn set_provider_signal_deadline(&self, _deadline: Arc<marathon_core::signal_finish::SignalDeadline>) {}
    /// Force the transport closed when its graceful close runs out of time.
    fn abandon(&self) {}
    /// D90: a capacity refusal followed a D30 abandonment on this transport.
    fn shared_ssh_is_saturated(&self) -> bool {
        false
    }
    /// D90: real SSH holds the slot until transport loss or closure is observed.
    fn hold_shared_ssh_slot(
        &self,
        slot: tokio::sync::OwnedSemaphorePermit,
    ) -> Option<tokio::sync::OwnedSemaphorePermit> {
        Some(slot)
    }
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
    #[cfg(feature = "test-support")]
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
    /// Receive a whole tree under its report name, preserving any existing entry.
    fn receive_bundle<'a>(&'a self, src: &'a str, dst: &'a Path, stop: TransferStop) -> VendorFuture<'a, bool> {
        self.receive_with(src, dst, stop)
    }
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
    pub(crate) worker: Option<Arc<dyn CommandExecutor>>,
    cancellation: Arc<dyn CancellationSignal>,
    lost: Option<Pin<Box<dyn Future<Output = Failure> + Send>>>,
    /// What `lost` gave once it was ready, since a finished future may not be polled again.
    lost_with: Option<Failure>,
    /// D91: the device's hold on this transfer. Whoever holds this stop owns the transfer, and the receipt ends when it
    /// lets go, with `unconfirmed` if its owner could not confirm the cleanup.
    receipt: Option<Arc<TransferReceipt>>,
    unconfirmed: Option<Failure>,
    /// D93 (round 8): a device's admission for the transfer: looked at as the transfer starts, never while it runs (a
    /// disposal closes a device's pulls to new ones, and leaves one under way to finish, as Kotlin's disposal does).
    admission: Option<Arc<dyn CancellationSignal>>,
}
impl Drop for TransferStop {
    fn drop(&mut self) {
        if let Some(receipt) = self.receipt.take() {
            let unconfirmed = self.unconfirmed.take().or_else(|| {
                std::thread::panicking().then(|| failure(FailureKind::IllegalState, "The transfer's owner panicked"))
            });
            receipt.end(unconfirmed.map_or(TransferCleanup::Confirmed, TransferCleanup::Unconfirmed));
        }
    }
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
            worker: None,
            cancellation,
            lost: None,
            lost_with: None,
            receipt: None,
            unconfirmed: None,
            admission: None,
        }
    }
    /// Use the device's connection to own any remote rsync sender.
    pub fn on_worker(mut self, worker: Arc<dyn CommandExecutor>) -> Self {
        self.worker = Some(worker);
        self
    }
    /// D93 (round 8): also refused as it starts once `admission` is cancelled; a transfer under way is not stopped by it.
    pub fn admitted_by(mut self, admission: Arc<dyn CancellationSignal>) -> Self {
        self.admission = Some(admission);
        self
    }
    /// D93 (round 8): what refuses the transfer as it starts: its admission, then a stop.
    pub(crate) fn start_check(&self) -> impl Fn() -> Option<Cancellation> + Send + Sync + 'static {
        let admission = self.admission.clone();
        let stopped = self.cancellation_check();
        move || admission.as_ref().and_then(|a| a.reason()).or_else(&stopped)
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
    /// D91: also stops when `receipt` is stopped, and ends `receipt` once whoever holds this stop lets go of it.
    pub fn owned_by(mut self, receipt: Arc<TransferReceipt>) -> Self {
        if let Some(previous) = self.receipt.replace(receipt) {
            previous.end(TransferCleanup::Confirmed);
        }
        self
    }
    /// D91: the cancellation that has come, from the caller or the device, without waiting; checked again as the process
    /// starts, where `now` cannot be (it polls the loss).
    pub(crate) fn cancellation_check(&self) -> impl Fn() -> Option<Cancellation> + Send + Sync + 'static {
        let cancellation = self.cancellation.clone();
        let receipt = self.receipt.clone();
        move || {
            cancellation
                .reason()
                .or_else(|| receipt.as_ref().and_then(|r| r.stop_reason()))
        }
    }
    /// D91: the device's hold on this transfer, through which it can ask for the cleanup after a stop to end sooner than
    /// the bridge's own bound.
    pub(crate) fn receipt(&self) -> Option<Arc<TransferReceipt>> {
        self.receipt.clone()
    }
    /// D91: the owner could not confirm that the transfer's processes ended; the receipt says so when it ends.
    pub(crate) fn unconfirmed(&mut self, why: Failure) {
        self.unconfirmed.get_or_insert(why);
    }
    /// The stop that has already come, if any, without waiting. A cancellation comes first.
    pub fn now(&mut self) -> Option<TransferStopped> {
        if let Some(reason) = self.start_check()() {
            return Some(TransferStopped::Cancelled(reason));
        }
        self.connection_stop()
    }
    pub(crate) fn connection_stop(&mut self) -> Option<TransferStopped> {
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
        let receipt = &self.receipt;
        let cancelled = async {
            match &receipt {
                Some(stop) => tokio::select! {
                    biased;
                    reason = cancellation.cancelled() => reason,
                    reason = stop.stopped() => reason,
                },
                None => cancellation.cancelled().await,
            }
        };
        match &mut self.lost {
            Some(lost) => tokio::select! {
                biased;
                reason = cancelled => TransferStopped::Cancelled(reason),
                failure = lost.as_mut() => {
                    self.lost = None;
                    self.lost_with = Some(failure.clone());
                    TransferStopped::Lost(failure)
                }
            },
            None => TransferStopped::Cancelled(cancelled.await),
        }
    }
}
/// D54: the stop for a send to the device that `executor` is the connection of: the call's cancellation, or the loss of that
/// connection, which fails the send as a connection failure naming the worker. The loss is the one that the provider sees as
/// a disconnect (D36), such as keep-alive's; closing the connection ourselves is not one.
pub fn send_stop(executor: &dyn CommandExecutor, context: &VendorContext) -> TransferStop {
    TransferStop::new(context.cancellation.clone()).or_lost(connection_loss(executor))
}
/// D54: a receive's owner can outlive its waiter. Connection loss and the device's transfer receipt stop the owner.
pub fn receive_stop(executor: &dyn CommandExecutor) -> TransferStop {
    TransferStop::never().or_lost(connection_loss(executor))
}
/// D93 (round 8): `receive_stop` for a device whose pulls `gate` can close (its seal by a disposal, its quarantine): the
/// receive is not started once it has, and is stopped if it comes while the receive runs.
pub fn receive_stop_gated(executor: &dyn CommandExecutor, gate: Arc<dyn CancellationSignal>) -> TransferStop {
    TransferStop::never()
        .or_lost(connection_loss(executor))
        .admitted_by(gate)
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
