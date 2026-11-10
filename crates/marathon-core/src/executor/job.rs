//! Port of core/src/main/kotlin/com/malinskiy/marathon/actor/Actor.kt
//! and execution/Scheduler.kt (Job ownership and completion).
use super::shared::*;
use crate::{error::*, runtime::*};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::{Rc, Weak},
    sync::Arc,
};
use tokio::sync::Notify;

type Callback = Box<dyn FnOnce(Completion)>;
type CallbackQueue = Rc<RefCell<VecDeque<Callback>>>;
pub(crate) type FailureHandler = Box<dyn FnOnce(Failure)>;

/// A sealed outcome and its taken FIFO callbacks. Membership is removed through
/// every ready ancestor before any of these packets can invoke user code.
struct CompletionPacket {
    job: Rc<RuntimeJob>,
    outcome: Completion,
    callbacks: CallbackQueue,
}
/// Explicit stacks bound executor stack use even for very deep job trees.
/// User callbacks may reenter through public methods, without any inner borrow.
enum Step {
    Cancel(Rc<RuntimeJob>, Cancellation),
    ChildFailed(Rc<RuntimeJob>, Failure),
    FinishBody(Rc<RuntimeJob>),
    Prepare(CompletionPacket),
}
fn cascade(first: Step) {
    run_stack(vec![first]);
}
fn invoke(callback: Callback, outcome: Completion) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(outcome)));
}
fn run_stack(mut stack: Vec<Step>) {
    let mut ready = Vec::new();
    while let Some(step) = stack.pop() {
        match step {
            Step::Cancel(job, reason) => job.cancel_step(reason, &mut stack),
            Step::ChildFailed(job, failure) => job.child_failed_step(failure, &mut stack),
            Step::FinishBody(job) => {
                let packet = {
                    let mut inner = job.inner.borrow_mut();
                    inner.body_finished = true;
                    inner.seal(job.clone())
                };
                if let Some(packet) = packet {
                    stack.push(Step::Prepare(packet));
                }
            }
            Step::Prepare(packet) => {
                let mut next = Some(packet);
                while let Some(packet) = next {
                    next = packet.job.parent.as_ref().and_then(Weak::upgrade).and_then(|parent| {
                        let mut inner = parent.inner.borrow_mut();
                        let index = inner.children.iter().position(|child| Rc::ptr_eq(child, &packet.job))?;
                        inner.children.remove(index);
                        // D65: removal, completion and callback takeover share a borrow.
                        inner.seal(parent.clone())
                    });
                    ready.push(packet);
                }
            }
        }
    }
    // Complete the synchronous transition cascade before user code can attach
    // work. Every ready ancestor has detached and sealed; packets stay child first.
    for packet in ready {
        let handler = packet.job.handler.borrow_mut().take();
        if let (Some(handler), Completion::Failed(failure)) = (handler, &packet.outcome)
            && packet.job.reports_failure()
        {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(failure.clone())));
        }
        packet.job.changed.notify_waiters();
        loop {
            let callback = packet.callbacks.borrow_mut().pop_front();
            let Some(callback) = callback else { break };
            invoke(callback, packet.outcome.clone());
        }
        packet.job.inner.borrow_mut().pending_callbacks = None;
    }
}

