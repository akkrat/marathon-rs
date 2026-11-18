//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/Scheduler.kt
//!
//! Pools and accumulators iterate in creation order, not ConcurrentHashMap order (WP 3.8
//! S2). `safeSend` is an ignored `try_send` on the single core thread (S3). The queue's
//! history prefetch is awaited in the subscription loop before the pool exists (D13, S4).
use crate::{
    analytics::Analytics,
    ci::Ci,
    error::*,
    execution::{
        DevicePoolMessage, FromScheduler,
        pool::{DevicePool, DevicePoolActor, PoolParts},
        progress::{PoolProgressAccumulator, ProgressServices},
        queue::{QueueActor, QueueMessage, SharedProgress},
    },
    executor::{CoreExecutor, shared::*},
    model::{DevicePoolId, TestShard},
    output::OutputDriver,
    runtime::*,
    strategy::pooling::PoolingStrategy,
    time::{ReplayBarriers, ReplayHooks, SchedulerClock, Timer},
    vendor::{Device, DeviceEvent, DeviceProvider, DeviceRef, TestBundleIdentifier},
    wiring::Tracker,
};
use futures_util::future::Either;
use indexmap::IndexMap;
use marathon_config::Configuration;
use marathon_error::FailureKind;
use marathon_logging::Level;
use std::{cell::RefCell, rc::Rc, sync::Arc};

const LOGGER: &str = "marathon_core::execution::scheduler";

/// D84: added to a device's own bound for ending a cancelled call (`Device::cancellation_cleanup_millis`), for the
/// core's handling after it. Past both, a stopping run finishes without that device.
pub const STOP_MARGIN_MILLIS: i64 = 5_000;

/// D84 (x82b): whether the run was asked to stop, by its owner or by a signal (D52). The Scheduler's `fail` and the
/// run's decision point read only this. It reads the owner's own job, which the owner's cancel reaches at once whatever
/// the run's tasks are doing (the run task may have completed, the protected finish may be running), and never a job of
/// the run's own machinery, which a failure inside the run can cancel. Everything here runs on the core thread, so a
/// request and a failure are ordered as they happen.
#[derive(Clone, Default)]
pub struct RunCancellation(Rc<RunCancellationState>);
#[derive(Default)]
struct RunCancellationState {
    owner: RefCell<Option<JobRef>>,
    requested: RefCell<Option<Cancellation>>,
}
impl RunCancellation {
    /// A run whose owner's job is `owner`: its cancellation is a request to stop the run.
    pub fn owned_by(owner: JobRef) -> Self {
        let cancellation = Self::default();
        cancellation.set_owner(owner);
        cancellation
    }
    pub fn set_owner(&self, owner: JobRef) {
        self.replace_owner(Some(owner));
    }
    /// The owner of the invocation now running (`None` for one without an owner). Each invocation of the run sets its
    /// own and releases it at its decision, so a previous owner's cancellation never reaches a later command.
    pub fn replace_owner(&self, owner: Option<JobRef>) {
        *self.0.owner.borrow_mut() = owner;
    }
    /// A request with no job behind it: a signal. The first one stands.
    pub fn request(&self, reason: Cancellation) {
        self.0.requested.borrow_mut().get_or_insert(reason);
    }
    /// The request, if the run was asked to stop.
    pub fn reason(&self) -> Option<Cancellation> {
        if let Some(reason) = self.0.requested.borrow().clone() {
            return Some(reason);
        }
        self.0
            .owner
            .borrow()
            .as_ref()
            .and_then(|owner| owner.cancellation_signal().reason())
    }
}

/// The Kotlin constructor's parameters, plus what the accumulators and actors need on core.
/// `context` is the run's coroutine context: the subscription is its child. `cancellation` says whether the run was
/// asked to stop.
pub struct SchedulerParts {
    pub core: Rc<CoreExecutor>,
    pub provider: Arc<dyn DeviceProvider>,
    pub analytics: Arc<Analytics>,
    pub configuration: Arc<Configuration>,
    pub shard: TestShard,
    pub tracker: Arc<dyn Tracker>,
    pub timer: Arc<dyn Timer>,
    pub bundle_identifier: Option<Arc<dyn TestBundleIdentifier>>,
    pub hooks: Arc<dyn ReplayHooks>,
    pub barriers: Arc<dyn ReplayBarriers>,
    pub ci: Ci,
    pub output: OutputDriver,
    pub context: TaskContext,
    pub cancellation: RunCancellation,
}

