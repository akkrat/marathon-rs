//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/BaseCommand.kt
//! and cmd/CommandSession.kt.

use super::{CommandResult, cancelled, channel::LineChannel, failure};
use marathon_core::{
    error::{Cancellation, VendorError, VendorResult},
    runtime::{CancellationSignal, VendorFuture, VendorRuntime, VendorTask},
    vendor::VendorContext,
};
use marathon_error::FailureKind;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

#[derive(Clone)]
pub struct ExitCode(watch::Sender<Option<VendorResult<Option<i32>>>>);
impl Default for ExitCode {
    fn default() -> Self {
        Self(watch::channel(None).0)
    }
}
impl ExitCode {
    pub fn complete(&self, result: VendorResult<Option<i32>>) -> bool {
        self.0.send_if_modified(|value| {
            if value.is_some() {
                false
            } else {
                *value = Some(result);
                true
            }
        })
    }
    pub fn is_completed(&self) -> bool {
        self.0.borrow().is_some()
    }
    pub async fn wait(&self) -> VendorResult<Option<i32>> {
        let mut rx = self.0.subscribe();
        loop {
            if let Some(result) = rx.borrow().clone() {
                return result;
            }
            rx.changed()
                .await
                .map_err(|e| VendorError::Other(failure(FailureKind::IllegalState, e.to_string())))?;
        }
    }
}

pub struct SessionScope {
    reason: watch::Sender<Option<Cancellation>>,
    tasks: Mutex<Vec<Arc<dyn VendorTask>>>,
}
impl Default for SessionScope {
    fn default() -> Self {
        Self {
            reason: watch::channel(None).0,
            tasks: Mutex::new(Vec::new()),
        }
    }
}
impl SessionScope {
    pub fn cancel(&self, reason: Cancellation) {
        self.reason.send_if_modified(|r| {
            if r.is_none() {
                *r = Some(reason);
                true
            } else {
                false
            }
        });
    }
    pub fn track(&self, task: Arc<dyn VendorTask>) {
        self.tasks.lock().unwrap_or_else(|e| e.into_inner()).push(task);
    }
    pub async fn close(&self) -> VendorResult<()> {
        self.cancel(cancelled("Session closed"));
        let tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for task in tasks {
            task.join().await?;
        }
        Ok(())
    }
}
impl CancellationSignal for SessionScope {
    fn reason(&self) -> Option<Cancellation> {
        self.reason.borrow().clone()
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Cancellation> + Send + '_>> {
        Box::pin(async move {
            let mut rx = self.reason.subscribe();
            loop {
                if let Some(reason) = rx.borrow().clone() {
                    return reason;
                }
                if rx.changed().await.is_err() {
                    return cancelled("Session closed");
                }
            }
        })
    }
}

pub struct BaseCommand {
    pub stdout: LineChannel,
    pub stderr: LineChannel,
    pub exit_code: ExitCode,
    pub scope: Arc<SessionScope>,
    runtime: Arc<dyn VendorRuntime>,
}
impl BaseCommand {
    pub fn new(capacity: usize, runtime: Arc<dyn VendorRuntime>) -> Self {
        Self {
            runtime,
            stdout: LineChannel::new(capacity),
            stderr: LineChannel::new(capacity),
            exit_code: ExitCode::default(),
            scope: Arc::new(SessionScope::default()),
        }
    }
    fn start_collection(
        &self,
        channel: &LineChannel,
        keep: bool,
        wrap_failure: bool,
    ) -> tokio::sync::oneshot::Receiver<VendorResult<Vec<String>>> {
        let channel = channel.clone();
        let scope = self.scope.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.scope.track(self.runtime.spawn(Box::pin(async move {
            let collect = async {
                let mut lines = Vec::new();
                loop {
                    match channel.receive().await {
                        Ok(Some(line)) => {
                            if keep {
                                lines.push(line);
                            }
                        }
                        Ok(None) => return Ok(lines),
                        Err(VendorError::Other(cause)) if wrap_failure => {
                            let mut detail = failure(FailureKind::Cancelled, "Channel closed");
                            detail.cause = Some(Box::new(cause));
                            return Err(VendorError::Cancelled(Cancellation::Requested(detail)));
                        }
                        Err(error) => return Err(error),
                    }
                }
            };
            let result = tokio::select! {
                biased;
                reason = scope.cancelled() => Err(VendorError::Cancelled(reason)),
                result = collect => result,
            };
            let _ = tx.send(result);
            Ok(())
        })));
        rx
    }
    async fn collected(rx: tokio::sync::oneshot::Receiver<VendorResult<Vec<String>>>) -> VendorResult<Vec<String>> {
        rx.await
            .map_err(|e| VendorError::Other(failure(FailureKind::IllegalState, e.to_string())))?
    }
    pub async fn await_result(&self, context: &VendorContext) -> VendorResult<CommandResult> {
        // The collectors belong to the detached session, not its awaiter. In
        // particular cancelling an await must not strand a full producer channel.
        let out = self.start_collection(&self.stdout, true, true);
        let err = self.start_collection(&self.stderr, true, true);
        tokio::select! {
            biased;
            reason = context.cancellation.cancelled() => Err(VendorError::Cancelled(reason)),
            reason = self.scope.cancelled() => Err(VendorError::Cancelled(reason)),
            result = async {
                let (stdout, stderr, exit_code) = tokio::try_join!(Self::collected(out), Self::collected(err), self.exit_code.wait())?;
                Ok(CommandResult { stdout, stderr, exit_code })
            } => result,
        }
    }
    pub async fn drain(&self, context: &VendorContext) -> VendorResult<()> {
        if let Some(reason) = context.cancellation.reason() {
            return Err(VendorError::Cancelled(reason));
        }
        // Kotlin async(job) attaches both collectors to the command job, outside
        // supervisorScope. drain returns without awaiting either Deferred (A26).
        drop(self.start_collection(&self.stdout, false, false));
        drop(self.start_collection(&self.stderr, false, false));
        Ok(())
    }
    pub async fn close(&self) -> VendorResult<()> {
        self.stdout.cancel(cancelled("Session closed"));
        self.stderr.cancel(cancelled("Session closed"));
        self.scope.close().await
    }
}

