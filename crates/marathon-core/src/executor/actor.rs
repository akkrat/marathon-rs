//! Port of core/src/main/kotlin/com/malinskiy/marathon/actor/Actor.kt
//! and actor/extensions.kt.
use super::{CoreExecutor, shared::*};
use crate::{error::*, runtime::*, time::SchedulerClock};
use futures_util::FutureExt;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};
use tokio::sync::Notify;

pub struct RuntimeActor<M> {
    identity: std::rc::Weak<Self>,
    job: JobRef,
    receiver: JobRef,
    closed: Cell<bool>,
    queue: RefCell<VecDeque<M>>,
    changed: Notify,
    handler: RefCell<Option<Box<dyn ActorHandler<M>>>>,
}
impl<M: 'static> RuntimeActor<M> {
    /// Every replay actor must have a label/probe, including actors with no sends.
    pub fn new(
        core: Rc<CoreExecutor>,
        parent: JobRef,
        clock: Rc<dyn SchedulerClock>,
        handler: impl ActorHandler<M> + 'static,
        label: &'static str,
    ) -> Rc<Self> {
        Self::spawn(core, parent, clock, handler, label, None)
    }
    /// As `new`, with Actor.kt's `logger`: tasks launched in the actor's scope
    /// report an uncaught failure under it (`newCoroutineExceptionHandler(logger)`).
    pub fn with_logger(
        core: Rc<CoreExecutor>,
        parent: JobRef,
        clock: Rc<dyn SchedulerClock>,
        handler: impl ActorHandler<M> + 'static,
        label: &'static str,
        logger: &str,
    ) -> Rc<Self> {
        Self::spawn(core, parent, clock, handler, label, Some(logger.into()))
    }
    fn spawn(
        core: Rc<CoreExecutor>,
        parent: JobRef,
        clock: Rc<dyn SchedulerClock>,
        handler: impl ActorHandler<M> + 'static,
        label: &'static str,
        logger: Option<Rc<str>>,
    ) -> Rc<Self> {
        // Actor.kt owns Job(parent); the builder adds a receiver child. A
        // cancelled receive closes that child without completing the owner.
        let job = core.new_job(JobKind::Ordinary, Some(parent), false);
        if logger.is_some() {
            *job.handler_logger.borrow_mut() = logger;
        }
        let receiver = core.new_job(JobKind::Ordinary, Some(job.clone()), true);
        let actor = Rc::new_cyclic(|identity| Self {
            identity: identity.clone(),
            job: job.clone(),
            receiver: receiver.clone(),
            closed: Cell::new(false),
            queue: RefCell::new(VecDeque::new()),
            changed: Notify::new(),
            handler: RefCell::new(Some(Box::new(handler))),
        });
        core.mailboxes
            .borrow_mut()
            .push((label, Rc::new(ActorProbe(Rc::downgrade(&actor)))));
        let context = ActorContext {
            task: TaskContext {
                job: job.clone(),
                core: core.clone(),
                clock: clock.clone(),
            },
            actor: actor.clone(),
        };
        // started can install a weak own-job callback even on a cancelled parent.
        let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(handler) = actor.handler.borrow_mut().as_mut() {
                handler.started(context);
            }
        }));
        let active = actor.clone();
        let keepalive = actor.clone();
        // The owner can outlive its receiver. Keep bookkeeping through owner
        // completion without a strong own-job callback cycle.
        let owner = job.clone();
        tokio::task::spawn_local(super::tracking::Tracked::new(
            async move {
                owner.join().await.ok();
                keepalive.queue.borrow_mut().clear();
                keepalive.handler.borrow_mut().take();
            },
            core.tracker.clone(),
        ));
        let ended = actor.clone();
        core.launch_retained(
            receiver,
            Box::new(move |task| {
                Box::pin(async move {
                    if let Err(panic) = started {
                        return Err(super::panic_failure(panic).into());
                    }
                    let mut handler = match active.handler.borrow_mut().take() {
                        Some(h) => h,
                        None => return Ok(()),
                    };
                    let result = match std::panic::AssertUnwindSafe(active.receive_loop(&mut *handler, task, clock))
                        .catch_unwind()
                        .await
                    {
                        Ok(result) => result,
                        Err(payload) => Err(super::panic_failure(payload).into()),
                    };
                    *active.handler.borrow_mut() = Some(handler);
                    result
                })
            }),
            Box::new(move || {
                ended.closed.set(true);
                ended.queue.borrow_mut().clear();
            }),
        );
        actor
    }
    async fn receive_loop(
        &self,
        handler: &mut dyn ActorHandler<M>,
        task: TaskContext,
        clock: Rc<dyn SchedulerClock>,
    ) -> OperationResult<()> {
        loop {
            let changed = self.changed.notified();
            let mut changed = std::pin::pin!(changed);
            changed.as_mut().enable();
            if let Some(reason) = self.receiver.cancellation_signal().reason() {
                self.queue.borrow_mut().clear();
                return Err(reason.into());
            }
            let message = self.queue.borrow_mut().pop_front();
            if let Some(message) = message {
                let self_handle = self.self_handle();
                handler
                    .receive(
                        message,
                        ActorContext {
                            task: TaskContext {
                                job: task.job.clone(),
                                core: task.core.clone(),
                                clock: clock.clone(),
                            },
                            actor: self_handle,
                        },
                    )
                    .await?;
            } else {
                let signal = self.receiver.cancellation_signal();
                if let futures_util::future::Either::Left(reason) = race(signal.cancelled(), changed.as_mut()).await {
                    self.queue.borrow_mut().clear();
                    return Err(reason.into());
                }
            }
        }
    }
    fn self_handle(&self) -> Rc<dyn Actor<M>> {
        // Replaced at construction by the weak identity, never a strong cycle.
        match self.identity.upgrade() {
            Some(actor) => actor,
            None => unreachable!("actor is retained until completion"),
        }
    }
}
impl<M: 'static> Actor<M> for RuntimeActor<M> {
    fn job(&self) -> JobRef {
        self.job.clone()
    }
    fn is_closed_for_send(&self) -> bool {
        self.closed.get()
            || self.receiver.cancellation_signal().reason().is_some()
            || matches!(self.receiver.state(), JobState::Completed(_))
    }
    fn try_send(&self, message: M) -> Result<(), SendError<M>> {
        if self.is_closed_for_send() {
            Err(SendError { message, cause: None })
        } else {
            self.queue.borrow_mut().push_back(message);
            self.changed.notify_waiters();
            Ok(())
        }
    }
    fn send(&self, message: M) -> LocalFuture<'_, Result<(), SendError<M>>> {
        Box::pin(async move { self.try_send(message) })
    }
    fn safe_send(&self, message: M) -> LocalFuture<'_, Result<(), SendError<M>>> {
        // Check at call time, send when polled: closure between them can fail.
        let closed = self.is_closed_for_send();
        Box::pin(async move { if closed { Ok(()) } else { self.send(message).await } })
    }
    fn close(&self) {
        self.closed.set(true);
        self.queue.borrow_mut().clear();
        self.job.cancel(requested("Actor was cancelled"));
        self.changed.notify_waiters();
    }
}
impl<M: 'static> MailboxProbe for RuntimeActor<M> {
    fn state(&self) -> MailboxState {
        let closed = self.is_closed_for_send();
        MailboxState {
            queued: if closed { 0 } else { self.queue.borrow().len() },
            closed_for_send: closed,
            discarded: closed,
        }
    }
}

struct ActorProbe<M>(std::rc::Weak<RuntimeActor<M>>);
impl<M: 'static> MailboxProbe for ActorProbe<M> {
    fn state(&self) -> MailboxState {
        self.0.upgrade().map_or(
            MailboxState {
                queued: 0,
                closed_for_send: true,
                discarded: true,
            },
            |actor| MailboxProbe::state(actor.as_ref()),
        )
    }
}
