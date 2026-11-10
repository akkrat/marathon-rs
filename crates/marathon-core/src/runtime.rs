//! Port of core/src/main/kotlin/com/malinskiy/marathon/actor/Actor.kt
//! and execution/device/DeviceActor.kt (Job and CompletableDeferred use sites).
//!
//! WP 3.1 implements these contracts. There is no executor hidden in a default
//! method. See lifecycle.md for cancellation, ordering and replay requirements.

use crate::error::{Cancellation, Failure, OperationResult, VendorResult};
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
    pub core: Rc<dyn CoreRuntime>,
    pub clock: Rc<dyn SchedulerClock>,
}
pub struct ActorContext<M> {
    /// Construction uses the owner; receive uses the executing receiver child.
    pub task: TaskContext,
    /// actor.job() is the owner for actor-scoped launches and bookkeeping.
    pub actor: Rc<dyn Actor<M>>,
}
pub type JobRef = Rc<dyn Job>;
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
pub trait Job {
    /// Explicit cancellation observation, before body/child completion.
    fn cancellation_signal(&self) -> Arc<dyn CancellationSignal>;
    /// A cancellable join needs its caller; join() alone is protected.
    fn join_in(&self, caller: JobRef) -> LocalFuture<'_, Result<(), Cancellation>> {
        Box::pin(async move {
            let signal = caller.cancellation_signal();
            match crate::executor::shared::race(signal.cancelled(), self.join()).await {
                futures_util::future::Either::Left(reason) => Err(reason),
                futures_util::future::Either::Right(joined) => joined,
            }
        })
    }
    fn state(&self) -> JobState;
    fn cancel(&self, reason: Cancellation);
    fn complete(&self) -> bool;
    fn join(&self) -> LocalFuture<'_, Result<(), Cancellation>>;
    fn children_snapshot(&self) -> Vec<JobRef>;
    /// Kotlin job.children traversal, joining each observed child. Not a fixed
    /// point over future children. Preserve the pinned single-node/list traversal
    /// documented in lifecycle.md, including successor reads after each join.
    fn join_observed_children(&self) -> LocalFuture<'_, Result<(), Cancellation>>;
    /// Cancellable traversal for an explicit caller, as join_in is for join (WP 3.6).
    fn join_observed_children_in(&self, caller: JobRef) -> LocalFuture<'_, Result<(), Cancellation>> {
        Box::pin(async move {
            let signal = caller.cancellation_signal();
            match crate::executor::shared::race(signal.cancelled(), self.join_observed_children()).await {
                futures_util::future::Either::Left(reason) => Err(reason),
                futures_util::future::Either::Right(joined) => joined,
            }
        })
    }
    fn invoke_on_completion(&self, callback: Box<dyn FnOnce(Completion)>) -> Box<dyn CompletionRegistration>;
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
pub trait CoreRuntime {
    fn job(&self, kind: JobKind, parent: Option<JobRef>) -> JobRef;
    fn spawn(&self, parent: JobRef, body: TaskBody<'static>) -> JobRef;
    /// Cleanup must finish even under parent cancellation. Retain the returned
    /// job until joined; dropping the waiting future must not abort cleanup.
    fn spawn_cleanup(&self, body: TaskBody<'static>) -> JobRef;
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

/// Object-safe timeout runner. Typed values travel in captured cells/Deferreds,
/// as with CoreRuntime's erased tasks. WP 3.1 supplies concrete constructors for
/// Deferred<T>, EventChannel<T> and actors; no unwired generic factory traits.
pub trait TimeoutScope {
    /// The body receives the scope's child job. Join its cleanup before returning.
    /// Own expiry returns Err(OperationError::Cancelled(Cancellation::Timeout {
    /// scope, .. })); inner/external cancellation and failures propagate unchanged.
    fn with_timeout<'a>(
        &'a self,
        parent: TaskContext,
        millis: i64,
        body: TaskBody<'a>,
    ) -> LocalFuture<'a, OperationResult<()>>;
    /// Only this invocation's expiry becomes None; inner/external cancellation
    /// and ordinary failure propagate unchanged. Signed Java millis are retained.
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: TaskContext,
        millis: i64,
        body: TaskBody<'a>,
    ) -> LocalFuture<'a, OperationResult<Option<()>>>;
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
