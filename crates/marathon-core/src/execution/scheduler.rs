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
        queue::{QueueActor, SharedProgress},
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

/// The Kotlin constructor's parameters, plus what the accumulators and actors need on core.
/// `context` is the run's coroutine context: the subscription is its child.
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
}

#[derive(Clone)]
pub struct Scheduler {
    inner: Rc<Inner>,
}
struct Inner {
    parts: SchedulerParts,
    /// `Job()`: detached, the parent of every pool actor and pool supervisor.
    job: JobRef,
    pools: RefCell<IndexMap<DevicePoolId, DevicePool>>,
    results: RefCell<IndexMap<DevicePoolId, SharedProgress>>,
    pooling: PoolingStrategy,
    phase: std::cell::Cell<&'static str>,
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
        let job = parts.core.job(JobKind::Ordinary, None);
        let pooling = PoolingStrategy::from_config(&parts.configuration.pooling_strategy);
        Self {
            inner: Rc::new(Inner {
                parts,
                job,
                pools: RefCell::new(IndexMap::new()),
                results: RefCell::new(IndexMap::new()),
                pooling,
                phase: std::cell::Cell::new("not-started"),
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

    /// `true` if the scheduled tests passed. Without a pool before
    /// `deviceInitializationTimeoutMillis`, NoDevices with an empty message.
    pub async fn execute(&self) -> VendorResult<bool> {
        let result = self.execute_inner().await;
        self.inner.phase.set("finished");
        result
    }

    async fn execute_inner(&self) -> VendorResult<bool> {
        let inner = &self.inner;
        let context = inner.parts.context.clone();
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
        let core = &self.parts.core;
        core.log_queue()
            .log(LOGGER, core.clock().now_millis(), Level::DEBUG, message, None);
    }

    /// Launched in the run's context, not under `job`: a NoDevices timeout leaves it running.
    fn subscribe_on_devices(self: Rc<Self>) -> JobRef {
        let parent = self.parts.context.job.clone();
        let core = self.parts.core.clone();
        core.spawn(
            parent,
            Box::new(move |task: TaskContext| {
                Box::pin(async move {
                    let channel = self.parts.provider.subscribe();
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
                })
            }),
        )
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
            self.pools.borrow_mut().insert(pool_id.clone(), pool);
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
