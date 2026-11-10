//! Port of core/src/main/kotlin/com/malinskiy/marathon/extension/Timeout.kt
//! and execution/Retry.kt (scheduler delay and scoped cancellation).
use super::{CoreExecutor, shared::*};
use crate::{error::*, runtime::*, time::*};
use crossbeam_queue::SegQueue;
use futures_util::{FutureExt, future::Either};
use marathon_error::FailureKind;
use std::{
    rc::Weak,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;

static SCOPE: AtomicU64 = AtomicU64::new(1);
fn timeout(millis: i64) -> Cancellation {
    Cancellation::Timeout {
        scope: SCOPE.fetch_add(1, Ordering::Relaxed),
        detail: failure(
            FailureKind::TimeoutCancelled,
            &if millis <= 0 {
                "Timed out immediately".to_owned()
            } else {
                format!("Timed out waiting for {millis} ms")
            },
        ),
    }
}
/// Virtual timers are queued in every build but only the test driver reads them.
#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
pub(crate) struct TimerRequest {
    pub deadline: i64,
    pub sequence: u64,
    pub fired: oneshot::Sender<()>,
}
pub(crate) struct Timeline {
    pub virtual_time: bool,
    pub driver: AtomicBool,
    pub now: AtomicI64,
    pub timers: SegQueue<TimerRequest>,
    sequence: AtomicU64,
    epoch: std::time::Instant,
}
impl Timeline {
    fn new(virtual_time: bool) -> Self {
        Self {
            virtual_time,
            driver: AtomicBool::new(false),
            now: AtomicI64::new(0),
            timers: SegQueue::new(),
            sequence: AtomicU64::new(0),
            epoch: std::time::Instant::now(),
        }
    }
    pub fn now(&self) -> i64 {
        if self.virtual_time {
            self.now.load(Ordering::SeqCst)
        } else {
            self.epoch.elapsed().as_millis().min(i64::MAX as u128) as i64
        }
    }
    pub async fn delay(&self, millis: i64) -> Result<(), Failure> {
        if millis == i64::MAX {
            if self.virtual_time && !self.driver.load(Ordering::SeqCst) {
                return Err(replay_failure("virtual clock requires a ReplayDriver"));
            }
            return std::future::pending().await;
        }
        self.sleep(millis).await
    }
    pub async fn sleep(&self, millis: i64) -> Result<(), Failure> {
        if self.virtual_time && !self.driver.load(Ordering::SeqCst) {
            return Err(replay_failure("virtual clock requires a ReplayDriver"));
        }
        if millis <= 0 {
            return Ok(());
        }
        if self.virtual_time {
            let (fired, receiver) = oneshot::channel();
            let deadline = self.now().saturating_add(millis);
            self.timers.push(TimerRequest {
                deadline,
                sequence: self.sequence.fetch_add(1, Ordering::SeqCst),
                fired,
            });
            receiver.await.map_err(|_| replay_failure("timer driver stopped"))
        } else {
            tokio::time::sleep(Duration::from_millis(millis as u64)).await;
            Ok(())
        }
    }
}
pub struct RuntimeClock {
    pub(crate) timeline: Arc<Timeline>,
}
impl RuntimeClock {
    pub(crate) fn new(virtual_time: bool) -> Self {
        Self {
            timeline: Arc::new(Timeline::new(virtual_time)),
        }
    }
    /// Scheduler time for Send producers that stamp core log records (WP 3.10).
    pub fn now_source(&self) -> Arc<dyn Fn() -> i64 + Send + Sync> {
        let timeline = self.timeline.clone();
        Arc::new(move || timeline.now())
    }
    pub fn vendor(&self) -> Arc<dyn VendorClock> {
        Arc::new(SendClock {
            timeline: self.timeline.clone(),
        })
    }
}
impl SchedulerClock for RuntimeClock {
    fn now_millis(&self) -> i64 {
        self.timeline.now()
    }
    fn delay(&self, millis: i64, job: JobRef) -> LocalFuture<'_, Result<(), Cancellation>> {
        Box::pin(async move {
            // Nonpositive delay is a no-op even in a cancelled Kotlin context.
            if millis <= 0 {
                return Ok(());
            }
            let signal = job.cancellation_signal();
            match race(signal.cancelled(), self.timeline.delay(millis)).await {
                Either::Left(reason) => Err(reason),
                Either::Right(result) => result.map_err(Cancellation::Requested),
            }
        })
    }
}
/// Core timeout runner; typed results travel in captured cells or Deferreds.
pub struct RuntimeTimeouts {
    core: Weak<CoreExecutor>,
}
impl RuntimeTimeouts {
    pub(crate) fn new(core: Weak<CoreExecutor>) -> Self {
        Self { core }
    }
    async fn run(
        &self,
        parent: TaskContext,
        millis: i64,
        reason: Cancellation,
        body: TaskBody<'_>,
    ) -> OperationResult<()> {
        if millis <= 0 {
            return Err(reason.into());
        }
        let core = self
            .core
            .upgrade()
            .ok_or_else(|| failure(FailureKind::IllegalState, "Core runtime stopped"))?;
        let job = core.new_job(JobKind::Ordinary, Some(parent.job.clone()), true);
        job.scoped.set(true);
        let context = TaskContext {
            job: job.clone(),
            core: parent.core.clone(),
            clock: parent.clock.clone(),
        };
        let signal = job.cancellation_signal();
        let work_job = job.clone();
        let mut work = Box::pin(async move {
            let result = match std::panic::AssertUnwindSafe(async move { body(context).await })
                .catch_unwind()
                .await
            {
                Ok(result) => result,
                Err(payload) => Err(super::panic_failure(payload).into()),
            };
            work_job.finish_body(result.clone());
            work_job.join().await?;
            match work_job.state() {
                JobState::Completed(Completion::Failed(f)) => Err(f.into()),
                JobState::Completed(Completion::Cancelled(c)) => Err(c.into()),
                _ => result,
            }
        });
        match race(
            race(signal.cancelled(), core.clock.timeline.sleep(millis)),
            work.as_mut(),
        )
        .await
        {
            Either::Left(Either::Left(reason)) => {
                let result = work.as_mut().await;
                preserve_failure(result, reason)
            }
            Either::Left(Either::Right(timer)) => {
                let cancellation = match timer {
                    Ok(()) => reason,
                    Err(f) => Cancellation::Requested(f),
                };
                job.cancel(cancellation.clone());
                let result = work.as_mut().await;
                preserve_failure(result, cancellation)
            }
            Either::Right(result) => result,
        }
    }
    /// The body receives the scope's child job. Join its cleanup before returning.
    /// Own expiry is Cancelled(Timeout); inner/external cancellation and failures
    /// propagate unchanged, including the original timeout scope identity.
    pub fn with_timeout<'a>(
        &'a self,
        parent: TaskContext,
        millis: i64,
        body: TaskBody<'a>,
    ) -> LocalFuture<'a, OperationResult<()>> {
        Box::pin(self.run(parent, millis, timeout(millis), body))
    }
    /// Only this invocation's expiry becomes None; inner/external cancellation
    /// and ordinary failure propagate unchanged. Signed Java millis are retained.
    pub fn with_timeout_or_null<'a>(
        &'a self,
        parent: TaskContext,
        millis: i64,
        body: TaskBody<'a>,
    ) -> LocalFuture<'a, OperationResult<Option<()>>> {
        Box::pin(async move {
            let reason = timeout(millis);
            match self.run(parent, millis, reason.clone(), body).await {
                Ok(()) => Ok(Some(())),
                Err(OperationError::Cancelled(c)) if c == reason => Ok(None),
                Err(e) => Err(e),
            }
        })
    }
}
fn preserve_failure(result: OperationResult<()>, reason: Cancellation) -> OperationResult<()> {
    match result {
        Err(OperationError::Failed(f)) => Err(f.into()),
        _ => Err(reason.into()),
    }
}
struct SendClock {
    timeline: Arc<Timeline>,
}
impl SendClock {
    async fn run(
        &self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        reason: Cancellation,
        body: VendorTimeoutBody<'_>,
    ) -> VendorResult<()> {
        if millis <= 0 {
            return Err(VendorError::Cancelled(reason));
        }
        let signal = Signal::new();
        if let Some(reason) = parent.reason() {
            signal.cancel(reason);
        }
        let body_signal = signal.clone();
        let mut work = Box::pin(async move {
            match std::panic::AssertUnwindSafe(async move { body(body_signal).await })
                .catch_unwind()
                .await
            {
                Ok(result) => result,
                Err(payload) => Err(VendorError::Other(super::panic_failure(payload))),
            }
        });
        match race(race(parent.cancelled(), self.timeline.sleep(millis)), work.as_mut()).await {
            Either::Left(Either::Left(reason)) => {
                signal.cancel(reason.clone());
                match work.await {
                    Err(VendorError::Cancelled(_)) | Ok(()) => Err(VendorError::Cancelled(reason)),
                    Err(e) => Err(e),
                }
            }
            Either::Left(Either::Right(timer)) => {
                let reason = match timer {
                    Ok(()) => reason,
                    Err(f) => Cancellation::Requested(f),
                };
                signal.cancel(reason.clone());
                match work.await {
                    Err(VendorError::Cancelled(_)) | Ok(()) => Err(VendorError::Cancelled(reason)),
                    Err(e) => Err(e),
                }
            }
            Either::Right(result) => result,
        }
    }
}
impl VendorClock for SendClock {
    fn now_millis(&self) -> i64 {
        self.timeline.now()
    }
    fn delay(&self, millis: i64, cancellation: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if millis <= 0 {
                return Ok(());
            }
            match race(cancellation.cancelled(), self.timeline.delay(millis)).await {
                Either::Left(reason) => Err(VendorError::Cancelled(reason)),
                Either::Right(result) => result.map_err(VendorError::Other),
            }
        })
    }
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, ()> {
        Box::pin(self.run(parent, millis, timeout(millis), body))
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, Option<()>> {
        Box::pin(async move {
            let reason = timeout(millis);
            match self.run(parent, millis, reason.clone(), body).await {
                Ok(()) => Ok(Some(())),
                Err(VendorError::Cancelled(c)) if c == reason => Ok(None),
                Err(e) => Err(e),
            }
        })
    }
}
