//! Port of core/src/main/kotlin/com/malinskiy/marathon/Marathon.kt
//! and actor/Actor.kt (coroutine execution and explicit Job context).

mod actor;
pub mod bridge;
mod clock;
pub mod completion_logging;
pub mod hooks;
mod job;
pub mod retry;
pub mod shared;
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod timeout;
mod tracking;
use crate::{error::*, runtime::*};
pub use actor::RuntimeActor;
pub use clock::{RuntimeClock, RuntimeTimeouts};
use futures_util::FutureExt;
pub use job::RuntimeJob;
use shared::*;
pub use shared::{SharedChannel, SharedDeferred, Signal};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::{Rc, Weak},
    sync::{Arc, Mutex},
};
use tracking::{Tracked, Tracker};

/// Constructed inside run's dedicated core thread, never moved between threads.
pub struct CoreExecutor {
    myself: Weak<Self>,
    clock: Rc<RuntimeClock>,
    jobs: RefCell<HashMap<usize, Weak<RuntimeJob>>>,
    /// Registry size after the last sweep; sweeping on every creation is quadratic.
    swept: std::cell::Cell<usize>,
    tracker: Arc<Tracker>,
    vendor: Arc<IoRuntime>,
    completions: RefCell<Vec<completion_logging::CompletionLog>>,
    log: Rc<crate::logging::LogQueue>,
    pub(crate) mailboxes: RefCell<Vec<(&'static str, Rc<dyn MailboxProbe>)>>,
    logging: Arc<Mutex<jcompat::logging::MarathonLogging>>,
}
impl CoreExecutor {
    /// The Send factory builds all Rc state on the dedicated LocalSet thread.
    /// This blocking entry point belongs to the CLI/test thread, never core.
    pub fn run<T: Send + 'static>(
        body: impl FnOnce(Rc<Self>) -> LocalFuture<'static, OperationResult<T>> + Send + 'static,
    ) -> OperationResult<T> {
        Self::run_mode(false, None, body)
    }
    /// `watchdog` bounds the caller's wait in wall time (test support, S5): a future
    /// that blocks inside poll stalls the core thread and its in-executor driver too.
    pub(crate) fn run_mode<T: Send + 'static>(
        virtual_time: bool,
        watchdog: Option<std::time::Duration>,
        body: impl FnOnce(Rc<Self>) -> LocalFuture<'static, OperationResult<T>> + Send + 'static,
    ) -> OperationResult<T> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(failure(
                "java.lang.IllegalStateException",
                "CoreExecutor::run must be called outside an executor",
            )
            .into());
        }
        let vendor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            // Vendor code needs the I/O driver as well as timers: the SSH
            // executor's sockets, and tokio::process pipes and SIGCHLD for the
            // local executor.
            .enable_io()
            .enable_time()
            .thread_name("marathon-vendor")
            .build()
            .map_err(|e| failure("java.io.IOException", &e.to_string()))?;
        let handle = vendor.handle().clone();
        let (finished, settled) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("marathon-core".into())
            .spawn(move || {
                let _finished = finished;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .map_err(|e| failure("java.io.IOException", &e.to_string()))?;
                let local = tokio::task::LocalSet::new();
                let tracker = Arc::new(Tracker::default());
                let clock = Rc::new(RuntimeClock::new(virtual_time));
                let core = Rc::new_cyclic(|myself| Self {
                    myself: myself.clone(),
                    clock,
                    jobs: RefCell::new(HashMap::new()),
                    swept: std::cell::Cell::new(0),
                    tracker: tracker.clone(),
                    vendor: Arc::new(IoRuntime { handle, tracker }),
                    completions: RefCell::new(Vec::new()),
                    log: Rc::default(),
                    mailboxes: RefCell::new(Vec::new()),
                    logging: Arc::new(Mutex::new(jcompat::logging::MarathonLogging::default())),
                });
                runtime.block_on(local.run_until(async move {
                    match std::panic::AssertUnwindSafe(async move { body(core).await })
                        .catch_unwind()
                        .await
                    {
                        Ok(result) => result,
                        Err(payload) => Err(panic_failure(payload).into()),
                    }
                }))
            })
            .map_err(|e| failure("java.io.IOException", &e.to_string()))?;
        if let Some(limit) = watchdog
            && let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = settled.recv_timeout(limit)
        {
            // The stuck core thread is left detached; nothing can preempt its poll.
            vendor.shutdown_background();
            return Err(replay_failure(&format!(
                "replay watchdog: the core thread did not finish within {} ms; a future may be blocking inside poll",
                limit.as_millis()
            ))
            .into());
        }
        let result = thread.join().map_err(panic_failure)?;
        drop(vendor);
        result
    }
    pub fn event_channel<T: Send + 'static>(&self, label: &'static str) -> Arc<SharedChannel<T>> {
        let channel = SharedChannel::new();
        self.mailboxes
            .borrow_mut()
            .push((label, Rc::new(ChannelProbe(channel.clone()))));
        channel
    }
    pub fn clock(&self) -> Rc<RuntimeClock> {
        self.clock.clone()
    }
    pub fn vendor_clock(&self) -> Arc<dyn VendorClock> {
        self.clock.vendor()
    }
    pub fn vendor(&self) -> Arc<dyn VendorRuntime> {
        self.vendor.clone()
    }
    pub fn timeouts(&self) -> Rc<RuntimeTimeouts> {
        Rc::new(RuntimeTimeouts::new(self.myself.clone()))
    }
    pub fn context(&self, job: JobRef) -> TaskContext {
        TaskContext {
            job,
            core: self.rc(),
            clock: self.clock.clone(),
        }
    }
    pub fn completion_logs(&self) -> Vec<completion_logging::CompletionLog> {
        self.completions.borrow().clone()
    }
    /// Share this same registry with VendorDependencies and the output task.
    /// Core enqueues typed records; filtering/formatting and locking run off core.
    pub fn logging(&self) -> Arc<Mutex<jcompat::logging::MarathonLogging>> {
        self.logging.clone()
    }
    /// Core actors' log records, rendered off core (lifecycle.md "Core log records").
    pub fn log_queue(&self) -> Rc<crate::logging::LogQueue> {
        self.log.clone()
    }
    pub fn take_completion_logs(&self) -> Vec<completion_logging::CompletionLog> {
        std::mem::take(&mut self.completions.borrow_mut())
    }
    fn rc(&self) -> Rc<Self> {
        match self.myself.upgrade() {
            Some(core) => core,
            None => unreachable!("runtime method requires a live receiver"),
        }
    }
    pub(crate) fn new_job(&self, kind: JobKind, parent: Option<JobRef>, task: bool) -> Rc<RuntimeJob> {
        let parent = parent.map(|parent| self.resolve(&parent));
        let job = RuntimeJob::new(kind, parent, task);
        let key = Rc::as_ptr(&job) as usize;
        let mut jobs = self.jobs.borrow_mut();
        if jobs.len() >= 2 * self.swept.get() + 64 {
            jobs.retain(|_, job| job.strong_count() > 0);
            self.swept.set(jobs.len());
        }
        // A dead entry at a reused address is simply replaced.
        jobs.insert(key, Rc::downgrade(&job));
        drop(jobs);
        job
    }
    fn resolve(&self, job: &JobRef) -> Rc<RuntimeJob> {
        let key = Rc::as_ptr(job) as *const () as usize;
        match self.jobs.borrow().get(&key).and_then(Weak::upgrade) {
            Some(job) => job,
            None => panic!("parent must belong to this CoreExecutor"),
        }
    }
    pub(crate) fn launch(&self, job: Rc<RuntimeJob>, body: TaskBody<'static>) {
        self.launch_retained(job, body, Box::new(|| {}));
    }
    pub(crate) fn launch_retained(&self, job: Rc<RuntimeJob>, body: TaskBody<'static>, release: Box<dyn FnOnce()>) {
        let core = self.rc();
        let handler_core = self.myself.clone();
        let logger = job.handler_logger.borrow().clone();
        *job.handler.borrow_mut() = Some(Box::new(move |failure| {
            if let Some(core) = handler_core.upgrade() {
                core.record_failure(logger, failure);
            }
        }));
        tokio::task::spawn_local(Tracked::new(
            async move {
                let context = core.context(job.clone());
                let result = if let Some(reason) = job.cancellation_signal().reason() {
                    Err(OperationError::Cancelled(reason))
                } else {
                    match std::panic::AssertUnwindSafe(async move { body(context).await })
                        .catch_unwind()
                        .await
                    {
                        Ok(result) => result,
                        Err(payload) => Err(panic_failure(payload).into()),
                    }
                };
                job.finish_body(result);
                job.join().await.ok();
                release();
            },
            self.tracker.clone(),
        ));
    }
    /// In an actor scope the handler logs under the actor's logger, in call order
    /// with the actor's own records; elsewhere it stays a loggerless CompletionLog.
    fn record_failure(&self, logger: Option<Rc<str>>, failure: Failure) {
        match logger {
            Some(logger) => self.log.log(
                &logger,
                crate::time::SchedulerClock::now_millis(&*self.clock),
                jcompat::logging::Level::Error,
                completion_logging::UNRECOVERABLE.into(),
                Some(failure),
            ),
            None => {
                let log = completion_logging::completion_log(Completion::Failed(failure));
                self.completions.borrow_mut().push(log);
            }
        }
    }
}
impl CoreRuntime for CoreExecutor {
    fn job(&self, kind: JobKind, parent: Option<JobRef>) -> JobRef {
        self.new_job(kind, parent, false)
    }
    fn spawn(&self, parent: JobRef, body: TaskBody<'static>) -> JobRef {
        let job = self.new_job(JobKind::Ordinary, Some(parent), true);
        self.launch(job.clone(), body);
        job
    }
    fn spawn_cleanup(&self, body: TaskBody<'static>) -> JobRef {
        let job = self.new_job(JobKind::Ordinary, None, true);
        self.launch(job.clone(), body);
        job
    }
}
fn panic_failure(payload: Box<dyn std::any::Any + Send>) -> Failure {
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("Rust task panicked");
    failure("java.lang.RuntimeException", message)
}
struct IoTask {
    signal: Arc<Signal>,
    result: Arc<SharedDeferred<()>>,
}
impl VendorTask for IoTask {
    fn cancel(&self, reason: Cancellation) {
        self.signal.cancel(reason);
    }
    fn join(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move { self.result.wait().await.map(|_| ()) })
    }
}
struct IoRuntime {
    handle: tokio::runtime::Handle,
    tracker: Arc<Tracker>,
}
impl VendorRuntime for IoRuntime {
    fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn VendorTask> {
        self.spawn_cancellable(Box::new(move |_| body))
    }
    fn spawn_cancellable(&self, body: VendorTaskBody) -> Arc<dyn VendorTask> {
        let task = Arc::new(IoTask {
            signal: Signal::new(),
            result: SharedDeferred::new(),
        });
        let output = task.clone();
        self.handle.spawn(Tracked::new(
            async move {
                // Erased VendorFuture has no signal argument. Its completion remains
                // joinable; cancellation-aware operations use an explicit signal.
                let result = match std::panic::AssertUnwindSafe(async { body(output.signal.clone()).await })
                    .catch_unwind()
                    .await
                {
                    Ok(r) => r,
                    Err(p) => Err(VendorError::Other(panic_failure(p))),
                };
                let result = match output.signal.reason() {
                    Some(reason) => Err(VendorError::Cancelled(reason)),
                    None => result,
                };
                output.result.complete(result);
            },
            self.tracker.clone(),
        ));
        task
    }
    fn spawn_blocking(&self, body: Box<dyn FnOnce() -> VendorResult<()> + Send>) -> Arc<dyn VendorTask> {
        let task = Arc::new(IoTask {
            signal: Signal::new(),
            result: SharedDeferred::new(),
        });
        let output = task.clone();
        // Blocking work is runnable until it returns, so replay quiescence waits for it.
        // Its completion wakes the awaiter (tracked itself) before the count drops.
        let tracker = self.tracker.clone();
        tracker.acquire();
        self.handle.spawn_blocking(move || {
            let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
                Ok(r) => r,
                Err(p) => Err(VendorError::Other(panic_failure(p))),
            };
            output.result.complete(result);
            tracker.release();
            tracker.changed.notify_waiters();
        });
        task
    }
}

struct ChannelProbe<T>(Arc<SharedChannel<T>>);
impl<T: Send> MailboxProbe for ChannelProbe<T> {
    fn state(&self) -> MailboxState {
        EventChannel::state(self.0.as_ref())
    }
}
