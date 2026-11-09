//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/device/DeviceActor.kt
use super::state::{DeviceAction, DeviceEvent, DeviceState, Running};
use crate::{
    error::*,
    execution::{DevicePoolMessage, FromDevice},
    executor::{
        CoreExecutor, RuntimeActor, SharedDeferred,
        bridge::call_vendor,
        retry::with_retry,
        shared::{failure, race},
    },
    logging::LogQueue,
    model::{BatchRef, DevicePoolId, TestBatchResults},
    runtime::*,
    state_machine::{StateMachine, Transition, dont_transition, transition_to},
    time::{ReplayBarriers, ReplayHooks, SchedulerClock},
    vendor::{Device, DeviceRef, VendorContext},
};
use futures_util::future::Either;
use jcompat::logging::Level;
use marathon_config::Configuration;
use marathon_error::FailureKind;
use std::{
    cell::{Cell, RefCell},
    future::Future,
    rc::{Rc, Weak},
    sync::Arc,
};

const PREPARE: &str = "core.DeviceActor.prepare";
const EXECUTE: &str = "core.DeviceActor.execute";

/// The actor and the device it was created for (Kotlin's `DeviceActor.device`).
#[derive(Clone)]
pub struct DeviceActorRef {
    pub actor: Rc<RuntimeActor<DeviceEvent>>,
    pub device: DeviceRef,
    /// Only the test-support accessors below read it.
    #[cfg(feature = "test-support")]
    shared: Weak<Shared>,
}
#[cfg(feature = "test-support")]
impl DeviceActorRef {
    /// The `initializeJob` field, which the oracle probe reads by reflection.
    pub fn initialize_job(&self) -> Option<JobRef> {
        self.shared
            .upgrade()
            .and_then(|shared| shared.initialize_job.borrow().clone())
    }
    /// The `executeJob` field.
    pub fn execute_job(&self) -> Option<JobRef> {
        self.shared
            .upgrade()
            .and_then(|shared| shared.execute_job.borrow().clone())
    }
}

/// The `ActorHandler`; everything the side effects need lives in `Shared`.
pub struct DeviceActor {
    shared: Rc<Shared>,
}

type Machine = StateMachine<DeviceState, DeviceEvent, DeviceAction>;

struct Shared {
    pool_id: DevicePoolId,
    /// Weak, so a pool keeping its device actors forms no cycle. A pool that is gone
    /// has completed, and a send to it fails as one to a closed pool would.
    pool: Weak<dyn Actor<DevicePoolMessage>>,
    configuration: Arc<Configuration>,
    device: DeviceRef,
    serial: String,
    logger: String,
    log: Rc<LogQueue>,
    core: Rc<CoreExecutor>,
    hooks: Arc<dyn ReplayHooks>,
    barriers: Arc<dyn ReplayBarriers>,
    machine: RefCell<Machine>,
    initialize_job: RefCell<Option<JobRef>>,
    execute_job: RefCell<Option<JobRef>>,
    terminated_notified: Cell<bool>,
    /// The owner context from `started`: side effects launch siblings under the
    /// actor job, never under the task that happens to run the transition.
    owner: RefCell<Option<TaskContext>>,
    actor: RefCell<Option<Weak<dyn Actor<DeviceEvent>>>>,
    registration: RefCell<Option<Box<dyn CompletionRegistration>>>,
}

impl DeviceActor {
    /// The Kotlin constructor, which also starts the actor. `parent` is the pool's
    /// supervisor job (DevicePoolActor.kt:308).
    ///
    /// `pool` must be the pool actor's own long-lived handle (e.g. its
    /// `ActorContext.actor`): the device actor keeps it weak, so a temporary
    /// handle is gone by the first send and the pool looks closed.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        core: Rc<CoreExecutor>,
        pool_id: DevicePoolId,
        pool: Rc<dyn Actor<DevicePoolMessage>>,
        configuration: Arc<Configuration>,
        device: DeviceRef,
        parent: JobRef,
        hooks: Arc<dyn ReplayHooks>,
        barriers: Arc<dyn ReplayBarriers>,
    ) -> DeviceActorRef {
        let serial = device.0.info().serial_number;
        let logger = format!(
            "DevicePool[{}]_DeviceActor[{}]",
            pool_id.name.to_owned(),
            serial.to_owned()
        );
        let shared = Rc::new(Shared {
            pool_id,
            pool: Rc::downgrade(&pool),
            configuration,
            device: device.clone(),
            serial,
            logger: logger.clone(),
            log: core.log_queue(),
            core: core.clone(),
            hooks,
            barriers,
            machine: RefCell::new(machine()),
            initialize_job: RefCell::new(None),
            execute_job: RefCell::new(None),
            terminated_notified: Cell::new(false),
            owner: RefCell::new(None),
            actor: RefCell::new(None),
            registration: RefCell::new(None),
        });
        let clock: Rc<dyn SchedulerClock> = core.clock();
        #[cfg(feature = "test-support")]
        let weak = Rc::downgrade(&shared);
        let actor = RuntimeActor::with_logger(core, parent, clock, DeviceActor { shared }, "DeviceActor", &logger);
        DeviceActorRef {
            actor,
            device,
            #[cfg(feature = "test-support")]
            shared: weak,
        }
    }
}