#[derive(Clone)]
pub struct Scheduler {
    inner: Rc<Inner>,
}
struct Inner {
    myself: std::rc::Weak<Inner>,
    parts: SchedulerParts,
    /// `Job()`: detached, the parent of every pool actor and pool supervisor.
    job: JobRef,
    pools: RefCell<IndexMap<DevicePoolId, DevicePool>>,
    results: RefCell<IndexMap<DevicePoolId, SharedProgress>>,
    pooling: PoolingStrategy,
    phase: std::cell::Cell<&'static str>,
    /// D68: a console failure is logged once per run.
    console_warned: Rc<std::cell::Cell<bool>>,
    /// D68, D84: the first error the run's own machinery ended with: a pool's queue (set by the queue job's completion,
    /// whether or not its pool is still there to hear of it), a pool, or the device subscription. Kept for the whole
    /// run, so that a failure after `execute` has returned still reaches the run's outcome.
    fatal: RefCell<Option<Failure>>,
    /// D84: while `execute` waits, the job it waits under; a stop cancels it to end the wait. None before and after.
    waiter: RefCell<Option<JobRef>>,
    /// The device subscription, which a stop ends.
    subscription: RefCell<Option<JobRef>>,
    /// D84 (F1): the provider's device channel has been seen to end (closed, failed or cancelled).
    channel_ended: std::cell::Cell<bool>,
    /// D84: the shutdown state. `None` while running; once `begin_stop` has run, the job of the stop routine, which has
    /// completed once the run has stopped. Set once.
    stop: RefCell<Option<JobRef>>,
    /// D84 (I2): set first by `begin_stop`. No pool is created once it is set.
    draining: std::cell::Cell<bool>,
    /// The provider's device channel the subscription reads, for `settle` to read what it ended with.
    channel: RefCell<Option<Arc<dyn EventChannel<DeviceEvent>>>>,
}

fn operation_to_vendor(error: OperationError) -> VendorError {
    match error {
        OperationError::Cancelled(reason) => VendorError::Cancelled(reason),
        OperationError::Failed(failure) => VendorError::Other(failure),
    }
}
fn vendor_to_operation(error: VendorError) -> OperationError {
    match error {
        VendorError::Cancelled(reason) => reason.into(),
        other => other.failure().clone().into(),
    }
}

impl Scheduler {
    pub fn new(parts: SchedulerParts) -> Self {
        // D84 (I3): a supervisor, so that a pool's or queue's failure cancels none of its siblings; the stop routine
        // cancels them, once each has taken what was delivered to it.
        let job = parts.core.job(JobKind::Supervisor, None);
        let pooling = PoolingStrategy::from_config(&parts.configuration.pooling_strategy);
        Self {
            inner: Rc::new_cyclic(|myself| Inner {
                myself: myself.clone(),
                parts,
                job,
                pools: RefCell::new(IndexMap::new()),
                results: RefCell::new(IndexMap::new()),
                pooling,
                phase: std::cell::Cell::new("not-started"),
                console_warned: Rc::default(),
                fatal: RefCell::new(None),
                waiter: RefCell::new(None),
                stop: RefCell::new(None),
                draining: std::cell::Cell::new(false),
                channel: RefCell::new(None),
                channel_ended: std::cell::Cell::new(false),
                subscription: RefCell::new(None),
            }),
        }
    }

