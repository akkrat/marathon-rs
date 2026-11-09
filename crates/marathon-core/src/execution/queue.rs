//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/queue/QueueActor.kt
//!
//! A line-by-line port, including the fork's retry placement and the Kotlin bugs
//! listed in docs/known-kotlin-bugs.md (C5, C30, C45–C47), with D60's ordered queue.
use marathon_logging::Level;

use crate::{
    analytics::Analytics,
    error::*,
    execution::{DevicePoolMessage, FromQueue, progress::PoolProgressAccumulator},
    executor::{
        CoreExecutor, RuntimeActor,
        bridge::call_vendor,
        shared::{failure, race},
    },
    logging::LogQueue,
    model::{
        BatchRef, DeviceInfo, DevicePoolId, Test, TestAction, TestBatchResults, TestResult, TestShard, TestStatus,
    },
    runtime::*,
    strategy::{
        MetricsLookup,
        batching::{BatchingError, BatchingStrategy},
        retry::RetryStrategy,
        sorting::{SortingStrategy, TestQueue},
    },
    time::{ReplayHooks, SchedulerClock, Timer},
    vendor::TestBundleIdentifier,
    wiring::MetricsProvider,
};
use indexmap::IndexMap;
use marathon_config::Configuration;
use marathon_error::FailureKind;
use std::{
    collections::HashMap,
    rc::{Rc, Weak},
    sync::Arc,
};
use tokio::sync::oneshot;

/// The pool and the queue share one accumulator (Scheduler.kt:115). Kotlin's
/// `testEnded` never suspends and only the queue calls it, so a pool cannot
/// observe it half done; an async lock held across the awaited output and
/// tracker delivery keeps that (WP 3.7 decision 3b).
pub type SharedProgress = Rc<tokio::sync::Mutex<PoolProgressAccumulator>>;

/// `QueueMessage.RequestBatch`. `worker_id` is the device's `workerId`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestBatch {
    pub device: DeviceInfo,
    pub worker_id: Option<String>,
}

#[derive(Debug)]
pub enum QueueMessage {
    RequestBatch(RequestBatch),
    /// Requests of several devices, answered for each of them like `RequestBatch`.
    RequestBatches(Vec<RequestBatch>),
    /// Answered after every message sent before it. A queue that closes first drops
    /// the sender, so the receiver sees an error instead.
    IsEmpty(oneshot::Sender<bool>),
    Completed(DeviceInfo, TestBatchResults),
    ReturnBatch(DeviceInfo, BatchRef, String),
    RejectBatch(DeviceInfo, BatchRef),
    /// Answered with `FromQueue::OutcomeProcessed` once the messages sent before it are processed.
    ConfirmOutcome(DeviceInfo),
    Terminate,
}

/// The queue as the pool sees it (`DevicePoolActor.kt:236–280`).
#[derive(Clone)]
pub struct QueueHandle {
    actor: Rc<RuntimeActor<QueueMessage>>,
}
impl QueueHandle {
    /// `trySend(..).isSuccess`: false once the queue has closed itself.
    pub fn try_send(&self, message: QueueMessage) -> bool {
        self.actor.try_send(message).is_ok()
    }
    /// `queue.coroutineContext.job`, for awaitQueue's select over IsEmpty or join.
    pub fn job(&self) -> JobRef {
        Actor::job(self.actor.as_ref())
    }
    pub fn close(&self) {
        self.actor.close();
    }
    pub fn actor(&self) -> Rc<RuntimeActor<QueueMessage>> {
        self.actor.clone()
    }
}

/// The queue state after a message, in priority/FIFO and LinkedHashMap order.
#[cfg(feature = "test-support")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueSnapshot {
    pub queue: Vec<Test>,
    pub retry_workers: Vec<(Test, Vec<String>)>,
    pub active_batches: Vec<String>,
}
#[cfg(feature = "test-support")]
pub type QueueObserver = Rc<dyn Fn(QueueSnapshot)>;
/// Called on core just before the queue sends `ExecuteBatch` for a device (WP 3.8 decision 2): the moment
/// Kotlin's scheduling tests reach through a metrics provider queried while the batch is picked.
#[cfg(any(test, feature = "test-support"))]
pub type BeforeExecute = Rc<dyn Fn(&DeviceInfo, &BatchRef)>;
#[cfg(feature = "test-support")]
pub type KeyWrapper = Box<dyn FnOnce(crate::strategy::sorting::TestKey) -> crate::strategy::sorting::TestKey>;

