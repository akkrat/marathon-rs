//! Port of core/src/main/kotlin/com/malinskiy/marathon/actor/Actor.kt
//! and execution/device/DeviceActor.kt (Job and CompletableDeferred use sites).
//!
//! WP 3.1 implements these contracts. There is no executor hidden in a default
//! method. See lifecycle.md for cancellation, ordering and replay requirements.

use crate::error::{Cancellation, Failure, OperationResult, VendorResult};
use crate::executor::CoreExecutor;
use crate::time::SchedulerClock;
use std::{future::Future, pin::Pin, rc::Rc, sync::Arc};

pub type LocalFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;
pub type VendorFuture<'a, T> = Pin<Box<dyn Future<Output = VendorResult<T>> + Send + 'a>>;
pub type ServiceFuture<'a, T> = Pin<Box<dyn Future<Output = OperationResult<T>> + Send + 'a>>;
pub type TaskBody<'a> = Box<dyn FnOnce(TaskContext) -> LocalFuture<'a, OperationResult<()>> + 'a>;
pub type VendorTaskBody = Box<dyn FnOnce(Arc<dyn CancellationSignal>) -> VendorFuture<'static, ()> + Send>;
pub type VendorTimeoutBody<'a> = Box<dyn FnOnce(Arc<dyn CancellationSignal>) -> VendorFuture<'a, ()> + Send + 'a>;

/// Explicit coroutine context; delay observes this job, including timeout children.
#[derive(Clone)]
pub struct TaskContext {
    pub job: JobRef,
    pub core: Rc<CoreExecutor>,
    pub clock: Rc<dyn SchedulerClock>,
}
pub struct ActorContext<M> {
    /// Construction uses the owner; receive uses the executing receiver child.
    pub task: TaskContext,
    /// actor.job() is the owner for actor-scoped launches and bookkeeping.
    pub actor: Rc<dyn Actor<M>>,
}
/// Core-thread job handle; construction is private to the executor.
/// Each executor constructs its jobs on its own dedicated thread. The Send
/// factory/result boundary prevents a handle from reaching another executor.
///
/// A job cannot be captured by another run's factory:
/// ```compile_fail,E0277
/// use marathon_core::{executor::CoreExecutor, runtime::JobKind};
/// let _ = CoreExecutor::run(|core| {
///     let job = core.job(JobKind::Ordinary, None);
///     let _ = CoreExecutor::run(move |other| {
///         other.job(JobKind::Ordinary, Some(job));
///         Box::pin(async { Ok(()) })
///     });
///     Box::pin(async { Ok(()) })
/// });
/// ```
/// Nor can it escape a run as its result:
/// ```compile_fail,E0277
/// use marathon_core::{executor::CoreExecutor, runtime::JobKind};
/// let _ = CoreExecutor::run(|core| {
///     Box::pin(async move { Ok(core.job(JobKind::Ordinary, None)) })
/// });
/// ```
pub type JobRef = Rc<crate::executor::RuntimeJob>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Ordinary,
    Supervisor,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    Active,
    Completing,
    Cancelling(Cancellation),
    Completed(Completion),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Completion {
    Success,
    Cancelled(Cancellation),
    Failed(Failure),
}
pub trait CompletionRegistration {
    fn dispose(&self);
}
/// Thread-safe cancellation observation for a vendor task. Requesting cancellation
/// is owned by its Job; this handle does not grant the vendor parent ownership.
pub trait CancellationSignal: Send + Sync {
    fn reason(&self) -> Option<Cancellation>;
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = Cancellation> + Send + '_>>;
}
/// Parentless first-writer-wins cell, shared across core and vendor runtimes.
/// Await cancellation is separate from stored failure. Completion never runs
/// core callbacks inline on the completing vendor thread.
pub trait Deferred<T>: Send + Sync {
    fn complete(&self, value: VendorResult<T>) -> bool;
    fn cancel(&self, reason: Cancellation) -> bool;
    fn completed(&self) -> Option<VendorResult<Arc<T>>>;
    fn wait(&self) -> VendorFuture<'_, Arc<T>>;
}
pub struct SendError<M> {
    pub message: M,
    pub cause: Option<Failure>,
}
pub trait Actor<M> {
    fn job(&self) -> JobRef;
    fn is_closed_for_send(&self) -> bool;
    fn try_send(&self, message: M) -> Result<(), SendError<M>>;
    fn send(&self, message: M) -> LocalFuture<'_, Result<(), SendError<M>>>;
    /// Cancel immediately and discard queued messages, including on repeat close.
    fn close(&self);
}
pub trait ActorHandler<M> {
    /// Called once at construction before dispatch; install completion callbacks
    /// here even if no message is received (already-completed jobs notify now).
    fn started(&mut self, _context: ActorContext<M>) {}
    fn receive(&mut self, message: M, context: ActorContext<M>) -> LocalFuture<'_, OperationResult<()>>;
}
/// Provider channel: shared consuming handle, never broadcast. Graceful close
/// drains the buffer; cancellation discards it. Receiver cancellation is separate.
pub trait EventChannel<T>: Send + Sync {
    fn try_send(&self, message: T) -> Result<(), SendError<T>>;
    fn receive(&self) -> VendorFuture<'_, Option<T>>;
    fn close(&self, cause: Option<Failure>);
    fn cancel(&self, reason: Cancellation);
    fn state(&self) -> MailboxState;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MailboxState {
    pub queued: usize,
    pub closed_for_send: bool,
    pub discarded: bool,
}
pub trait MailboxProbe {
    fn state(&self) -> MailboxState;
}
/// Erased worker tasks keep the runtime object-safe. Typed results travel in
/// Deferred<T>; a Send task must be started here, not polled on the core thread.
pub trait VendorRuntime: Send + Sync {
    /// The erased spawn future cannot receive its own cancellation context.
    fn spawn_cancellable(&self, body: VendorTaskBody) -> Arc<dyn VendorTask>;
    fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn VendorTask>;
    /// Blocking task completion remains joinable even if its awaiter cancels.
    fn spawn_blocking(&self, body: Box<dyn FnOnce() -> VendorResult<()> + Send>) -> Arc<dyn VendorTask>;
}
pub trait VendorTask: Send + Sync {
    fn cancel(&self, reason: Cancellation);
    fn join(&self) -> VendorFuture<'_, ()>;
}

/// Send scheduler-time seam, sharing ReplayDriver's timer timeline with core.
/// Own expiry is VendorError::Cancelled(Cancellation::Timeout { scope, .. }).
/// Inner timeouts propagate unchanged: with_timeout_or_null catches only its
/// own scope's timeout, never an inner timeout or external cancellation.
pub trait VendorClock: Send + Sync {
    fn delay(&self, millis: i64, cancellation: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()>;
    /// This clock's time, for measuring a wait. A clock with no time of its own reads 0, so everything on it takes no time.
    fn now_millis(&self) -> i64 {
        0
    }
    /// Join cancelled cleanup, then return own expiry as
    /// Err(VendorError::Cancelled(Cancellation::Timeout { scope, .. })).
    /// Inner timeouts and external cancellation propagate with their scope intact.
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, ()>;
    /// Only own expiry becomes Ok(None); inner timeouts, external cancellation
    /// and ordinary failure propagate unchanged after structured cleanup.
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, Option<()>>;
}