struct Inner {
    body_finished: bool,
    outcome: Completion,
    completed: bool,
    children: Vec<Rc<RuntimeJob>>,
    callbacks: VecDeque<Callback>,
    pending_callbacks: Option<CallbackQueue>,
}
impl Inner {
    fn seal(&mut self, job: Rc<RuntimeJob>) -> Option<CompletionPacket> {
        if self.completed || !self.body_finished || !self.children.is_empty() {
            return None;
        }
        self.completed = true;
        let callbacks = Rc::new(RefCell::new(std::mem::take(&mut self.callbacks)));
        self.pending_callbacks = Some(callbacks.clone());
        Some(CompletionPacket {
            job,
            outcome: self.outcome.clone(),
            callbacks,
        })
    }
}
pub struct RuntimeJob {
    myself: Weak<Self>,
    kind: JobKind,
    task: bool,
    parent_handles_failure: bool,
    pub(crate) scoped: Cell<bool>,
    parent: Option<Weak<RuntimeJob>>,
    inner: RefCell<Inner>,
    signal: Arc<Signal>,
    changed: Notify,
    /// The nearest actor scope supplies the coroutine exception handler's logger.
    pub(crate) handler_logger: RefCell<Option<Rc<str>>>,
    /// A launched task's handler runs before its own completion callbacks.
    pub(crate) handler: RefCell<Option<FailureHandler>>,
}
// An abandoned active tree has no completion cascade to release its ownership.
// Drain uniquely owned descendants before dropping each job, bounding Drop depth.
impl Drop for RuntimeJob {
    fn drop(&mut self) {
        let mut children = std::mem::take(&mut self.inner.get_mut().children);
        while let Some(child) = children.pop() {
            if let Ok(mut child) = Rc::try_unwrap(child) {
                children.append(&mut child.inner.get_mut().children);
            }
        }
    }
}
impl RuntimeJob {
    pub(super) fn new(kind: JobKind, parent: Option<Rc<Self>>, task: bool) -> Rc<Self> {
        let job = Rc::new_cyclic(|myself| Self {
            myself: myself.clone(),
            kind,
            task,
            parent_handles_failure: parent.as_ref().is_some_and(|parent| {
                parent.kind != JobKind::Supervisor
                    && (parent.task || parent.scoped.get() || parent.parent_handles_failure)
            }),
            scoped: Cell::new(false),
            parent: parent.as_ref().map(Rc::downgrade),
            signal: Signal::new(),
            changed: Notify::new(),
            handler_logger: RefCell::new(
                parent
                    .as_ref()
                    .and_then(|parent| parent.handler_logger.borrow().clone()),
            ),
            handler: RefCell::new(None),
            inner: RefCell::new(Inner {
                body_finished: false,
                outcome: Completion::Success,
                completed: false,
                children: Vec::new(),
                callbacks: VecDeque::new(),
                pending_callbacks: None,
            }),
        });
        if let Some(parent) = parent {
            let reason = {
                let mut inner = parent.inner.borrow_mut();
                if inner.completed {
                    Some(requested("Parent job is completed"))
                } else {
                    inner.children.push(job.clone());
                    parent.signal.reason()
                }
            };
            if let Some(reason) = reason {
                job.cancel(reason);
            }
        }
        job
    }
    fn rc(&self) -> Rc<Self> {
        match self.myself.upgrade() {
            Some(job) => job,
            None => unreachable!("a job method requires a live receiver"),
        }
    }
    pub(crate) fn finish_body(&self, result: OperationResult<()>) {
        let outcome = {
            let mut inner = self.inner.borrow_mut();
            match result {
                Err(OperationError::Failed(f)) if !matches!(inner.outcome, Completion::Failed(_)) => {
                    inner.outcome = Completion::Failed(f)
                }
                Err(OperationError::Cancelled(c)) if matches!(inner.outcome, Completion::Success) => {
                    inner.outcome = Completion::Cancelled(c)
                }
                _ => {}
            }
            inner.outcome.clone()
        };
        let this = self.rc();
        // Propagate the winning failure and signal cancellation before freezing any
        // ancestor outcome. FinishBody then marks/seals in the same borrow.
        let mut stack = vec![Step::FinishBody(this.clone())];
        match outcome {
            Completion::Cancelled(reason) => stack.push(Step::Cancel(this, reason)),
            Completion::Failed(f) => {
                stack.push(Step::Cancel(this, Cancellation::Requested(f.clone())));
                if !self.scoped.get()
                    && let Some(parent) = self.parent.as_ref().and_then(Weak::upgrade)
                {
                    stack.push(Step::ChildFailed(parent, f));
                }
            }
            Completion::Success => {}
        }
        run_stack(stack);
    }
    fn child_failed_step(&self, failure: Failure, stack: &mut Vec<Step>) {
        if self.kind == JobKind::Supervisor {
            return;
        }
        {
            let mut inner = self.inner.borrow_mut();
            if inner.completed {
                return;
            }
            if !matches!(inner.outcome, Completion::Failed(_)) {
                inner.outcome = Completion::Failed(failure.clone());
            }
        }
        stack.push(Step::Cancel(self.rc(), Cancellation::Requested(failure.clone())));
        if !self.scoped.get()
            && let Some(parent) = self.parent.as_ref().and_then(Weak::upgrade)
        {
            stack.push(Step::ChildFailed(parent, failure));
        }
    }
    fn cancel_step(&self, reason: Cancellation, stack: &mut Vec<Step>) {
        if self.inner.borrow().completed || !self.signal.cancel(reason.clone()) {
            return;
        }
        let packet = {
            let mut inner = self.inner.borrow_mut();
            if matches!(inner.outcome, Completion::Success) {
                inner.outcome = Completion::Cancelled(reason.clone());
            }
            if !self.task {
                inner.body_finished = true;
            }
            inner.seal(self.rc())
        };
        if let Some(packet) = packet {
            stack.push(Step::Prepare(packet));
        }
        for child in self.children_snapshot().into_iter().rev() {
            stack.push(Step::Cancel(child, reason.clone()));
        }
    }
    pub(crate) fn reports_failure(&self) -> bool {
        // Ownership is frozen on attachment; a completed parent may disappear
        // before a launched task emits its handler record.
        !self.scoped.get() && !self.parent_handles_failure
    }
    /// A cancellable join needs its caller; join() alone is protected.
    pub fn join_in(&self, caller: JobRef) -> LocalFuture<'_, Result<(), Cancellation>> {
        Box::pin(async move {
            let signal = caller.cancellation_signal();
            match crate::executor::shared::race(signal.cancelled(), self.join()).await {
                futures_util::future::Either::Left(reason) => Err(reason),
                futures_util::future::Either::Right(joined) => joined,
            }
        })
    }
    /// Explicit cancellation observation, before body/child completion.
    pub fn cancellation_signal(&self) -> Arc<dyn CancellationSignal> {
        self.signal.clone()
    }
    pub fn state(&self) -> JobState {
        let inner = self.inner.borrow();
        if inner.completed {
            JobState::Completed(inner.outcome.clone())
        } else if let Some(reason) = self.signal.reason() {
            JobState::Cancelling(reason)
        } else if inner.body_finished {
            JobState::Completing
        } else {
            JobState::Active
        }
    }
    pub fn cancel(&self, reason: Cancellation) {
        cascade(Step::Cancel(self.rc(), reason));
    }
    pub fn complete(&self) -> bool {
        if self.task || self.inner.borrow().body_finished {
            return false;
        }
        cascade(Step::FinishBody(self.rc()));
        true
    }
    pub fn join(&self) -> LocalFuture<'_, Result<(), Cancellation>> {
        Box::pin(async move {
            loop {
                let changed = self.changed.notified();
                let mut changed = std::pin::pin!(changed);
                changed.as_mut().enable();
                if self.inner.borrow().completed {
                    return Ok(());
                }
                changed.await;
            }
        })
    }
    pub fn children_snapshot(&self) -> Vec<JobRef> {
        self.inner.borrow().children.clone()
    }
    /// Registrations join the pending FIFO through dispatch, including after the
    /// seal. Only fully dispatched jobs invoke immediately (D64, D65).
    pub fn invoke_on_completion(&self, callback: Callback) {
        let immediate = {
            let mut inner = self.inner.borrow_mut();
            if let Some(pending) = &inner.pending_callbacks {
                pending.borrow_mut().push_back(callback);
                None
            } else if inner.completed {
                Some((callback, inner.outcome.clone()))
            } else {
                inner.callbacks.push_back(callback);
                None
            }
        };
        if let Some((callback, outcome)) = immediate {
            invoke(callback, outcome);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_handler_panic_does_not_abandon_ready_packets_d65() {
        let parent = RuntimeJob::new(JobKind::Ordinary, None, false);
        let child = RuntimeJob::new(JobKind::Ordinary, Some(parent.clone()), true);
        let seen = Rc::new(RefCell::new(Vec::new()));
        let handler_called = Rc::new(Cell::new(false));
        let called = handler_called.clone();
        *child.handler.borrow_mut() = Some(Box::new(move |_| {
            called.set(true);
            panic!("handler probe");
        }));
        let recorded = seen.clone();
        child.invoke_on_completion(Box::new(move |_| recorded.borrow_mut().push("child")));
        let recorded = seen.clone();
        parent.invoke_on_completion(Box::new(move |_| recorded.borrow_mut().push("parent")));
        parent.complete();
        let cause = failure(marathon_error::FailureKind::IllegalState, "failed child");
        child.finish_body(Err(cause.clone().into()));
        assert!(handler_called.get());
        assert_eq!(*seen.borrow(), ["child", "parent"]);
        assert_eq!(child.state(), JobState::Completed(Completion::Failed(cause.clone())));
        assert_eq!(parent.state(), JobState::Completed(Completion::Failed(cause)));
        assert!(parent.inner.borrow().pending_callbacks.is_none());
        assert!(child.inner.borrow().pending_callbacks.is_none());
    }
}
