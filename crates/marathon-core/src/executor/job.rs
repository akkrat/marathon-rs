//! Port of core/src/main/kotlin/com/malinskiy/marathon/actor/Actor.kt
//! and execution/Scheduler.kt (Job ownership and children traversal).
use super::shared::*;
use crate::{error::*, runtime::*};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    sync::Arc,
};
use tokio::sync::Notify;
enum Callback {
    User(Box<dyn FnOnce(Completion)>),
    /// JobSupport's ChildCompletion: the waiting parent continues completing.
    Parent(Weak<RuntimeJob>),
}
pub(crate) type FailureHandler = Box<dyn FnOnce(Failure)>;
struct Registration {
    callback: RefCell<Option<Callback>>,
}
struct RegistrationHandle {
    registration: Rc<Registration>,
    owner: Weak<RuntimeJob>,
    node: Weak<Node>,
}
impl CompletionRegistration for RegistrationHandle {
    fn dispose(&self) {
        self.registration.callback.borrow_mut().take();
        if let (Some(owner), Some(node)) = (self.owner.upgrade(), self.node.upgrade()) {
            owner.remove_node(&node);
        }
    }
}
struct Node {
    job: Option<Rc<RuntimeJob>>,
    next: RefCell<Option<Rc<Node>>>,
}
/// The default drop of a long list recurses once per node; 100,000 completion
/// callbacks on one job overflowed the core thread's stack. Unlink iteratively;
/// a node still held elsewhere keeps its tail.
impl Drop for Node {
    fn drop(&mut self) {
        let mut next = self.next.get_mut().take();
        while let Some(node) = next {
            next = match Rc::try_unwrap(node) {
                Ok(mut node) => node.next.get_mut().take(),
                Err(_) => None,
            };
        }
    }
}
/// One unit of the cancel/fail/complete cascade. The cascade runs on an explicit
/// stack so deep job trees cannot exhaust the thread stack. Steps are pushed in
/// reverse, so they run depth first, in exactly the order of the JVM's recursion
/// (early child callback, then the parent's completion, then later callbacks).
enum Step {
    Cancel(Rc<RuntimeJob>, Cancellation),
    ChildFailed(Rc<RuntimeJob>, Failure),
    TryFinish(Rc<RuntimeJob>),
    Callback(Rc<Registration>, Completion),
}
/// Entry points start a cascade here. A user callback that starts another cascade
/// gets its own stack, so only user nesting, not tree depth, uses the thread stack.
fn cascade(first: Step) {
    run_stack(vec![first]);
}
fn run_stack(mut stack: Vec<Step>) {
    while let Some(step) = stack.pop() {
        match step {
            Step::Cancel(job, reason) => job.cancel_step(reason, &mut stack),
            Step::ChildFailed(job, failure) => job.child_failed_step(failure, &mut stack),
            Step::TryFinish(job) => job.try_finish_step(&mut stack),
            Step::Callback(registration, outcome) => {
                let callback = registration.callback.borrow_mut().take();
                match callback {
                    Some(Callback::User(callback)) => {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(outcome)));
                    }
                    Some(Callback::Parent(parent)) => {
                        if let Some(parent) = parent.upgrade() {
                            stack.push(Step::TryFinish(parent));
                        }
                    }
                    None => {}
                }
            }
        }
    }
}
struct Inner {
    body_finished: bool,
    outcome: Completion,
    completed: bool,
    children: usize,
    list_mode: bool,
    head: Option<Rc<Node>>,
    tail: Option<Weak<Node>>,
    callbacks: Vec<Rc<Registration>>,
    waiting_child: Option<Weak<RuntimeJob>>,
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
    /// The logger of the nearest actor scope: Kotlin's Actor puts
    /// `newCoroutineExceptionHandler(logger)` in the context its coroutines inherit.
    pub(crate) handler_logger: RefCell<Option<Rc<str>>>,
    /// A launched task's coroutine exception handler, run once at completion.
    pub(crate) handler: RefCell<Option<FailureHandler>>,
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
                children: 0,
                list_mode: false,
                head: None,
                tail: None,
                callbacks: Vec::new(),
                waiting_child: None,
            }),
        });
        if let Some(parent) = parent {
            let reason = if parent.inner.borrow().completed {
                Some(requested("Parent job is completed"))
            } else {
                parent.append(Rc::new(Node {
                    job: Some(job.clone()),
                    next: RefCell::new(None),
                }));
                parent.signal.reason()
            };
            if let Some(reason) = reason {
                job.cancel(reason);
            }
        }
        job
    }
    fn append(&self, node: Rc<Node>) {
        let mut inner = self.inner.borrow_mut();
        if inner.head.is_some() {
            inner.list_mode = true;
        }
        if node.job.is_some() {
            inner.children += 1;
        }
        if let Some(tail) = inner.tail.as_ref().and_then(Weak::upgrade) {
            *tail.next.borrow_mut() = Some(node.clone());
        } else {
            inner.head = Some(node.clone());
        }
        inner.tail = Some(Rc::downgrade(&node));
    }
    fn rc(&self) -> Rc<Self> {
        match self.myself.upgrade() {
            Some(job) => job,
            None => unreachable!("a job method requires a live receiver"),
        }
    }
    pub(crate) fn finish_body(&self, result: OperationResult<()>) {
        {
            let mut inner = self.inner.borrow_mut();
            inner.body_finished = true;
            match result {
                Err(OperationError::Failed(f)) if !matches!(inner.outcome, Completion::Failed(_)) => {
                    inner.outcome = Completion::Failed(f)
                }
                Err(OperationError::Cancelled(c)) if matches!(inner.outcome, Completion::Success) => {
                    inner.outcome = Completion::Cancelled(c)
                }
                _ => {}
            }
        }
        let outcome = self.inner.borrow().outcome.clone();
        let this = self.rc();
        let mut stack = vec![Step::TryFinish(this.clone())];
        match outcome {
            Completion::Cancelled(reason) => stack.push(Step::Cancel(this, reason)),
            Completion::Failed(f) => {
                stack.push(Step::Cancel(this, Cancellation::Parent(f.clone())));
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
        stack.push(Step::Cancel(self.rc(), Cancellation::Parent(failure.clone())));
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
        {
            let mut inner = self.inner.borrow_mut();
            if matches!(inner.outcome, Completion::Success) {
                inner.outcome = Completion::Cancelled(reason.clone());
            }
            if !self.task {
                inner.body_finished = true;
            }
        }
        stack.push(Step::TryFinish(self.rc()));
        for child in self.child_jobs().into_iter().rev() {
            stack.push(Step::Cancel(child, reason.clone()));
        }
    }
    fn child_jobs(&self) -> Vec<Rc<RuntimeJob>> {
        let mut children = Vec::new();
        let mut node = self.inner.borrow().head.clone();
        while let Some(current) = node {
            if let Some(job) = &current.job {
                children.push(job.clone());
            }
            node = current.next.borrow().clone();
        }
        children
    }
    fn remove_node(&self, node: &Rc<Node>) {
        let mut inner = self.inner.borrow_mut();
        let mut current = inner.head.clone();
        let mut previous: Option<Rc<Node>> = None;
        while let Some(at) = current {
            let next = at.next.borrow().clone();
            if Rc::ptr_eq(&at, node) {
                if let Some(previous) = &previous {
                    *previous.next.borrow_mut() = next.clone();
                } else {
                    inner.head = next.clone();
                }
                if next.is_none() {
                    inner.tail = previous.as_ref().map(Rc::downgrade);
                }
                if at.job.is_some() {
                    inner.children -= 1;
                }
                break;
            }
            previous = Some(at);
            current = next;
        }
    }
    fn remove_child(&self, child: &RuntimeJob) {
        let mut current = self.inner.borrow().head.clone();
        while let Some(at) = current {
            if at.job.as_ref().is_some_and(|job| std::ptr::eq(job.as_ref(), child)) {
                self.remove_node(&at);
                return;
            }
            current = at.next.borrow().clone();
        }
    }
    fn try_finish_step(&self, stack: &mut Vec<Step>) {
        let child = {
            let mut inner = self.inner.borrow_mut();
            if inner.completed || !inner.body_finished {
                return;
            }
            let mut node = inner.head.clone();
            let mut child = None;
            while let Some(current) = node {
                if let Some(job) = &current.job {
                    child = Some(job.clone());
                    break;
                }
                node = current.next.borrow().clone();
            }
            if let Some(child) = &child {
                if inner
                    .waiting_child
                    .as_ref()
                    .and_then(Weak::upgrade)
                    .is_some_and(|waiting| Rc::ptr_eq(&waiting, child))
                {
                    return;
                }
                inner.waiting_child = Some(Rc::downgrade(child));
            }
            child
        };
        if let Some(child) = child {
            // JobSupport installs ChildCompletion when the parent starts waiting,
            // interleaving it with the child's user completion registrations.
            if child.register(Callback::Parent(self.myself.clone())).1.is_some() {
                stack.push(Step::TryFinish(self.rc()));
            }
            return;
        }
        let (callbacks, outcome) = {
            let mut inner = self.inner.borrow_mut();
            if inner.completed || !inner.body_finished || inner.children != 0 {
                return;
            }
            inner.completed = true;
            inner.head = None;
            inner.tail = None;
            (std::mem::take(&mut inner.callbacks), inner.outcome.clone())
        };
        // finalizeFinishingState: `cancelParent(e) || handleJobException(e)` runs
        // before the completion handlers, so the handler logs ahead of them.
        let handler = self.handler.borrow_mut().take();
        if let (Some(handler), Completion::Failed(failure)) = (handler, &outcome)
            && self.reports_failure()
        {
            handler(failure.clone());
        }
        if let Some(parent) = self.parent.as_ref().and_then(Weak::upgrade) {
            parent.remove_child(self);
        }
        self.changed.notify_waiters();
        for registration in callbacks.into_iter().rev() {
            stack.push(Step::Callback(registration, outcome.clone()));
        }
    }
    /// Appends a registration, or returns the completion if the job has completed.
    fn register(&self, callback: Callback) -> (RegistrationHandle, Option<Completion>) {
        let registration = Rc::new(Registration {
            callback: RefCell::new(Some(callback)),
        });
        let node = Rc::new(Node {
            job: None,
            next: RefCell::new(None),
        });
        let completion = {
            let mut inner = self.inner.borrow_mut();
            if inner.completed {
                Some(inner.outcome.clone())
            } else {
                inner.callbacks.push(registration.clone());
                None
            }
        };
        if completion.is_none() {
            self.append(node.clone());
        }
        let handle = RegistrationHandle {
            registration,
            owner: self.myself.clone(),
            node: Rc::downgrade(&node),
        };
        (handle, completion)
    }
    pub(crate) fn reports_failure(&self) -> bool {
        // JobImpl freezes exception ownership on attachment; completed parents
        // can detach and disappear before this task emits its handler record.
        !self.scoped.get() && !self.parent_handles_failure
    }
}
impl RuntimeJob {
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
    /// Cancellable traversal for an explicit caller, as join_in is for join (WP 3.6).
    pub fn join_observed_children_in(&self, caller: JobRef) -> LocalFuture<'_, Result<(), Cancellation>> {
        Box::pin(async move {
            let signal = caller.cancellation_signal();
            match crate::executor::shared::race(signal.cancelled(), self.join_observed_children()).await {
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
        self.inner.borrow_mut().body_finished = true;
        cascade(Step::TryFinish(self.rc()));
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
        let mut children = Vec::new();
        let mut node = self.inner.borrow().head.clone();
        while let Some(current) = node {
            if let Some(job) = &current.job {
                children.push(job.clone());
            }
            node = current.next.borrow().clone();
        }
        children
    }
    /// Kotlin job.children traversal, joining each observed child. Not a fixed
    /// point over future children. Preserve the pinned single-node/list traversal
    /// documented in lifecycle.md, including successor reads after each join.
    pub fn join_observed_children(&self) -> LocalFuture<'_, Result<(), Cancellation>> {
        Box::pin(async move {
            let (mut node, single) = {
                let inner = self.inner.borrow();
                (inner.head.clone(), !inner.list_mode)
            };
            while let Some(current) = node {
                if let Some(job) = &current.job {
                    job.join().await?;
                }
                if single {
                    break;
                }
                // Kotlin retains the yielded node and reads its successor after join.
                node = current.next.borrow().clone();
            }
            Ok(())
        })
    }
    pub fn invoke_on_completion(&self, callback: Box<dyn FnOnce(Completion)>) -> Box<dyn CompletionRegistration> {
        let (handle, completion) = self.register(Callback::User(callback));
        if let Some(completion) = completion {
            cascade(Step::Callback(handle.registration.clone(), completion));
        }
        Box::new(handle)
    }
}