/// The strategies and the analytics view of a queue whose history keys are prefetched (D13).
pub struct PreparedQueue {
    sorting: SortingStrategy,
    batching: BatchingStrategy,
    lookup: Arc<dyn MetricsLookup>,
    metrics: Arc<dyn MetricsProvider>,
    #[cfg(feature = "test-support")]
    wrapper: Option<KeyWrapper>,
    #[cfg(feature = "test-support")]
    observer: Option<QueueObserver>,
    #[cfg(any(test, feature = "test-support"))]
    before_execute: Option<BeforeExecute>,
}
impl PreparedQueue {
    /// A queue over a lookup and a recording provider, without a prefetch.
    #[cfg(feature = "test-support")]
    pub fn from_parts(
        configuration: &Configuration,
        lookup: Arc<dyn MetricsLookup>,
        metrics: Arc<dyn MetricsProvider>,
    ) -> Self {
        Self {
            sorting: SortingStrategy::from_config(&configuration.sorting_strategy),
            batching: BatchingStrategy::from_config(&configuration.batching_strategy),
            lookup,
            metrics,
            wrapper: None,
            observer: None,
            before_execute: None,
        }
    }
    /// Wraps the insertion key function, e.g. to record key extraction.
    #[cfg(feature = "test-support")]
    pub fn with_key(mut self, wrapper: KeyWrapper) -> Self {
        self.wrapper = Some(wrapper);
        self
    }
    /// Called with the queue state after each message, also when its handler fails.
    #[cfg(feature = "test-support")]
    pub fn with_observer(mut self, observer: QueueObserver) -> Self {
        self.observer = Some(observer);
        self
    }
    /// Runs `hook` just before each `ExecuteBatch` is sent to the pool.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_before_execute(mut self, hook: BeforeExecute) -> Self {
        self.before_execute = Some(hook);
        self
    }
}

/// The `ActorHandler`. Its state is owned by the receive loop, so no borrow
/// crosses an await.
pub struct QueueActor {
    pool: Weak<dyn Actor<DevicePoolMessage>>,
    pool_id: DevicePoolId,
    shard: TestShard,
    uncompleted_test_retry_quota: i32,
    timer: Arc<dyn Timer>,
    bundle_identifier: Option<Arc<dyn TestBundleIdentifier>>,
    progress: SharedProgress,
    hooks: Arc<dyn ReplayHooks>,
    logger: String,
    log: Rc<LogQueue>,
    clock: Rc<dyn SchedulerClock>,

    queue: TestQueue,
    batching: BatchingStrategy,
    retry: RetryStrategy,
    lookup: Arc<dyn MetricsLookup>,
    metrics: Arc<dyn MetricsProvider>,

    // LinkedHashMaps where Kotlin iterates them: the debug line joins
    // activeBatches' keys, and Notify flattens retryWorkers.
    active_batches: IndexMap<String, BatchRef>,
    uncompleted_tests_retry_count: HashMap<Test, i32>,
    /// Workers of the devices that have asked for batches, by serial number, and for each queued test, the workers of the
    /// attempts that queued its copies as retries. A device gets a test all of whose queued copies are retries from its own
    /// worker only if nothing else is queued
    workers: HashMap<String, String>,
    retry_workers: IndexMap<Test, Vec<String>>,
    /// Workers of the retries in the active batch of each device, by serial number, queued again if the batch is rejected
    active_retries: HashMap<String, Vec<(Test, String)>>,
    terminated: bool,
    actor: Option<Weak<dyn Actor<QueueMessage>>>,

    #[cfg(feature = "test-support")]
    observer: Option<QueueObserver>,
    #[cfg(any(test, feature = "test-support"))]
    before_execute: Option<BeforeExecute>,
}