fn machine() -> Machine {
    use DeviceEvent as E;
    use DeviceState as S;
    StateMachine::create(S::Connected, |m| {
        m.define(
            |s| matches!(s, S::Connected),
            |d| {
                d.on(
                    |e| matches!(e, E::Initialize),
                    |_, _| transition_to(S::Initializing, Some(DeviceAction::Initialize)),
                );
                d.on(
                    |e| matches!(e, E::Terminate),
                    |_, _| transition_to(S::Terminated, Some(DeviceAction::Terminate(None))),
                );
            },
        );
        m.define(
            |s| matches!(s, S::Initializing),
            |d| {
                d.on(
                    |e| matches!(e, E::Complete),
                    |_, _| transition_to(S::Ready, Some(DeviceAction::NotifyIsReady(None))),
                );
                d.on(
                    |e| matches!(e, E::Terminate),
                    |_, _| transition_to(S::Terminated, Some(DeviceAction::Terminate(None))),
                );
            },
        );
        m.define(
            |s| matches!(s, S::Ready),
            |d| {
                d.on(
                    |e| matches!(e, E::Execute(_)),
                    |s, e| match e {
                        E::Execute(batch) => {
                            let result = SharedDeferred::new();
                            transition_to(
                                S::Running(Running {
                                    test_batch: batch.clone(),
                                    result: result.clone(),
                                }),
                                Some(DeviceAction::ExecuteBatch(batch.clone(), result)),
                            )
                        }
                        _ => dont_transition(s, None),
                    },
                );
                d.on(
                    |e| matches!(e, E::Terminate),
                    |_, _| transition_to(S::Terminated, Some(DeviceAction::Terminate(None))),
                );
            },
        );
        m.define(
            |s| matches!(s, S::Running(_)),
            |d| {
                d.on(
                    |e| matches!(e, E::Terminate),
                    |s, _| transition_to(S::Terminated, Some(DeviceAction::Terminate(running(s)))),
                );
                d.on(
                    |e| matches!(e, E::Complete),
                    |s, _| transition_to(S::Ready, Some(DeviceAction::NotifyIsReady(running(s)))),
                );
            },
        );
        m.define(
            |s| matches!(s, S::Terminated),
            |d| {
                d.on(|e| matches!(e, E::Complete), |s, _| dont_transition(s, None));
                d.on(|e| matches!(e, E::Terminate), |s, _| dont_transition(s, None));
            },
        );
    })
}

fn running(state: &DeviceState) -> Option<Running> {
    match state {
        DeviceState::Running(running) => Some(running.clone()),
        _ => None,
    }
}

fn text(parts: &[&dyn AsJava]) -> String {
    let mut text = String::default();
    for part in parts {
        part.append_to(&mut text);
    }
    text
}
trait AsJava {
    fn append_to(&self, text: &mut String);
}
impl AsJava for &str {
    fn append_to(&self, text: &mut String) {
        text.push_str(self);
    }
}
impl AsJava for String {
    fn append_to(&self, text: &mut String) {
        text.push_str(&self.clone());
    }
}

/// Kotlin's `CancellationException(message)`, which completes a result cell.
fn cancellation(message: String) -> Cancellation {
    Cancellation::Requested(Failure {
        kind: FailureKind::Cancelled,
        message: Some(message),
        cause: None,
        stacktrace: None,
    })
}

