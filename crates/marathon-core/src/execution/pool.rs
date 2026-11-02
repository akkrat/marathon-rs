//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/DevicePoolActor.kt
//!
//! Line by line, including C27 (RemoveDevice removes by serial alone). Device
//! identity is `DeviceRef` pointer identity; the maps are keyed by serial number
//! as Kotlin's are, because the queue's replies carry only a `DeviceInfo`.
use crate::error::*;
use crate::{
    execution::{
        DevicePoolMessage, FromDevice, FromQueue, FromScheduler,
        device::{DeviceActor, DeviceActorRef, DeviceEvent},
        queue::{PreparedQueue, QueueActor, QueueHandle, QueueMessage, RequestBatch, SharedProgress},
    },
    executor::{CoreExecutor, RuntimeActor, shared::*},
    logging::LogQueue,
    model::{BatchRef, DeviceInfo, DevicePoolId, TestBatchResults, TestShard},
    runtime::*,
    time::{ReplayBarriers, ReplayHooks, SchedulerClock, Timer},
    vendor::{DeviceRef, TestBundleIdentifier},
};
use futures_util::future::Either;
use indexmap::IndexMap;
use jcompat::{logging::Level, writers::JavaString};
use marathon_config::Configuration;
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::{Rc, Weak},
    sync::Arc,
};

/// Everything the Kotlin constructor takes. `parent` is the Scheduler's detached job:
/// the pool's actor job and its supervisor are both children of it (DevicePoolActor.kt:35–61).
pub struct PoolParts {
    pub core: Rc<CoreExecutor>,
    pub pool_id: DevicePoolId,
    pub configuration: Arc<Configuration>,
    pub progress: SharedProgress,
    pub prepared: PreparedQueue,
    pub shard: TestShard,
    pub timer: Arc<dyn Timer>,
    pub bundle_identifier: Option<Arc<dyn TestBundleIdentifier>>,
    pub parent: JobRef,
    pub hooks: Arc<dyn ReplayHooks>,
    pub barriers: Arc<dyn ReplayBarriers>,
}

/// Called with each device actor the pool creates (test support: the Kotlin tests reach
/// a device actor's scope through the device's own coroutine context).
#[cfg(any(test, feature = "test-support"))]
pub type DeviceObserver = Rc<dyn Fn(&DeviceActorRef)>;

/// The pool as its owner sees it. `actor` is the pool's own long-lived handle: the queue
/// and the device actors keep it weak, so whoever owns the pool must keep this alive.
#[derive(Clone)]
pub struct DevicePool {
    pub actor: Rc<RuntimeActor<DevicePoolMessage>>,
    pub queue: QueueHandle,
    /// The `SupervisorJob` of the queue and the device actors.
    pub supervisor: JobRef,
}
impl DevicePool {
    /// `actor.coroutineContext.job.isCompleted`.
    pub fn terminated(&self) -> bool {
        matches!(Actor::job(self.actor.as_ref()).state(), JobState::Completed(_))
    }
}

struct Assignment {
    device: DeviceRef,
    batch: BatchRef,
}
struct FailedBatch {
    device: DeviceRef,
    /// The device once it is ready, waiting to ask for its next batch
    ready: Option<DeviceActorRef>,
}

/// The `ActorHandler`; its state is owned by the receive loop.
pub struct DevicePoolActor {
    core: Rc<CoreExecutor>,
    pool_id: DevicePoolId,
    configuration: Arc<Configuration>,
    hooks: Arc<dyn ReplayHooks>,
    barriers: Arc<dyn ReplayBarriers>,
    logger: String,
    log: Rc<LogQueue>,
    clock: Rc<dyn SchedulerClock>,
    /// The queue's construction inputs until `started` builds it.
    pending: Option<PendingQueue>,
    built: Rc<RefCell<Option<(QueueHandle, JobRef)>>>,
    queue: Option<QueueHandle>,
    supervisor: Option<JobRef>,
    myself: Option<Weak<dyn Actor<DevicePoolMessage>>>,