fn vendor_error(error: VendorError) -> OperationError {
    match error {
        VendorError::Cancelled(reason) => reason.into(),
        other => other.failure().clone().into(),
    }
}
fn batching_error(error: BatchingError) -> OperationError {
    match error {
        BatchingError::Bundle(error) => vendor_error(error),
        BatchingError::Replay(error) => failure(FailureKind::Replay, &error.message.to_owned()).into(),
        // QueueActor never batches an empty queue.
        BatchingError::EmptyQueue => failure(FailureKind::NoSuchElement, "empty queue").into(),
    }
}
fn text(parts: &[&dyn Fn(&mut String)]) -> String {
    let mut out = String::default();
    for part in parts {
        part(&mut out);
    }
    out
}

impl QueueActor {
    /// The D13 prefetch of the strategies' history keys, run off core. An error
    /// is a cancellation, since the provider absorbs fetch failures (P1).
    pub async fn prepare(
        vendor: &dyn VendorRuntime,
        analytics: Arc<Analytics>,
        configuration: &Configuration,
        cancellation: Arc<dyn CancellationSignal>,
    ) -> OperationResult<PreparedQueue> {
        let sorting = SortingStrategy::from_config(&configuration.sorting_strategy);
        let batching = BatchingStrategy::from_config(&configuration.batching_strategy);
        let mut keys = sorting.metrics_keys();
        for key in batching.metrics_keys() {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        let fetcher = analytics.clone();
        call_vendor(vendor, cancellation.clone(), move |signal| {
            Box::pin(async move {
                // P4 permits dropping the history fetch, releasing the single-flight
                // lock without publishing an entry. Keep the bridge joined: only
                // this pool's prefetch is cancelled, never the run-wide provider.
                let result = match race(signal.cancelled(), fetcher.prefetch(&keys)).await {
                    futures_util::future::Either::Left(reason) => return Err(VendorError::Cancelled(reason)),
                    futures_util::future::Either::Right(result) => result,
                };
                result.map_err(|error| match error {
                    OperationError::Cancelled(reason) => VendorError::Cancelled(reason),
                    OperationError::Failed(failure) => VendorError::Other(failure),
                })
            })
        })
        .await
        .map_err(vendor_error)?;
        if let Some(reason) = cancellation.reason() {
            return Err(reason.into());
        }
        Ok(PreparedQueue {
            sorting,
            batching,
            lookup: analytics.lookup(),
            metrics: analytics,
            #[cfg(feature = "test-support")]
            wrapper: None,
            #[cfg(feature = "test-support")]
            observer: None,
            #[cfg(any(test, feature = "test-support"))]
            before_execute: None,
        })
    }

    /// The Kotlin constructor, on core: builds the queue (extracting its keys) and
    /// starts the actor. `parent` is the pool's supervisor job (DevicePoolActor.kt:63–74).
    ///
    /// `pool` must be the pool actor's own long-lived handle: the queue keeps it
    /// weak, as `DeviceActor` does, and a send to a pool that is gone fails.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        core: Rc<CoreExecutor>,
        prepared: PreparedQueue,
        configuration: &Configuration,
        shard: TestShard,
        pool: Rc<dyn Actor<DevicePoolMessage>>,
        pool_id: DevicePoolId,
        timer: Arc<dyn Timer>,
        bundle_identifier: Option<Arc<dyn TestBundleIdentifier>>,
        progress: SharedProgress,
        parent: JobRef,
        hooks: Arc<dyn ReplayHooks>,
    ) -> QueueHandle {
        // "QueueActor[$poolId]" with the data class toString.
        let logger = format!("QueueActor[DevicePoolId(name={})]", pool_id.name.to_owned());
        #[allow(unused_mut)]
        let mut key = prepared.sorting.process(prepared.lookup.clone(), hooks.clone());
        #[cfg(feature = "test-support")]
        if let Some(wrapper) = prepared.wrapper {
            key = wrapper(key);
        }
        let mut queue = TestQueue::new(key);
        queue.add_all(shard.tests.iter().chain(&shard.flaky_tests).cloned());
        let clock: Rc<dyn SchedulerClock> = core.clock();
        let handler = QueueActor {
            pool: Rc::downgrade(&pool),
            pool_id,
            shard,
            uncompleted_test_retry_quota: configuration.uncompleted_test_retry_quota,
            timer,
            bundle_identifier,
            progress,
            hooks,
            logger: logger.clone(),
            log: core.log_queue(),
            clock: clock.clone(),
            queue,
            batching: prepared.batching,
            // One strategy, so one retry quota, per pool (C30).
            retry: RetryStrategy::from_config(&configuration.retry_strategy),
            lookup: prepared.lookup,
            metrics: prepared.metrics,
            active_batches: IndexMap::new(),
            uncompleted_tests_retry_count: HashMap::new(),
            workers: HashMap::new(),
            retry_workers: IndexMap::new(),
            active_retries: HashMap::new(),
            terminated: false,
            actor: None,
            #[cfg(feature = "test-support")]
            observer: prepared.observer,
            #[cfg(any(test, feature = "test-support"))]
            before_execute: prepared.before_execute,
        };
        let actor = RuntimeActor::with_logger(core, parent, clock, handler, "QueueActor", &logger);
        QueueHandle { actor }
    }