/// `Deferred.cancel()` on the initialize/execute `async` jobs (S2).
fn async_job_cancelled() -> Cancellation {
    Cancellation::Requested(failure(FailureKind::JobCancelled, "DeferredCoroutine was cancelled"))
}

impl Shared {
    fn log(&self, level: Level, message: String, cause: Option<Failure>) {
        self.log
            .log(&self.logger, self.core.clock().now_millis(), level, message, cause);
    }

    fn pool_try_send(&self, message: FromDevice) -> bool {
        self.pool
            .upgrade()
            .is_some_and(|pool| pool.try_send(DevicePoolMessage::FromDevice(message)).is_ok())
    }

    /// Kotlin transitions inside a reentrant `synchronized` block. The Rust machine
    /// is borrowed only for the transition itself; side effects run afterwards, so
    /// a job they cancel can complete and re-enter here synchronously.
    fn transition(self: &Rc<Self>, event: DeviceEvent) {
        let transition = self.machine.borrow_mut().transition(event);
        match transition {
            Ok(Transition::Valid {
                side_effect: Some(effect),
                ..
            }) => self.side_effect(effect),
            Ok(Transition::Invalid { from_state, event }) => self.log(
                Level::Error,
                format!("Invalid transition from {from_state} event {event}"),
                None,
            ),
            // Every state has a definition, so the machine never reports a missing one.
            Ok(Transition::Valid { side_effect: None, .. }) | Err(_) => {}
        }
    }

    fn side_effect(self: &Rc<Self>, effect: DeviceAction) {
        match effect {
            DeviceAction::Initialize => self.initialize(),
            DeviceAction::NotifyIsReady(execution) => {
                if let Some(execution) = execution {
                    self.account_for(&execution, text(&[&"Device ", &self.serial, &" reported no results"]));
                }
                self.notify_is_ready();
            }
            DeviceAction::ExecuteBatch(batch, result) => self.execute_batch(batch, result),
            DeviceAction::Terminate(execution) => {
                if let Some(execution) = execution {
                    self.account_for(&execution, text(&[&"Device ", &self.serial, &" terminated"]));
                }
                self.terminate();
            }
        }
    }

    /// Exactly one transition leaves Running: seal the result, then send whichever
    /// completion won, synchronously, so neither Terminated nor a later IsReady
    /// can overtake it (`DeviceActor.kt:131–152`).
    fn account_for(&self, execution: &Running, reason: String) {
        execution.result.cancel(cancellation(reason.clone()));
        let outcome = match execution.result.completed() {
            Some(Ok(results)) => FromDevice::CompletedTestBatch(self.device.clone(), TestBatchResults::clone(&results)),
            completion => {
                let reason = completion
                    .and_then(|c| c.err())
                    .and_then(|error| error.failure().message.to_owned())
                    .unwrap_or(reason);
                self.log(
                    Level::Debug,
                    text(&[&"Returning batch ", &execution.test_batch.id, &". Reason: ", &reason]),
                    None,
                );
                FromDevice::ReturnTestBatch(self.device.clone(), execution.test_batch.clone(), reason)
            }
        };
        if !self.pool_try_send(outcome) {
            self.log(
                Level::Warn,
                text(&[
                    &"Pool is closed, outcome of batch ",
                    &execution.test_batch.id,
                    &" is dropped",
                ]),
                None,
            );
        }
    }