    /// Bounded, synchronous view of existing core state; sends no actor or worker commands.
    pub fn state_dump(&self) -> String {
        let mut out = String::new();
        self.diagnostic(&mut out);
        crate::diagnostics::bound(&mut out);
        out
    }
    pub(crate) fn diagnostic(&self, out: &mut String) {
        use crate::diagnostics::{MAX_DEVICES, MAX_POOLS, job, name, omitted};
        use std::fmt::Write;
        let pools = self.inner.pools.borrow();
        let _ = writeln!(
            out,
            "scheduler={} phase={} pools={}",
            job(&self.inner.job),
            self.inner.phase.get(),
            pools.len()
        );
        let mut budget = MAX_DEVICES;
        for (id, pool) in pools.iter().take(MAX_POOLS) {
            let _ = writeln!(out, "pool={}", name(&id.name));
            pool.diagnostic(out, &mut budget);
        }
        omitted(out, pools.len().saturating_sub(MAX_POOLS), "pools");
    }
    /// The detached job, the Kotlin `job` field: cancelling it stops every pool, queue and device actor.
    pub fn job(&self) -> JobRef {
        self.inner.job.clone()
    }
    /// The pools created so far, in creation order.
    #[cfg(any(test, feature = "test-support"))]
    pub fn pools(&self) -> Vec<(DevicePoolId, DevicePool)> {
        self.inner
            .pools
            .borrow()
            .iter()
            .map(|(id, pool)| (id.clone(), pool.clone()))
            .collect()
    }

    /// D84 (I1): the first failure of the run's own machinery, also one after `execute` has returned (a pool that a
    /// late device started while the run finishes). It is the run's error.
    pub fn failure(&self) -> Option<Failure> {
        self.inner.fatal.borrow().clone()
    }
    /// D84 (R3-1, F2, F3, F6): the first step of the run's finish, before the provider terminates. Admission closes
    /// for good (no connection the listener still holds reaches a pool), the device listener is stopped and joined,
    /// what the provider's channel has ended with is read, pools or devices still running (a pool a late device
    /// started, or a pool whose supervisor still owns a device cleaning up) are stopped after taking what was
    /// delivered to them, and the stop is waited for. After this nothing of the Scheduler runs. Idempotent.
    pub async fn settle(&self) {
        let inner = &self.inner;
        inner.draining.set(true);
        let subscription = inner.subscription.borrow().clone();
        if let Some(subscription) = subscription {
            subscription.cancel(requested("The run is finishing"));
            let _ = subscription.join().await;
        }
        inner.read_channel_end();
        let running = inner
            .pools
            .borrow()
            .values()
            .any(|pool| !pool.terminated() || !matches!(pool.supervisor.state(), JobState::Completed(_)));
        if running {
            inner.begin_stop(requested("The run is finishing"));
        }
        if let Some(stop) = self.stopping() {
            let _ = stop.join().await;
        }
    }
    /// D84 (F1): after the provider has terminated, what its device channel ended with: a failure, or a cancellation
    /// nothing in the run asked for, is the run's cause. A stop it begins has nothing left to run and is waited for.
    pub async fn settle_after_termination(&self) {
        self.inner.read_channel_end();
        if let Some(stop) = self.stopping() {
            let _ = stop.join().await;
        }
    }
    /// D84: the stop routine's job once the run has begun to stop, completed once it has stopped. The run joins it
    /// before it finishes, so that no device work it waits for is still running when the provider terminates.
    pub fn stopping(&self) -> Option<JobRef> {
        self.inner.stop.borrow().clone()
    }

    /// `true` if the scheduled tests passed. Without a pool before
    /// `deviceInitializationTimeoutMillis`, NoDevices with an empty message.
    pub async fn execute(&self) -> VendorResult<bool> {
        // D84: an unwind out of here (a panic) begins the same stop as a failure, which the run owner joins. A normal
        // return disarms it: pools that join late keep D65's behaviour.
        let mut unwinding = StopOnUnwind(Some(self.inner.clone()));
        let context = &self.inner.parts.context;
        // D84: the waits below run under this job, so that a stop, which cancels it, ends them whatever they wait on.
        let waiter = self.inner.parts.core.job(JobKind::Ordinary, Some(context.job.clone()));
        *self.inner.waiter.borrow_mut() = Some(waiter.clone());
        let result = self.execute_inner(&waiter).await;
        self.inner.waiter.borrow_mut().take();
        waiter.cancel(requested("The scheduler has finished waiting"));
        let stop = self.inner.stop.borrow().clone();
        let result = match stop {
            Some(stop) => {
                self.inner.phase.set("stopping-after-failure");
                let _ = stop.join().await;
                match self.failure() {
                    Some(cause) => Err(VendorError::Other(cause)),
                    None => result,
                }
            }
            None => result,
        };
        unwinding.0.take();
        self.inner.phase.set("finished");
        result
    }