    fn log(&self, level: Level, message: String) {
        self.log
            .log(&self.logger, self.clock.now_millis(), level, message, None);
    }
    fn debug(&self, prefix: &str, serial: &str) {
        self.log(Level::DEBUG, text(&[&|m| m.push_str(prefix), &|m| m.push_str(serial)]));
    }

    async fn send_pool(&self, message: FromQueue) -> OperationResult<()> {
        // A closed pool fails the send as `pool.send` throws: a cancellation, or the close cause.
        let Some(pool) = self.pool.upgrade() else {
            return Err(crate::executor::shared::requested("Channel was cancelled").into());
        };
        pool.send(DevicePoolMessage::FromQueue(message))
            .await
            .map_err(|error| match error.cause {
                Some(cause) => cause.into(),
                None => crate::executor::shared::requested("Channel was cancelled").into(),
            })
    }

    async fn handle(&mut self, message: QueueMessage, signal: Arc<dyn CancellationSignal>) -> OperationResult<()> {
        match message {
            QueueMessage::RequestBatch(request) => self.on_request_batch(request.device, request.worker_id).await,
            QueueMessage::RequestBatches(requests) => self.on_request_batches(requests).await,
            QueueMessage::IsEmpty(deferred) => {
                let _ = deferred.send(self.queue.is_empty() && self.active_batches.is_empty());
                Ok(())
            }
            QueueMessage::Terminate => {
                self.on_terminate();
                Ok(())
            }
            QueueMessage::Completed(device, results) => self.on_batch_completed(device, results, signal).await,
            QueueMessage::ReturnBatch(device, batch, reason) => {
                self.on_return_batch(device, batch, reason, signal).await
            }
            QueueMessage::RejectBatch(device, batch) => self.on_reject_batch(device, batch).await,
            QueueMessage::ConfirmOutcome(device) => self.send_pool(FromQueue::OutcomeProcessed(device)).await,
        }
    }

    async fn on_batch_completed(
        &mut self,
        device: DeviceInfo,
        results: TestBatchResults,
        signal: Arc<dyn CancellationSignal>,
    ) -> OperationResult<()> {
        let finished = &results.finished;
        let failed = &results.failed;
        self.debug("handle test results ", &device.serial_number);
        let observed: Vec<TestResult> = finished.iter().chain(failed).cloned().collect();
        // Kotlin's observe cannot throw; an error here ends the actor (P2).
        self.metrics.observe_execution_times(&observed).await?;
        let progress = self.progress.clone();
        let mut progress = progress.lock().await;
        if !finished.is_empty() {
            self.handle_finished_tests(finished, &device, &mut progress, &signal)
                .await?;
        }
        if !results.uncompleted.is_empty() {
            self.handle_uncompleted_tests(&results.uncompleted, &device, &mut progress, &signal)
                .await?;
        }
        if !failed.is_empty() {
            self.handle_failed_tests(failed, &device, &mut progress, &signal)
                .await?;
        }
        drop(progress);
        self.active_batches.shift_remove(&device.serial_number);
        self.active_retries.remove(&device.serial_number);
        self.on_batch_released().await
    }