/// Explicit backend operations allow live local isAlive and captured SSH isAlive.
/// Async close may retain a backend task so dropping its awaiter cannot abort cleanup.
pub struct SessionControl {
    pub is_alive: Box<dyn Fn() -> bool + Send + Sync>,
    pub terminate: Box<dyn Fn() -> VendorResult<()> + Send + Sync>,
    pub kill: Box<dyn Fn() -> VendorResult<()> + Send + Sync>,
    pub close: Box<dyn Fn() -> VendorFuture<'static, ()> + Send + Sync>,
}
/// The retained close task and the attempt it belongs to.
#[derive(Default)]
struct CloseSlot {
    attempts: u64,
    task: Option<(u64, Arc<dyn VendorTask>)>,
}
pub struct CommandSession {
    pub base: Arc<BaseCommand>,
    control: Arc<SessionControl>,
    close_task: Arc<Mutex<CloseSlot>>,
    fixed_result: Option<CommandResult>,
}
impl CommandSession {
    pub fn new(base: BaseCommand, control: SessionControl) -> Self {
        Self {
            base: Arc::new(base),
            control: Arc::new(control),
            close_task: Arc::new(Mutex::new(CloseSlot::default())),
            fixed_result: None,
        }
    }
    /// Finished Kotlin test session: closed streams, repeatable await, inert controls.
    #[cfg(feature = "test-support")]
    pub fn finished(runtime: Arc<dyn VendorRuntime>, result: CommandResult) -> Self {
        let base = BaseCommand::new(1, runtime);
        base.stdout.close(None);
        base.stderr.close(None);
        base.exit_code.complete(Ok(result.exit_code));
        let mut session = Self::new(
            base,
            SessionControl {
                is_alive: Box::new(|| false),
                terminate: Box::new(|| Ok(())),
                kill: Box::new(|| Ok(())),
                close: Box::new(|| Box::pin(async { Ok(()) })),
            },
        );
        session.fixed_result = Some(result);
        session
    }
    pub fn is_alive(&self) -> bool {
        (self.control.is_alive)()
    }
    pub fn terminate(&self) -> VendorResult<()> {
        (self.control.terminate)()
    }
    pub fn kill(&self) -> VendorResult<()> {
        (self.control.kill)()
    }
    pub async fn await_result(&self, context: &VendorContext) -> VendorResult<CommandResult> {
        if let Some(result) = &self.fixed_result {
            return Ok(result.clone());
        }
        self.base.await_result(context).await
    }
    pub async fn drain(&self, context: &VendorContext) -> VendorResult<()> {
        self.base.drain(context).await
    }
    pub async fn close(&self) -> VendorResult<()> {
        let task = {
            let mut slot = self.close_task.lock().unwrap_or_else(|e| e.into_inner());
            match &slot.task {
                Some((_, task)) => task.clone(),
                None => {
                    slot.attempts += 1;
                    let attempt = slot.attempts;
                    let base = self.base.clone();
                    let control = self.control.clone();
                    let closing = self.close_task.clone();
                    let task = self.base.runtime.spawn(Box::pin(async move {
                        let result = async {
                            (control.close)().await?;
                            base.close().await
                        }
                        .await;
                        // SSH may time out during close (at most 30 s, D30); a later explicit close must retry
                        // instead of replaying that error. Only this attempt's own entry is released, after all of
                        // its work, so a retry never overlaps it (the SSH backend itself acts only on the first
                        // call, A14; the retry then cancels and joins the session scope, approved P5).
                        if result.is_err() {
                            let mut slot = closing.lock().unwrap_or_else(|e| e.into_inner());
                            if slot.task.as_ref().is_some_and(|(owner, _)| *owner == attempt) {
                                slot.task = None;
                            }
                        }
                        result
                    }));
                    slot.task = Some((attempt, task.clone()));
                    task
                }
            }
        };
        task.join().await
    }
}