    // LinkedHashMap: onQueueTerminated terminates the devices in insertion order.
    devices: IndexMap<JavaString, DeviceActorRef>,
    /// Removed devices whose Terminated has not been received yet. A device sends its batch outcome before Terminated,
    /// so the pool must not shut down while any of them is pending
    terminating_devices: Vec<DeviceRef>,
    /// Batch requests in flight by serial number, mapped to the device the reply is for, or to None if that device has
    /// left the pool
    requests: HashMap<JavaString, Option<DeviceActorRef>>,
    assignments: HashMap<JavaString, Assignment>,
    /// A LinkedHashSet of actors: insertion order, actor identity.
    parked_devices: Vec<DeviceActorRef>,
    failed_batches: HashMap<JavaString, FailedBatch>,

    #[cfg(any(test, feature = "test-support"))]
    device_observer: Option<DeviceObserver>,
}

struct PendingQueue {
    prepared: PreparedQueue,
    shard: TestShard,
    timer: Arc<dyn Timer>,
    bundle_identifier: Option<Arc<dyn TestBundleIdentifier>>,
    progress: SharedProgress,
    parent: JobRef,
}

/// `Job.cancel()` without a cause.
fn job_was_cancelled() -> Cancellation {
    Cancellation::Requested(failure(
        "kotlinx.coroutines.JobCancellationException",
        "Job was cancelled",
    ))
}

fn same_actor(a: &DeviceActorRef, b: &DeviceActorRef) -> bool {
    Rc::ptr_eq(&a.actor, &b.actor)
}
fn serial(device: &DeviceRef) -> JavaString {
    device.0.info().serial_number
}

/// The Kotlin simple class name, for `sendToQueue`'s drop line.
fn simple_name(message: &QueueMessage) -> &'static str {
    match message {
        QueueMessage::RequestBatch(_) => "RequestBatch",
        QueueMessage::RequestBatches(_) => "RequestBatches",
        QueueMessage::IsEmpty(_) => "IsEmpty",
        QueueMessage::Completed(..) => "Completed",
        QueueMessage::ReturnBatch(..) => "ReturnBatch",
        QueueMessage::RejectBatch(..) => "RejectBatch",
        QueueMessage::ConfirmOutcome(_) => "ConfirmOutcome",
        QueueMessage::Terminate => "Terminate",
    }
}