    async fn handle_uncompleted_tests(
        &mut self,
        uncompleted_tests: &[TestResult],
        device: &DeviceInfo,
        progress: &mut PoolProgressAccumulator,
        signal: &Arc<dyn CancellationSignal>,
    ) -> OperationResult<()> {
        let over = charge_uncompleted(
            &mut self.uncompleted_tests_retry_count,
            self.uncompleted_test_retry_quota,
            uncompleted_tests.iter().map(|result| &result.test),
        );
        let mut over = over.into_iter();
        let (exceeded, uncompleted): (Vec<&TestResult>, Vec<&TestResult>) =
            uncompleted_tests.iter().partition(|_| over.next().unwrap_or(false));
        for test in exceeded {
            self.log(
                Level::DEBUG,
                text(&[&|m| m.push_str("uncompletedTestRetryQuota exceeded for "), &|m| {
                    m.push_str(&test.test.to_test_name('.', '#'))
                }]),
            );
            let action = progress
                .test_ended(device, test, true, signal.clone())
                .await
                .map_err(vendor_error)?;
            self.process_test_action(action, test, progress);
        }
        for result in uncompleted {
            let action = progress
                .test_ended(device, result, false, signal.clone())
                .await
                .map_err(vendor_error)?;
            match action {
                Some(TestAction::Complete) => self.process_test_action(action, result, progress),
                None => self.rerun_test(&result.test, device),
            }
        }
        Ok(())
    }

    async fn on_return_batch(
        &mut self,
        device: DeviceInfo,
        batch: BatchRef,
        reason: String,
        signal: Arc<dyn CancellationSignal>,
    ) -> OperationResult<()> {
        self.log(
            Level::DEBUG,
            text(&[
                &|m| m.push_str("onReturnBatch "),
                &|m| m.push_str(&device.serial_number.clone()),
                &|m| m.push_str(". reason="),
                &|m| m.push_str(&reason.clone()),
            ]),
        );
        let mut results = Vec::with_capacity(batch.tests.len());
        for test in &batch.tests {
            // One clock read per test.
            let current_time_millis = self
                .timer
                .current_time_millis()
                .map_err(|error| failure(FailureKind::Replay, &error.message.to_owned()))?;
            results.push(TestResult {
                test: test.clone(),
                device: device.clone(),
                test_batch_id: batch.id.clone(),
                status: TestStatus::Incomplete,
                start_time: current_time_millis,
                end_time: current_time_millis.wrapping_add(1),
                stacktrace: None,
                attachments: Vec::new(),
            });
        }
        let progress = self.progress.clone();
        let mut progress = progress.lock().await;
        self.handle_uncompleted_tests(&results, &device, &mut progress, &signal)
            .await?;
        drop(progress);
        self.active_batches.shift_remove(&device.serial_number);
        self.active_retries.remove(&device.serial_number);
        self.on_batch_released().await
    }

    /// The batch never reached the device, so its tests are queued again without counting an attempt
    async fn on_reject_batch(&mut self, device: DeviceInfo, batch: BatchRef) -> OperationResult<()> {
        let serial = &device.serial_number;
        self.debug("onRejectBatch ", serial);
        // Reference identity (`!==`): an equal batch handed out again is not this one.
        if !self
            .active_batches
            .get(serial)
            .is_some_and(|active| Arc::ptr_eq(active, &batch))
        {
            self.log(
                Level::WARN,
                text(&[
                    &|m| m.push_str("Batch "),
                    &|m| m.push_str(&batch.id.clone()),
                    &|m| m.push_str(" is not active on "),
                    &|m| m.push_str(&serial.clone()),
                    &|m| m.push_str(", not queueing it again"),
                ]),
            );
            return Ok(());
        }
        self.active_batches.shift_remove(serial);
        // Also tests that completed on another device meanwhile (C47).
        self.queue.add_all(batch.tests.iter().cloned());
        if let Some(pairs) = self.active_retries.remove(serial) {
            for (test, worker) in pairs {
                self.retry_workers.entry(test).or_default().push(worker);
            }
        }
        self.on_batch_released().await
    }