    /// Launches under the owner (actor) job; a cancelled owner never runs the body.
    fn launch<F: Future<Output = OperationResult<()>> + 'static>(
        &self,
        body: impl FnOnce(TaskContext) -> F + 'static,
    ) -> Option<JobRef> {
        let owner = self.owner.borrow().clone()?;
        Some(
            owner
                .core
                .spawn(owner.job, Box::new(move |context| Box::pin(body(context)))),
        )
    }

    /// A suspending send under the owner: lost if the owner is cancelled first.
    fn notify_is_ready(self: &Rc<Self>) {
        let shared = self.clone();
        self.launch(move |_| async move {
            let message = DevicePoolMessage::FromDevice(FromDevice::IsReady(shared.device.clone()));
            let pool = shared.pool.upgrade();
            match pool {
                Some(pool) => match pool.send(message).await {
                    Ok(()) => Ok(()),
                    // A channel closed without a cause throws ClosedSendChannelException,
                    // which fails this task and with it the actor (oracle "readiness to a
                    // closed pool"). Pool actors are only ever cancelled, so in a run
                    // the send throws their cancellation, which ends this task silently.
                    Err(SendError {
                        cause: Some(failure), ..
                    }) => Err(failure.into()),
                    Err(SendError { cause: None, .. }) => Err(cancellation("Channel was closed".into()).into()),
                },
                None => Err(cancellation("Channel was closed".into()).into()),
            }
        });
    }

    fn initialize(self: &Rc<Self>) {
        self.log(Level::Debug, text(&[&"initialize ", &self.serial]), None);
        let shared = self.clone();
        let job = self.launch(move |context| async move {
            let attempts = shared.clone();
            let attempt_context = context.clone();
            let result = with_retry(30, 10_000, &*context.clock, context.job.clone(), move || {
                attempts.clone().prepare_attempt(attempt_context.clone())
            })
            .await;
            match result {
                Ok(()) => shared.transition(DeviceEvent::Complete),
                // `catch (e: Exception)` also catches this task's own cancellation (K1)
                Err(error) => {
                    let failure = match error {
                        OperationError::Cancelled(reason) => reason.failure().clone(),
                        OperationError::Failed(failure) => failure,
                    };
                    let message = failure.message.to_owned().unwrap_or_else(|| "null".into());
                    shared.log(Level::Error, text(&[&"Error ", &message]), Some(failure));
                    shared.transition(DeviceEvent::Terminate);
                }
            }
            Ok(())
        });
        *self.initialize_job.borrow_mut() = job;
    }

    async fn prepare_attempt(self: Rc<Self>, context: TaskContext) -> OperationResult<()> {
        // `if (isActive)`: an inactive task "succeeds" without preparing. Unreachable
        // in practice, since the retry delay already throws on cancellation.
        if context.job.cancellation_signal().reason().is_some() {
            return Ok(());
        }
        let configuration = self.configuration.clone();
        let outcome = self
            .vendor_call(PREPARE, context.job.cancellation_signal(), move |device, vendor| {
                Box::pin(async move { device.prepare(configuration, vendor).await })
            })
            .await;
        match outcome {
            Ok(()) => Ok(()),
            Err(VendorError::Cancelled(reason)) => Err(reason.into()),
            // NotBooted/NotAdmitted are CancellationException subclasses: never retried.
            Err(VendorError::LeavePool { detail, .. }) => Err(Cancellation::Requested(detail).into()),
            // S3: every other error and every panic is an Exception, retried and
            // logged on each attempt, including the last (K2).
            Err(error) => {
                let failure = error.failure().clone();
                self.log(
                    Level::Debug,
                    text(&[&"device ", &self.serial, &" initialization failed. Retrying"]),
                    Some(failure.clone()),
                );
                Err(failure.into())
            }
        }
    }

    fn execute_batch(self: &Rc<Self>, batch: BatchRef, result: Arc<SharedDeferred<TestBatchResults>>) {
        self.log(Level::Debug, text(&[&"executeBatch ", &self.serial]), None);
        let shared = self.clone();
        let job = self.launch(move |context| async move {
            let (configuration, pool_id) = (shared.configuration.clone(), shared.pool_id.clone());
            let cell: Arc<dyn Deferred<TestBatchResults>> = result.clone();
            let outcome = shared
                .vendor_call(EXECUTE, context.job.cancellation_signal(), move |device, vendor| {
                    Box::pin(async move { device.execute(configuration, pool_id, batch, cell, vendor).await })
                })
                .await;
            // The catch order of DeviceActor.kt:190–207, over what the vendor returned.
            match outcome {
                Ok(()) => shared.transition(DeviceEvent::Complete),
                Err(error @ (VendorError::Cancelled(_) | VendorError::LeavePool { .. })) => {
                    let failure = error.failure().clone();
                    shared.log(Level::Warn, "Device execution has been cancelled".into(), Some(failure));
                    shared.transition(DeviceEvent::Terminate);
                }
                Err(VendorError::DeviceLost(failure)) => {
                    shared.log(Level::Error, "Critical error during execution".into(), Some(failure));
                    shared.transition(DeviceEvent::Terminate);
                }
                Err(VendorError::BatchExecution(failure)) => {
                    shared.log(Level::Warn, "Test batch failed execution".into(), Some(failure.clone()));
                    result.cancel(cancellation(text(&[
                        &"Test batch failed execution:\n",
                        &failure.stack_trace(),
                    ])));
                    shared.transition(DeviceEvent::Complete);
                }
                Err(error) => {
                    let failure = error.failure().clone();
                    shared.log(
                        Level::Error,
                        "Unknown vendor exception caught. Considering this a recoverable error".into(),
                        Some(failure.clone()),
                    );
                    result.cancel(cancellation(text(&[
                        &"Unknown vendor exception caught:\n",
                        &failure.stack_trace(),
                    ])));
                    shared.transition(DeviceEvent::Complete);
                }
            }
            Ok(())
        });
        *self.execute_job.borrow_mut() = job;
    }

    /// The live Android barrier around one vendor call (patch 0002): admission is
    /// cancellable and leaves the token unconsumed; release follows the vendor's
    /// own outcome. A barrier error fails the call like a vendor exception.
    async fn vendor_call(
        &self,
        label: &'static str,
        cancellation: Arc<dyn CancellationSignal>,
        call: impl FnOnce(Arc<dyn Device>, VendorContext) -> VendorFuture<'static, ()> + Send + 'static,
    ) -> VendorResult<()> {
        let permit = match race(cancellation.cancelled(), self.barriers.enter(label, &self.serial)).await {
            Either::Left(reason) => return Err(VendorError::Cancelled(reason)),
            Either::Right(Err(error)) => {
                return Err(VendorError::Other(Failure {
                    kind: FailureKind::Replay,
                    message: Some(error.message),
                    cause: None,
                    stacktrace: None,
                }));
            }
            Either::Right(Ok(permit)) => permit,
        };
        let device = self.device.0.clone();
        let (clock, hooks) = (self.core.vendor_clock(), self.hooks.clone());
        let outcome = call_vendor(&*self.core.vendor(), cancellation, move |cancellation| {
            call(
                device,
                VendorContext {
                    cancellation,
                    clock,
                    hooks,
                },
            )
        })
        .await;
        // A failed release is recorded by the hooks and reported by verify_consumed.
        let _ = permit.finish();
        outcome
    }

    /// Cancellable joins, as Kotlin's cancelAndJoin is cancel + join (`:211–219`).
    fn terminate(self: &Rc<Self>) {
        self.log(Level::Debug, text(&[&"terminate ", &self.serial]), None);
        let shared = self.clone();
        self.launch(move |context| async move {
            for slot in [&shared.initialize_job, &shared.execute_job] {
                let job = slot.borrow().clone();
                if let Some(job) = job {
                    job.cancel(async_job_cancelled());
                    job.join_in(context.job.clone()).await?;
                }
            }
            shared.notify_terminated();
            let actor = shared.actor.borrow().as_ref().and_then(Weak::upgrade);
            if let Some(actor) = actor {
                actor.close();
            }
            Ok(())
        });
    }

    /// Once only; a closed pool is ignored without a log (`:221–225`).
    fn notify_terminated(&self) {
        if !self.terminated_notified.replace(true) {
            self.pool_try_send(FromDevice::Terminated(self.device.clone()));
        }
    }
}

impl ActorHandler<DeviceEvent> for DeviceActor {
    fn started(&mut self, context: ActorContext<DeviceEvent>) {
        *self.shared.actor.borrow_mut() = Some(Rc::downgrade(&context.actor));
        let owner = context.task.job.clone();
        // Stored first: a completed owner runs the callback, and its terminate, now.
        *self.shared.owner.borrow_mut() = Some(context.task);
        // However the actor stops, a batch still Running is accounted for and the
        // pool receives Terminated exactly once (`:111–118`). Weak: the callback is
        // owned by the actor's own job.
        let shared = Rc::downgrade(&self.shared);
        let registration = owner.invoke_on_completion(Box::new(move |_| {
            if let Some(shared) = shared.upgrade() {
                shared.transition(DeviceEvent::Terminate);
                shared.notify_terminated();
            }
        }));
        *self.shared.registration.borrow_mut() = Some(registration);
    }

    fn receive(
        &mut self,
        message: DeviceEvent,
        _context: ActorContext<DeviceEvent>,
    ) -> LocalFuture<'_, OperationResult<()>> {
        let shared = self.shared.clone();
        Box::pin(async move {
            shared.transition(message);
            Ok(())
        })
    }
}