enum Part<'a> {
    S(&'a str),
    J(&'a JavaString),
}
fn text(parts: &[Part<'_>]) -> JavaString {
    let mut out = JavaString::default();
    for part in parts {
        match part {
            Part::S(s) => out.push_str(s),
            Part::J(j) => out.append((*j).clone()),
        }
    }
    out
}
use Part::{J, S};

impl DevicePoolActor {
    /// The Kotlin constructor: starts the pool actor, then (in `started`) its supervisor
    /// and its queue, as the property initialisers do (DevicePoolActor.kt:39–74).
    pub fn spawn(parts: PoolParts) -> DevicePool {
        Self::spawn_with(
            parts,
            #[cfg(any(test, feature = "test-support"))]
            None,
        )
    }

    /// As `spawn`, calling `observer` with each device actor the pool creates.
    #[cfg(any(test, feature = "test-support"))]
    pub fn spawn_observed(parts: PoolParts, observer: DeviceObserver) -> DevicePool {
        Self::spawn_with(parts, Some(observer))
    }

    fn spawn_with(
        parts: PoolParts,
        #[cfg(any(test, feature = "test-support"))] observer: Option<DeviceObserver>,
    ) -> DevicePool {
        let logger = format!("DevicePoolActor[{}]", parts.pool_id.name.to_string_lossy());
        let clock: Rc<dyn SchedulerClock> = parts.core.clock();
        let built = Rc::new(RefCell::new(None));
        let handler = DevicePoolActor {
            core: parts.core.clone(),
            pool_id: parts.pool_id,
            configuration: parts.configuration,
            hooks: parts.hooks,
            barriers: parts.barriers,
            logger: logger.clone(),
            log: parts.core.log_queue(),
            clock: clock.clone(),
            pending: Some(PendingQueue {
                prepared: parts.prepared,
                shard: parts.shard,
                timer: parts.timer,
                bundle_identifier: parts.bundle_identifier,
                progress: parts.progress,
                parent: parts.parent.clone(),
            }),
            built: built.clone(),
            queue: None,
            supervisor: None,
            myself: None,
            devices: IndexMap::new(),
            terminating_devices: Vec::new(),
            requests: HashMap::new(),
            assignments: HashMap::new(),
            parked_devices: Vec::new(),
            failed_batches: HashMap::new(),
            #[cfg(any(test, feature = "test-support"))]
            device_observer: observer,
        };
        let actor = RuntimeActor::with_logger(parts.core, parts.parent, clock, handler, "DevicePoolActor", &logger);
        let (queue, supervisor) = match built.borrow_mut().take() {
            Some(built) => built,
            None => unreachable!("started runs inside RuntimeActor::with_logger"),
        };
        DevicePool {
            actor,
            queue,
            supervisor,
        }
    }

    fn log(&self, level: Level, message: JavaString) {
        self.log
            .log(&self.logger, self.clock.now_millis(), level, message, None);
    }
    fn debug(&self, parts: &[Part<'_>]) {
        self.log(Level::Debug, text(parts));
    }
    fn pool(&self) -> JavaString {
        // "$poolId" with the data class toString.
        text(&[S("DevicePoolId(name="), J(&self.pool_id.name), S(")")])
    }
    fn queue(&self) -> &QueueHandle {
        match &self.queue {
            Some(queue) => queue,
            None => unreachable!("the queue is built in started"),
        }
    }

    async fn handle(&mut self, message: DevicePoolMessage, task: &TaskContext) -> OperationResult<()> {
        match message {
            DevicePoolMessage::FromScheduler(FromScheduler::AddDevice(device)) => self.add_device(device),
            DevicePoolMessage::FromScheduler(FromScheduler::RemoveDevice(device)) => {
                self.remove_device(device, task).await?
            }
            DevicePoolMessage::FromScheduler(FromScheduler::Terminate) => self.terminate(task).await?,
            DevicePoolMessage::FromDevice(FromDevice::IsReady(device)) => self.device_ready(device),
            DevicePoolMessage::FromDevice(FromDevice::CompletedTestBatch(device, results)) => {
                self.device_completed(device, results)
            }
            DevicePoolMessage::FromDevice(FromDevice::ReturnTestBatch(device, batch, reason)) => {
                self.device_returned_test_batch(device, batch, reason)
            }
            DevicePoolMessage::FromDevice(FromDevice::Terminated(device)) => {
                self.device_terminated(device, task).await?
            }
            DevicePoolMessage::FromQueue(FromQueue::Notify { retry_workers }) => {
                self.request_parked_devices(&retry_workers)
            }
            DevicePoolMessage::FromQueue(FromQueue::NoBatch(device)) => self.park_device(&device),
            DevicePoolMessage::FromQueue(FromQueue::Terminated) => self.on_queue_terminated(task).await?,
            DevicePoolMessage::FromQueue(FromQueue::ExecuteBatch(device, batch)) => self.execute_batch(device, batch),
            DevicePoolMessage::FromQueue(FromQueue::OutcomeProcessed(device)) => self.outcome_processed(&device),
        }
        Ok(())
    }

    async fn on_queue_terminated(&mut self, task: &TaskContext) -> OperationResult<()> {
        self.debug(&[S("pool "), J(&self.pool()), S(": onQueueTerminated")]);
        for actor in self.devices.values() {
            let _ = actor.actor.try_send(DeviceEvent::Terminate);
        }
        self.terminate(task).await?;
        Ok(())
    }

    fn device_returned_test_batch(&mut self, device: DeviceRef, batch: BatchRef, reason: JavaString) {
        let serial = serial(&device);
        self.debug(&[
            S("pool "),
            J(&self.pool()),
            S(": device "),
            J(&serial),
            S(" returned test batch"),
        ]);
        self.release_assignment(&device);
        self.send_to_queue(QueueMessage::ReturnBatch(device.0.info(), batch, reason));
        self.await_outcome(&device);
    }

    async fn device_terminated(&mut self, device: DeviceRef, task: &TaskContext) -> OperationResult<()> {
        let serial = serial(&device);
        if self.devices.get(&serial).is_some_and(|actor| actor.device == device) {
            self.devices.shift_remove(&serial);
        }
        self.terminating_devices.retain(|it| *it != device);
        self.release_device(&device);
        // A device accounts for a batch it has started before it reports Terminated, so this batch has never started
        if let Some(batch) = self.release_assignment(&device) {
            self.debug(&[
                S("pool "),
                J(&self.pool()),
                S(": device "),
                J(&serial),
                S(" terminated before starting batch "),
                J(&batch.id),
            ]);
            self.send_to_queue(QueueMessage::RejectBatch(device.0.info(), batch));
        }
        let size = self.devices.len().to_string();
        self.debug(&[
            S("pool "),
            J(&self.pool()),
            S(": device "),
            J(&serial),
            S(" terminated. devices.size = "),
            S(&size),
        ]);
        if self.no_active_devices() {
            self.log(
                Level::Error,
                text(&[
                    S("pool "),
                    J(&self.pool()),
                    S(": all devices terminated, remaining tests will not run"),
                ]),
            );
            self.terminate(task).await?;
        }
        Ok(())
    }

    fn device_completed(&mut self, device: DeviceRef, results: TestBatchResults) {
        self.release_assignment(&device);
        let failed = !results.failed.is_empty() || !results.uncompleted.is_empty();
        self.send_to_queue(QueueMessage::Completed(device.0.info(), results));
        if failed {
            self.await_outcome(&device);
        }
    }

    /// The device asks for its next batch only once the queue has processed the outcome of its failed batch and notified
    /// the parked devices about the retries, so they ask first
    fn await_outcome(&mut self, device: &DeviceRef) {
        if self.send_to_queue(QueueMessage::ConfirmOutcome(device.0.info())) {
            self.failed_batches.insert(
                serial(device),
                FailedBatch {
                    device: device.clone(),
                    ready: None,
                },
            );
        }
    }

    fn outcome_processed(&mut self, device: &DeviceInfo) {
        let Some(failed) = self.failed_batches.remove(&device.serial_number) else {
            return;
        };
        let Some(actor) = failed.ready else { return };
        if self
            .devices
            .get(&device.serial_number)
            .is_some_and(|current| same_actor(current, &actor))
        {
            self.request_batches(vec![actor]);
        }
    }

    fn device_ready(&mut self, device: DeviceRef) {
        let serial = serial(&device);
        // A late notification from a removed or replaced device must not claim work
        let Some(actor) = self
            .devices
            .get(&serial)
            .filter(|actor| actor.device == device)
            .cloned()
        else {
            return;
        };
        let requested = matches!(self.requests.get(&serial), Some(Some(r)) if same_actor(r, &actor));
        let assigned = self.assignments.get(&serial).is_some_and(|a| a.device == device);
        let parked = self.parked_devices.iter().any(|p| same_actor(p, &actor));
        if requested || assigned || parked {
            return;
        }
        if let Some(failed) = self.failed_batches.get_mut(&serial).filter(|f| f.device == device) {
            failed.ready = Some(actor);
            return;
        }
        self.request_batches(vec![actor]);
    }

    /// Several devices ask in one message, so the queue can hand out its retries among all of them
    fn request_batches(&mut self, actors: Vec<DeviceActorRef>) {
        let mut asking = Vec::new();
        for actor in actors {
            let serial = serial(&actor.device);
            let in_flight = self.requests.contains_key(&serial);
            self.requests.insert(serial, Some(actor.clone()));
            if !in_flight {
                asking.push(actor);
            }
        }
        if asking.is_empty() {
            return;
        }
        let mut requests: Vec<RequestBatch> = asking
            .iter()
            .map(|actor| RequestBatch {
                device: actor.device.0.info(),
                worker_id: actor.device.0.worker_id(),
            })
            .collect();
        let message = if requests.len() == 1 {
            QueueMessage::RequestBatch(requests.remove(0))
        } else {
            QueueMessage::RequestBatches(requests)
        };
        if !self.send_to_queue(message) {
            for actor in asking {
                self.requests.remove(&serial(&actor.device));
                if !self.parked_devices.iter().any(|p| same_actor(p, &actor)) {
                    self.parked_devices.push(actor);
                }
            }
        }
    }

    /// Devices on `retry_workers`, which can be the cause of the failures the retries are for, ask after the others
    fn request_parked_devices(&mut self, retry_workers: &[JavaString]) {
        let (later, first): (Vec<_>, Vec<_>) = std::mem::take(&mut self.parked_devices)
            .into_iter()
            .partition(|actor| actor.device.0.worker_id().is_some_and(|w| retry_workers.contains(&w)));
        self.request_batches(first.into_iter().chain(later).collect());
    }

    fn park_device(&mut self, device: &DeviceInfo) {
        if let Some(Some(actor)) = self.requests.remove(&device.serial_number)
            && !self.parked_devices.iter().any(|p| same_actor(p, &actor))
        {
            self.parked_devices.push(actor);
        }
    }

    fn execute_batch(&mut self, device: DeviceInfo, batch: BatchRef) {
        // `requests.remove` is null both for a missing key and for a device that has left
        let Some(Some(actor)) = self.requests.remove(&device.serial_number) else {
            self.debug(&[
                S("pool "),
                J(&self.pool()),
                S(": device "),
                J(&device.serial_number),
                S(" left before receiving batch "),
                J(&batch.id),
            ]);
            self.send_to_queue(QueueMessage::RejectBatch(device, batch));
            return;
        };
        self.assignments.insert(
            device.serial_number,
            Assignment {
                device: actor.device.clone(),
                batch: batch.clone(),
            },
        );
        // A device that has closed reports Terminated, which rejects the batch
        let _ = actor.actor.try_send(DeviceEvent::Execute(batch));
    }

    /// The queue closes itself once it is drained, so a send can fail; it never throws
    fn send_to_queue(&self, message: QueueMessage) -> bool {
        let name = simple_name(&message);
        let sent = self.queue().try_send(message);
        if !sent {
            self.debug(&[
                S("pool "),
                J(&self.pool()),
                S(": queue is closed, "),
                S(name),
                S(" is dropped"),
            ]);
        }
        sent
    }

    /// A reply to a request of a leaving device is not delivered to it
    fn release_device(&mut self, device: &DeviceRef) {
        let serial = serial(device);
        if let Some(slot) = self.requests.get_mut(&serial)
            && slot.as_ref().is_some_and(|actor| actor.device == *device)
        {
            *slot = None;
        }
        self.parked_devices.retain(|actor| actor.device != *device);
        if self.failed_batches.get(&serial).is_some_and(|f| f.device == *device) {
            self.failed_batches.remove(&serial);
        }
    }

    fn release_assignment(&mut self, device: &DeviceRef) -> Option<BatchRef> {
        let serial = serial(device);
        if !self.assignments.get(&serial).is_some_and(|a| a.device == *device) {
            return None;
        }
        self.assignments.remove(&serial).map(|a| a.batch)
    }

    async fn terminate(&mut self, task: &TaskContext) -> OperationResult<()> {
        self.debug(&[S("terminate pool "), J(&self.pool())]);
        self.await_queue(task).await?;
        if let Some(supervisor) = &self.supervisor {
            supervisor.cancel(job_was_cancelled());
        }
        if let Some(myself) = self.myself.as_ref().and_then(Weak::upgrade) {
            myself.close();
        }
        Ok(())
    }

    /// Cancelling the pool drops messages the queue has not processed yet, e.g. results of the last device.
    /// The queue answers IsEmpty only after the messages sent to it before
    async fn await_queue(&self, task: &TaskContext) -> OperationResult<()> {
        let (processed, answer) = tokio::sync::oneshot::channel();
        let queue = self.queue();
        if !queue.try_send(QueueMessage::IsEmpty(processed)) {
            return Ok(());
        }
        let job = queue.job();
        // The Boolean is ignored; a queue that ends first drops the sender.
        let signal = task.job.cancellation_signal();
        match race(signal.cancelled(), race(answer, job.join())).await {
            Either::Left(reason) => Err(reason.into()),
            Either::Right(_) => Ok(()),
        }
    }

    async fn remove_device(&mut self, device: DeviceRef, task: &TaskContext) -> OperationResult<()> {
        let serial = serial(&device);
        self.debug(&[S("remove device "), J(&serial)]);
        // By serial alone: a stale disconnect removes the replacement (C27)
        let actor = self.devices.shift_remove(&serial);
        if let Some(actor) = &actor {
            self.terminating_devices.push(actor.device.clone());
            self.release_device(&actor.device);
            let _ = actor.actor.try_send(DeviceEvent::Terminate);
        }
        let size = self.devices.len().to_string();
        self.debug(&[S("devices.size = "), S(&size)]);
        if self.no_active_devices() {
            self.debug(&[S("pool "), J(&self.pool()), S(": noActiveDevices")]);
            self.terminate(task).await?;
        }
        Ok(())
    }

    fn no_active_devices(&self) -> bool {
        self.devices.is_empty() && self.terminating_devices.is_empty()
    }

    fn add_device(&mut self, device: DeviceRef) {
        let serial = serial(&device);
        if self.devices.contains_key(&serial) {
            self.log(
                Level::Warn,
                text(&[
                    S("device "),
                    J(&serial),
                    S(" already present in pool "),
                    J(&self.pool_id.name),
                ]),
            );
            return;
        }
        self.debug(&[S("add device "), J(&serial)]);
        let (Some(pool), Some(supervisor)) = (self.myself.as_ref().and_then(Weak::upgrade), self.supervisor.clone())
        else {
            unreachable!("the pool is alive while it handles a message")
        };
        let actor = DeviceActor::spawn(
            self.core.clone(),
            self.pool_id.clone(),
            pool,
            self.configuration.clone(),
            device,
            supervisor,
            self.hooks.clone(),
            self.barriers.clone(),
        );
        #[cfg(any(test, feature = "test-support"))]
        if let Some(observer) = &self.device_observer {
            observer(&actor);
        }
        self.devices.insert(serial, actor.clone());
        let _ = actor.actor.try_send(DeviceEvent::Initialize);
    }
}

impl ActorHandler<DevicePoolMessage> for DevicePoolActor {
    fn started(&mut self, context: ActorContext<DevicePoolMessage>) {
        self.myself = Some(Rc::downgrade(&context.actor));
        let Some(pending) = self.pending.take() else { return };
        // Any problem with a device should not propagate a cancellation upstream
        let supervisor = self.core.job(JobKind::Supervisor, Some(pending.parent));
        let queue = QueueActor::spawn(
            self.core.clone(),
            pending.prepared,
            &self.configuration,
            pending.shard,
            context.actor.clone(),
            self.pool_id.clone(),
            pending.timer,
            pending.bundle_identifier,
            pending.progress,
            supervisor.clone(),
            self.hooks.clone(),
        );
        *self.built.borrow_mut() = Some((queue.clone(), supervisor.clone()));
        self.queue = Some(queue);
        self.supervisor = Some(supervisor);
    }
    fn receive(
        &mut self,
        message: DevicePoolMessage,
        context: ActorContext<DevicePoolMessage>,
    ) -> LocalFuture<'_, OperationResult<()>> {
        Box::pin(async move { self.handle(message, &context.task).await })
    }
}