    /// Idle devices of the pool don't ask for work until they are notified, so the queue also has to terminate on its
    /// own once nothing is left to run
    async fn on_batch_released(&mut self) -> OperationResult<()> {
        if !self.queue.is_empty() {
            // values.flatten().toSet(): a LinkedHashSet, first occurrences in order.
            let mut retry_workers: Vec<String> = Vec::new();
            for worker in self.retry_workers.values().flatten() {
                if !retry_workers.contains(worker) {
                    retry_workers.push(worker.clone());
                }
            }
            self.send_pool(FromQueue::Notify { retry_workers }).await
        } else if self.active_batches.is_empty() {
            self.send_pool(FromQueue::Terminated).await?;
            self.on_terminate();
            Ok(())
        } else {
            Ok(())
        }
    }

    /// Retries that last ran on `worker` are left for devices on other workers, unless nothing else is queued
    fn next_batch(&mut self, device: &DeviceInfo, worker: Option<&str>) -> OperationResult<BatchRef> {
        let avoided = worker.map(|worker| self.own_retries(worker)).unwrap_or_default();
        let copies: Vec<Test> = if avoided.is_empty() {
            Vec::new()
        } else {
            self.queue
                .iter()
                .filter(|test| avoided.contains(test))
                .cloned()
                .collect()
        };
        let identifier = self.bundle_identifier.as_deref();
        let batch = if copies.is_empty() || copies.len() == self.queue.len() {
            self.batching
                .process(&mut self.queue, self.lookup.as_ref(), identifier, self.hooks.as_ref())
        } else {
            self.queue.remove_all(&avoided);
            let batch = self
                .batching
                .process(&mut self.queue, self.lookup.as_ref(), identifier, self.hooks.as_ref());
            // Kotlin's `finally`: restored even when batching throws.
            self.queue.add_all(copies);
            batch
        }
        .map_err(batching_error)?;
        let mut taken = Vec::new();
        for test in &batch.tests {
            if let Some(retry_worker) = self.dispatched(test, worker) {
                taken.push((test.clone(), retry_worker));
            }
        }
        self.active_retries.insert(device.serial_number.clone(), taken);
        Ok(Arc::new(batch))
    }

    /// Queued tests all of whose copies are retries of attempts on `worker`, in retryWorkers order.
    fn own_retries(&self, worker: &str) -> Vec<Test> {
        let mut copies: HashMap<&Test, i32> = HashMap::new();
        for test in self.queue.iter() {
            *copies.entry(test).or_insert(0) += 1;
        }
        self.retry_workers
            .iter()
            .filter(|(test, workers)| {
                let own = workers.iter().filter(|it| *it == worker).count() as i32;
                copies.get(test) == Some(&own)
            })
            .map(|(test, _)| test.clone())
            .collect()
    }

    /// A device on `worker` has taken a copy of `test`: the copy that suits it best, i.e. one that isn't a retry if there is
    /// one, otherwise a retry from another worker, otherwise one from `worker`
    ///
    /// Returns the worker of the retry taken, or None if the copy isn't a retry.
    fn dispatched(&mut self, test: &Test, worker: Option<&str>) -> Option<String> {
        // Counted after the batch has taken its copies, so two copies of one test in a
        // batch see the same count. ClassName takes every copy, so each still takes one
        // worker while any is left (WP 3.7 K3, refuted by the oracle).
        let copies = self.queue.iter().filter(|it| *it == test).count() + 1;
        let workers = self.retry_workers.get_mut(test)?;
        if copies > workers.len() {
            return None;
        }
        let index = workers.iter().position(|it| Some(it.as_str()) != worker).unwrap_or(0);
        let taken = workers.remove(index);
        if workers.is_empty() {
            self.retry_workers.shift_remove(test);
        }
        Some(taken)
    }

    fn rerun_test(&mut self, test: &Test, device: &DeviceInfo) {
        self.queue.offer(test.clone());
        if let Some(worker) = self.workers.get(&device.serial_number) {
            self.retry_workers.entry(test.clone()).or_default().push(worker.clone());
        }
    }

    fn on_terminate(&mut self) {
        self.terminated = true;
        if let Some(actor) = self.actor.as_ref().and_then(Weak::upgrade) {
            actor.close();
        }
    }