    async fn execute_inner(&self, waiter: &JobRef) -> VendorResult<bool> {
        let inner = &self.inner;
        let context = inner.parts.core.context(waiter.clone());
        Inner::subscribe_on_devices(inner.clone());
        inner.phase.set("waiting-for-pools");
        let timeouts = inner.parts.core.timeouts();
        let waiting = inner.clone();
        let waited = timeouts
            .with_timeout(
                context.clone(),
                inner.parts.configuration.device_initialization_timeout_millis,
                Box::new(move |scope: TaskContext| {
                    Box::pin(async move {
                        while waiting.pools.borrow().is_empty() {
                            scope.clock.delay(100, scope.job.clone()).await?;
                        }
                        Ok(())
                    })
                }),
            )
            .await;
        match waited {
            Ok(()) => {}
            Err(OperationError::Cancelled(Cancellation::Timeout { .. })) => {
                inner.job.cancel(job_was_cancelled());
                inner
                    .job
                    .join_in(context.job.clone())
                    .await
                    .map_err(VendorError::Cancelled)?;
                return Err(VendorError::NoDevices(failure(FailureKind::NoDevices, "")));
            }
            Err(error) => return Err(operation_to_vendor(error)),
        }
        // D65: retained pool indices cannot disconnect when a joined job detaches.
        // The first absent index closes admission, even if aggregation later suspends.
        inner.phase.set("joining-pools");
        let mut results = Vec::new();
        loop {
            let next = inner
                .pools
                .borrow()
                .get_index(results.len())
                .map(|(id, pool)| (id.clone(), pool.clone()));
            let Some((id, pool)) = next else { break };
            pool.actor
                .job()
                .join_in(context.job.clone())
                .await
                .map_err(VendorError::Cancelled)?;
            pool.supervisor
                .join_in(context.job.clone())
                .await
                .map_err(VendorError::Cancelled)?;
            let progress = inner.results.borrow().get(&id).cloned().ok_or_else(|| {
                VendorError::Other(failure(
                    FailureKind::IllegalState,
                    "Published pool has no progress accumulator",
                ))
            })?;
            results.push(progress);
        }
        // D68: a queue that ended with an error leaves results that cannot be trusted. Its cause is the outcome, which
        // `execute` returns.
        if inner.fatal.borrow().is_some() {
            return Ok(false);
        }
        // Preserve short-circuiting, but exclude accumulators still awaiting publication.
        inner.phase.set("aggregating-results");
        for progress in results {
            if !progress.lock().await.aggregate_result()? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// D84: begins the stop if `execute` unwinds instead of returning; the run owner joins it.
struct StopOnUnwind(Option<Rc<Inner>>);
impl Drop for StopOnUnwind {
    fn drop(&mut self) {
        if let Some(inner) = self.0.take() {
            if let Ok(mut waiter) = inner.waiter.try_borrow_mut() {
                waiter.take();
            }
            inner.begin_stop(requested("The scheduler stopped before it finished"));
        }
    }
}

/// `Job.cancel()` without a cause.
fn job_was_cancelled() -> Cancellation {
    Cancellation::Requested(failure(FailureKind::JobCancelled, "Job was cancelled"))
}

fn text(parts: &[&dyn Fn(&mut String)]) -> String {
    let mut out = String::default();
    for part in parts {
        part(&mut out);
    }
    out
}

impl Inner {
    fn debug(&self, message: String) {
        self.log(Level::DEBUG, message, None);
    }
    fn log(&self, level: Level, message: String, cause: Option<Failure>) {
        let core = &self.parts.core;
        core.log_queue()
            .log(LOGGER, core.clock().now_millis(), level, message, cause);
    }

    /// Launched in the run's context, not under `job`: a NoDevices timeout leaves it running.
    fn subscribe_on_devices(self: Rc<Self>) -> JobRef {
        let parent = self.parts.context.job.clone();
        let core = self.parts.core.clone();
        let inner = self.clone();
        let channel = self.parts.provider.subscribe();
        *self.channel.borrow_mut() = Some(channel.clone());
        let subscription = core.spawn(
            parent,
            Box::new(move |task: TaskContext| {
                Box::pin(async move {
                    let received = async {
                        loop {
                            let event = receive_device_event(channel.as_ref(), task.job.cancellation_signal())
                                .await
                                .map_err(vendor_to_operation)?;
                            match event {
                                Some(DeviceEvent::Connected(device)) => self.on_device_connected(device, &task).await?,
                                Some(DeviceEvent::Disconnected(device)) => self.on_device_disconnected(device)?,
                                None => return Ok(()),
                            }
                        }
                    }
                    .await;
                    // D84: a cancellation the subscription was not given (a cancelled device channel, or a call of its
                    // own) would end it silently, and the run would hear of no device again. It fails instead.
                    match received {
                        Err(OperationError::Cancelled(reason)) if task.job.cancellation_signal().reason().is_none() => {
                            let mut stopped = failure(
                                FailureKind::IllegalState,
                                "The device subscription stopped: it was cancelled, but the run was not",
                            );
                            stopped.cause = Some(Box::new(reason.failure().clone()));
                            Err(stopped.into())
                        }
                        other => other,
                    }
                })
            }),
        );
        // D84 (x82b): the subscription's failure reaches the run only through `fail`, never by failing the run's task:
        // so a cancellation of the run's task is always a request (the owner's, or a signal's), and `fail` can tell a
        // failure after it by the cancellation alone.
        subscription.scoped.set(true);
        inner.fail_with(&subscription);
        *inner.subscription.borrow_mut() = Some(subscription.clone());
        subscription
    }

    /// D68, D84: `job` is part of the run's own machinery. If it ends with an error, that is the run's failure.
    fn fail_with(&self, job: &JobRef) {
        let inner = self.myself.clone();
        job.invoke_on_completion(Box::new(move |completion| {
            if let (Completion::Failed(cause), Some(inner)) = (completion, inner.upgrade()) {
                inner.fail(cause);
            }
        }));
    }

    /// D84 (F1): reads, without waiting, what the provider's device channel has ended with, once the listener is no
    /// longer reading it. Events still buffered are dropped: the run is finishing. A failure is the run's cause, and so
    /// is a cancellation, which nothing in the run asks the provider for; a close is the normal end. A listener that
    /// failed has already read and recorded it.
    fn read_channel_end(&self) {
        if self.channel_ended.get() {
            return;
        }
        let subscription = self.subscription.borrow().clone();
        if let Some(subscription) = &subscription {
            if !matches!(subscription.state(), JobState::Completed(_)) {
                return;
            }
            if matches!(subscription.state(), JobState::Completed(Completion::Failed(_))) {
                self.channel_ended.set(true);
                return;
            }
        }
        let channel = self.channel.borrow().clone();
        let Some(channel) = channel else { return };
        use futures_util::FutureExt;
        while let Some(received) = channel.receive().now_or_never() {
            match received {
                Ok(Some(_)) => continue,
                Ok(None) => {
                    self.channel_ended.set(true);
                    return;
                }
                Err(VendorError::Cancelled(reason)) => {
                    self.channel_ended.set(true);
                    let mut cancelled = failure(
                        FailureKind::IllegalState,
                        "The provider's device channel was cancelled, but the run was not",
                    );
                    cancelled.cause = Some(Box::new(reason.failure().clone()));
                    self.fail(cancelled);
                    return;
                }
                Err(error) => {
                    self.channel_ended.set(true);
                    self.fail(error.failure().clone());
                    return;
                }
            }
        }
    }

    /// D68, D84 (I1): the first failure is the run's error and is logged at once; a later one is only logged. Either way
    /// the run stops.
    fn fail(&self, cause: Failure) {
        if self.fatal.borrow().is_some() {
            self.log(
                Level::WARN,
                "A further failure while the run stops after its first one".into(),
                Some(cause),
            );
            return;
        }
        // D84 (U215): the run was asked to stop (a signal, D52, or its owner) before this failure reached the Scheduler.
        // The request explains what fails after it, so the failure is not the run's cause; the run still stops. Without
        // this the outcome depended on whether a failure that ended in the same moment as the cancel was handled before
        // or after the run's wait saw the cancel.
        if let Some(reason) = self.parts.cancellation.reason() {
            self.log(
                Level::WARN,
                "A failure after the run was cancelled; the cancellation stands".into(),
                Some(cause),
            );
            self.begin_stop(reason);
            return;
        }
        *self.fatal.borrow_mut() = Some(cause.clone());
        self.log(
            Level::ERROR,
            "The run's own machinery failed; stopping the run".into(),
            Some(cause.clone()),
        );
        self.begin_stop(Cancellation::Requested(cause));
    }

    /// D84: the only way out of `Running`, synchronous and idempotent. In this order: admission closes (I2: the
    /// Scheduler, every pool and every queue are draining from here on, whatever their mailboxes still hold), the
    /// subscription stops, every device actor is cancelled, as a D52 cancel stops them, and `execute`'s wait ends. The
    /// rest is the stop routine, a cleanup task the run joins.
    fn begin_stop(&self, reason: Cancellation) {
        if self.stop.borrow().is_some() {
            return;
        }
        let Some(inner) = self.myself.upgrade() else { return };
        self.draining.set(true);
        // The pools are only borrowed for moments that run no job transition, so this always succeeds; if it ever did
        // not, stopping everything at once is still a stop.
        let pools: Vec<DevicePool> = match self.pools.try_borrow() {
            Ok(pools) => pools.values().cloned().collect(),
            Err(_) => {
                self.job.cancel(reason.clone());
                Vec::new()
            }
        };
        for pool in &pools {
            pool.drain();
        }
        let subscription = self.subscription.borrow().clone();
        if let Some(subscription) = subscription {
            subscription.cancel(reason.clone());
        }
        let mut devices = Vec::new();
        for pool in &pools {
            let queue = pool.queue.job();
            let actors = pool.devices();
            for child in pool.supervisor.children_snapshot() {
                if Rc::ptr_eq(&child, &queue) {
                    continue;
                }
                child.cancel(reason.clone());
                if let Some((_, device)) = actors.iter().find(|(job, _)| Rc::ptr_eq(job, &child)) {
                    devices.push((child, device.clone()));
                }
            }
        }
        let routine = self.parts.core.spawn_cleanup(Box::new(move |_| {
            Box::pin(async move {
                inner.stop_routine(pools, devices, reason).await;
                Ok(())
            })
        }));
        *self.stop.borrow_mut() = Some(routine);
        let waiter = self.waiter.borrow().clone();
        if let Some(waiter) = waiter {
            waiter.cancel(requested("The run is stopping"));
        }
    }

    /// D84: (I4) waits for every cancelled device to end, each within its vendor's own bound for a cancelled call (none
    /// for a vendor without one); (I3) has each pool and then its queue take what was delivered to them, through
    /// barriers each answers after the messages before it; then cancels the pools and queues and waits for them.
    async fn stop_routine(&self, pools: Vec<DevicePool>, devices: Vec<(JobRef, DeviceRef)>, reason: Cancellation) {
        let clock = self.parts.core.clock();
        let started = clock.now_millis();
        let timer = self.parts.core.job(JobKind::Ordinary, None);
        for (job, device) in devices {
            // The device's cancelled call, then what the vendor keeps running after it (R3-2: an Apple preparation),
            // both within the device's one bound.
            let vendor = self.parts.core.vendor();
            let held = device.0.clone();
            let ended = async move {
                let _ = job.join().await;
                let _ = crate::executor::bridge::call_vendor(vendor.as_ref(), Signal::new(), move |_| {
                    Box::pin(async move { held.join_cancelled_work().await })
                })
                .await;
            };
            match device.0.cancellation_cleanup_millis() {
                None => ended.await,
                Some(bound) => {
                    let limit = bound.saturating_add(STOP_MARGIN_MILLIS);
                    let left = started.saturating_add(limit).saturating_sub(clock.now_millis()).max(0);
                    if let Either::Right(_) = race(ended, clock.delay(left, timer.clone())).await {
                        self.log(
                            Level::ERROR,
                            format!(
                                "Device {} did not stop within {} ms of the run's stop; finishing the run without it",
                                device.0.info().serial_number,
                                limit
                            ),
                            None,
                        );
                    }
                }
            }
        }
        timer.cancel(requested("The devices have stopped"));
        for pool in &pools {
            let barrier = crate::execution::pool_message::DrainBarrier(SharedDeferred::new());
            let drain = DevicePoolMessage::FromScheduler(FromScheduler::Drain(barrier.clone()));
            if pool.actor.try_send(drain).is_ok() {
                let job = pool.actor.job();
                let _ = race(barrier.0.wait(), job.join()).await;
            }
            let (taken, answer) = tokio::sync::oneshot::channel();
            if pool.queue.try_send(QueueMessage::IsEmpty(taken)) {
                let queue = pool.queue.job();
                let _ = race(answer, queue.join()).await;
            }
        }
        self.job.cancel(reason);
        for pool in &pools {
            let _ = pool.actor.job().join().await;
            let _ = pool.queue.job().join().await;
        }
    }

    fn on_device_disconnected(&self, device: DeviceRef) -> OperationResult<()> {
        let serial = device.0.info().serial_number;
        if self.filtered_by_configuration(&serial)? {
            self.debug(text(&[
                &|m| m.push_str("device "),
                &|m| m.push_str(&serial.clone()),
                &|m| m.push_str(" is filtered out by configuration. skipping disconnect"),
            ]));
            return Ok(());
        }
        self.debug(text(&[
            &|m| m.push_str("device "),
            &|m| m.push_str(&serial.clone()),
            &|m| m.push_str(" disconnected"),
        ]));
        // Every pool, not only the device's own (B3); no deviceDisconnected is tracked (B2).
        let pools: Vec<DevicePool> = self.pools.borrow().values().cloned().collect();
        for pool in pools {
            let _ = pool
                .actor
                .try_send(DevicePoolMessage::FromScheduler(FromScheduler::RemoveDevice(
                    device.clone(),
                )));
        }
        Ok(())
    }

    async fn on_device_connected(&self, device: DeviceRef, task: &TaskContext) -> OperationResult<()> {
        // D84 (I2, F6): once the run stops or finishes, a connection the listener still reads reaches no pool.
        if self.draining.get() {
            return Ok(());
        }
        let info = device.0.info();
        let serial = info.serial_number.clone();
        if self.filtered_by_configuration(&serial)? {
            self.debug(text(&[
                &|m| m.push_str("device "),
                &|m| m.push_str(&serial.clone()),
                &|m| m.push_str(" is filtered out by configuration. skipping"),
            ]));
            return Ok(());
        }
        let device_view: &dyn Device = device.0.as_ref();
        let pool_id = self.pooling.associate(device_view);
        self.debug(text(&[
            &|m| m.push_str("device "),
            &|m| m.push_str(&serial.clone()),
            &|m| m.push_str(" associated with poolId "),
            &|m| m.push_str(&pool_id.name.clone()),
        ]));
        let progress = self.accumulator(&pool_id);
        if !self.pools.borrow().contains_key(&pool_id) {
            self.debug(text(&[
                &|m| m.push_str("pool actor "),
                &|m| m.push_str(&pool_id.name.clone()),
                &|m| m.push_str(" is being created"),
            ]));
            // D13: the history prefetch, awaited here so later device events keep their order (S4).
            let prepared = QueueActor::prepare(
                self.parts.core.vendor().as_ref(),
                self.parts.analytics.clone(),
                &self.parts.configuration,
                task.job.cancellation_signal(),
            )
            .await?;
            // D84 (I2): the run began to stop while the history was read.
            if self.draining.get() {
                return Ok(());
            }
            let pool = DevicePoolActor::spawn(PoolParts {
                core: self.parts.core.clone(),
                pool_id: pool_id.clone(),
                configuration: self.parts.configuration.clone(),
                progress,
                prepared,
                shard: self.parts.shard.clone(),
                timer: self.parts.timer.clone(),
                bundle_identifier: self.parts.bundle_identifier.clone(),
                parent: self.job.clone(),
                hooks: self.parts.hooks.clone(),
                barriers: self.parts.barriers.clone(),
            });
            self.on_failure(&pool);
            self.pools.borrow_mut().insert(pool_id.clone(), pool);
        }
        if self.draining.get() {
            return Ok(());
        }
        // A pool that has terminated stays here, closed: the device is dropped but still tracked (B1).
        let pool = self.pools.borrow().get(&pool_id).cloned();
        if let Some(pool) = pool {
            let _ = pool
                .actor
                .try_send(DevicePoolMessage::FromScheduler(FromScheduler::AddDevice(
                    device.clone(),
                )));
        }
        self.parts
            .tracker
            .device_connected(&pool_id, &info)
            .await
            .map_err(vendor_to_operation)
    }

    /// D68: a queue that ends with an error stops the run as a D52 cancel does, and its cause is kept for `execute`.
    /// This hangs on the queue's job, not on its pool, which may have closed on reading the failed queue as empty.
    /// D84: so does a pool that ends with an error: a panic in its handler, or a cancellation it was not given.
    fn on_failure(&self, pool: &DevicePool) {
        self.fail_with(&pool.queue.job());
        self.fail_with(&pool.actor.job());
    }

    /// `results.computeIfAbsent`, before the pool.
    fn accumulator(&self, pool_id: &DevicePoolId) -> SharedProgress {
        if let Some(progress) = self.results.borrow().get(pool_id) {
            return progress.clone();
        }
        let core = &self.parts.core;
        let accumulator = PoolProgressAccumulator::new(
            pool_id.clone(),
            &self.parts.shard,
            self.parts.configuration.execution_strategy.clone(),
            self.parts.ci,
            ProgressServices {
                tracker: self.parts.tracker.clone(),
                logs: core.log_queue(),
                clock: core.clock(),
                output: self.parts.output.clone(),
                console_warned: self.console_warned.clone(),
            },
        );
        let progress: SharedProgress = Rc::new(tokio::sync::Mutex::new(accumulator));
        self.results.borrow_mut().insert(pool_id.clone(), progress.clone());
        progress
    }

    fn filtered_by_configuration(&self, serial: &str) -> OperationResult<bool> {
        let configuration = &self.parts.configuration;
        let serial = serial.to_owned();
        let allow_list_accepted = configuration.include_serial_regexes.is_empty()
            || configuration
                .include_serial_regexes
                .iter()
                .any(|regex| regex.matches(&serial));
        let block_list_accepted = configuration.exclude_serial_regexes.is_empty()
            || !configuration
                .exclude_serial_regexes
                .iter()
                .any(|regex| regex.matches(&serial));
        Ok(!(allow_list_accepted && block_list_accepted))
    }
}

// EventChannel is erased at the provider boundary; retain receive_in's ready-first
// fast path and prompt cancellation once the receive has suspended.
async fn receive_device_event(
    channel: &dyn EventChannel<DeviceEvent>,
    signal: Arc<dyn CancellationSignal>,
) -> VendorResult<Option<DeviceEvent>> {
    let mut receive = channel.receive();
    if let std::task::Poll::Ready(event) = futures_util::poll!(receive.as_mut()) {
        return event;
    }
    match race(signal.cancelled(), receive).await {
        Either::Left(reason) => Err(VendorError::Cancelled(reason)),
        Either::Right(event) => event,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{FutureExt, task::noop_waker};
    use std::{future::Future, task::Context};

    #[test]
    fn provider_terminal_state_precedes_receiver_cancellation() {
        for failed in [false, true] {
            let channel = SharedChannel::<DeviceEvent>::new();
            let signal = Signal::new();
            let cause = failure(FailureKind::IllegalState, "provider failed");
            channel.close(failed.then(|| cause.clone()));
            signal.cancel(requested("receiver cancelled"));
            let result = receive_device_event(channel.as_ref(), signal).now_or_never();
            if failed {
                assert!(matches!(result, Some(Err(VendorError::Other(actual))) if actual == cause));
            } else {
                assert!(matches!(result, Some(Ok(None))));
            }
        }
    }

    #[test]
    fn suspended_provider_receive_preserves_prompt_cancellation() {
        let channel = SharedChannel::<DeviceEvent>::new();
        let signal = Signal::new();
        let mut receive = Box::pin(receive_device_event(channel.as_ref(), signal.clone()));
        let waker = noop_waker();
        let mut context = Context::from_waker(&waker);
        assert!(receive.as_mut().poll(&mut context).is_pending());
        channel.close(None);
        let reason = requested("receiver cancelled");
        signal.cancel(reason.clone());
        assert!(matches!(receive.now_or_never(), Some(Err(VendorError::Cancelled(actual))) if actual == reason));
    }
}