    /// A device that could only get retries from its own worker is served after the other devices of `requests`, which may take
    /// them, and gets what is left
    async fn on_request_batches(&mut self, requests: Vec<RequestBatch>) -> OperationResult<()> {
        let mut pending = requests;
        while !pending.is_empty() && !self.terminated {
            let index = pending
                .iter()
                .position(|request| !self.only_own_retries(request.worker_id.as_deref()))
                .unwrap_or(0);
            let next = pending.remove(index);
            self.on_request_batch(next.device, next.worker_id).await?;
        }
        Ok(())
    }

    fn only_own_retries(&self, worker: Option<&str>) -> bool {
        let Some(worker) = worker else { return false };
        if self.queue.is_empty() {
            return false;
        }
        let avoided = self.own_retries(worker);
        !avoided.is_empty() && self.queue.iter().all(|test| avoided.contains(test))
    }

    fn process_test_action(
        &mut self,
        action: Option<TestAction>,
        result: &TestResult,
        progress: &mut PoolProgressAccumulator,
    ) {
        if action == Some(TestAction::Complete) {
            // Test has reached final state. No need to run any of the other retries
            // This doesn't do anything with retries currently in progress
            let old_size = self.queue.len() as i32;
            self.queue.remove_all(std::slice::from_ref(&result.test));
            self.retry_workers.shift_remove(&result.test);
            let diff = old_size.wrapping_sub(self.queue.len() as i32);
            if diff >= 0 {
                progress.remove_test(&result.test, diff);
            }
        }
    }

    async fn handle_finished_tests(
        &mut self,
        finished: &[TestResult],
        device: &DeviceInfo,
        progress: &mut PoolProgressAccumulator,
        signal: &Arc<dyn CancellationSignal>,
    ) -> OperationResult<()> {
        for result in finished {
            let action = progress
                .test_ended(device, result, false, signal.clone())
                .await
                .map_err(vendor_error)?;
            self.process_test_action(action, result, progress);
        }
        Ok(())
    }

    async fn handle_failed_tests(
        &mut self,
        failed: &[TestResult],
        device: &DeviceInfo,
        progress: &mut PoolProgressAccumulator,
        signal: &Arc<dyn CancellationSignal>,
    ) -> OperationResult<()> {
        self.debug("handle failed tests ", &device.serial_number);
        let retry_list = self.retry.process(&self.pool_id, failed, &self.shard, progress);
        for result in &retry_list {
            let action = progress
                .test_ended(device, result, false, signal.clone())
                .await
                .map_err(vendor_error)?;
            self.process_test_action(action, result, progress);
            // Queued again even when the attempt just completed the test (C45).
            self.rerun_test(&result.test, device);
        }
        // By Test equality: another failed copy of a retried test is never ended (C46).
        let retried: Vec<&Test> = retry_list.iter().map(|retry| &retry.test).collect();
        for result in failed.iter().filter(|result| !retried.contains(&&result.test)) {
            let action = progress
                .test_ended(device, result, false, signal.clone())
                .await
                .map_err(vendor_error)?;
            self.process_test_action(action, result, progress);
        }
        Ok(())
    }

    async fn on_request_batch(&mut self, device: DeviceInfo, worker_id: Option<String>) -> OperationResult<()> {
        let serial = device.serial_number.clone();
        self.debug("request next batch for device ", &serial);
        if let Some(worker) = &worker_id {
            self.workers.insert(serial.clone(), worker.clone());
        }
        let queue_is_empty = self.queue.is_empty();

        // Don't separate the condition and the mutator into separate suspending blocks
        if !self.queue.is_empty() && !self.active_batches.contains_key(&serial) {
            self.debug("sending next batch for device ", &serial);
            let batch = self.next_batch(&device, worker_id.as_deref())?;
            self.active_batches.insert(serial, batch.clone());
            #[cfg(any(test, feature = "test-support"))]
            if let Some(hook) = &self.before_execute {
                hook(&device, &batch);
            }
            return self.send_pool(FromQueue::ExecuteBatch(device, batch)).await;
        }
        if queue_is_empty && self.active_batches.is_empty() {
            self.send_pool(FromQueue::Terminated).await?;
            self.on_terminate();
            Ok(())
        } else {
            if queue_is_empty {
                let mut message = String::from("queue is empty but there are active batches present for ");
                for (i, key) in self.active_batches.keys().enumerate() {
                    if i > 0 {
                        message.push_str(", ");
                    }
                    message.push_str(&key.clone());
                }
                self.log(Level::DEBUG, message);
            }
            self.send_pool(FromQueue::NoBatch(device)).await
        }
    }

    #[cfg(feature = "test-support")]
    fn observe(&self) {
        if let Some(observer) = &self.observer {
            observer(QueueSnapshot {
                queue: self.queue.iter().cloned().collect(),
                retry_workers: self
                    .retry_workers
                    .iter()
                    .map(|(test, workers)| (test.clone(), workers.clone()))
                    .collect(),
                active_batches: self.active_batches.keys().cloned().collect(),
            });
        }
    }
}

impl ActorHandler<QueueMessage> for QueueActor {
    fn started(&mut self, context: ActorContext<QueueMessage>) {
        self.actor = Some(Rc::downgrade(&context.actor));
    }
    fn receive(
        &mut self,
        message: QueueMessage,
        context: ActorContext<QueueMessage>,
    ) -> LocalFuture<'_, OperationResult<()>> {
        Box::pin(async move {
            // The queue job's signal (3.5 handoff): close() cancels the tracker delivery too.
            let signal = context.task.job.cancellation_signal();
            let result = self.handle(message, signal).await;
            #[cfg(feature = "test-support")]
            self.observe();
            result
        })
    }
}

/// Whether each uncompleted copy is over the quota, then one attempt charged to its test per copy. Partitioned on
/// the counts before any copy is charged, so copies in one batch all see the old count.
///
/// D47: the count saturates where Kotlin's `Int` wraps. Copies charged together step it by more than one, so with
/// four copies and a quota of 2147483646 a wrapping count jumps from 2147483644 into the negatives and never
/// reaches the quota, and the loop D47 relies on the quota to end would go on.
fn charge_uncompleted<'t>(
    counts: &mut HashMap<Test, i32>,
    quota: i32,
    tests: impl Iterator<Item = &'t Test> + Clone,
) -> Vec<bool> {
    let over = tests
        .clone()
        .map(|test| counts.get(test).copied().unwrap_or(0) >= quota)
        .collect();
    for test in tests {
        let count = counts.entry(test.clone()).or_insert(0);
        *count = count.saturating_add(1);
    }
    over
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test(method: &str) -> Test {
        Test {
            pkg: "pkg".into(),
            clazz: "Clazz".into(),
            method: method.into(),
            meta_properties: vec![],
        }
    }

    /// S-x30-1: four copies of one test, as class-name batching can put in one batch, at the largest quota D47 accepts.
    #[test]
    fn four_copies_at_the_largest_accepted_quota_still_reach_it() {
        let quota = i32::MAX - 1;
        let poisoned = test("poisoned");
        let mut counts = HashMap::from([(poisoned.clone(), quota - 2)]);
        let copies = [&poisoned; 4];
        assert_eq!(
            charge_uncompleted(&mut counts, quota, copies.into_iter()),
            [false; 4],
            "2147483644 is under the quota"
        );
        assert_eq!(counts[&poisoned], i32::MAX, "a wrapping count would be negative here");
        assert_eq!(
            charge_uncompleted(&mut counts, quota, copies.into_iter()),
            [true; 4],
            "every copy ends at the next return"
        );
        assert_eq!(counts[&poisoned], i32::MAX);
    }

    #[test]
    fn copies_in_one_batch_all_see_the_old_count() {
        let (a, b) = (test("a"), test("b"));
        let mut counts = HashMap::from([(a.clone(), 1)]);
        assert_eq!(
            charge_uncompleted(&mut counts, 2, [&a, &b, &a].into_iter()),
            [false, false, false]
        );
        assert_eq!((counts[&a], counts[&b]), (3, 1));
        assert_eq!(charge_uncompleted(&mut counts, 2, [&a, &b].into_iter()), [true, false]);
    }
}
