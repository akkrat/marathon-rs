//! Port of core/src/main/kotlin/com/malinskiy/marathon/actor/extensions.kt
//! and execution/device/DeviceActor.kt (CompletableDeferred use sites).

use crate::{error::*, runtime::*};
use crossbeam_queue::SegQueue;
use futures_util::task::AtomicWaker;
use std::{
    future::{Future, poll_fn},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};
use tokio::sync::Notify;

pub fn failure(class: &str, message: &str) -> Failure {
    Failure {
        class: class.into(),
        message: Some(message.into()),
        cause: None,
        stacktrace: None,
    }
}
pub fn requested(message: &str) -> Cancellation {
    Cancellation::Requested(failure("java.util.concurrent.CancellationException", message))
}
pub(crate) fn replay_failure(message: &str) -> Failure {
    failure("marathon.ReplayError", message)
}
#[derive(Default)]
pub struct Signal {
    reason: OnceLock<Cancellation>,
    changed: Notify,
}
impl Signal {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn cancel(&self, reason: Cancellation) -> bool {
        let won = self.reason.set(reason).is_ok();
        if won {
            self.changed.notify_waiters();
        }
        won
    }
}
impl CancellationSignal for Signal {
    fn reason(&self) -> Option<Cancellation> {
        self.reason.get().cloned()
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn Future<Output = Cancellation> + Send + '_>> {
        Box::pin(async move {
            loop {
                let changed = self.changed.notified();
                let mut changed = std::pin::pin!(changed);
                changed.as_mut().enable();
                if let Some(reason) = self.reason() {
                    return reason;
                }
                changed.await;
            }
        })
    }
}

/// A cancelled waiter only drops its notification future, never the stored cell.
pub struct SharedDeferred<T> {
    value: OnceLock<VendorResult<Arc<T>>>,
    changed: Notify,
}
impl<T> Default for SharedDeferred<T> {
    fn default() -> Self {
        Self {
            value: OnceLock::new(),
            changed: Notify::new(),
        }
    }
}
impl<T: Send + Sync> SharedDeferred<T> {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub async fn wait_in(&self, signal: Arc<dyn CancellationSignal>) -> VendorResult<Arc<T>> {
        // awaitInternal returns an already-completed result before checking the
        // waiter. Suspended success is prompt-cancellable; exceptional resume
        // competes with cancellation at notification time, not the next poll.
        if let Some(value) = self.completed() {
            return value;
        }
        let resume = Arc::new(DeferredResume {
            signal: signal.clone(),
            cancellation: OnceLock::new(),
            outer: AtomicWaker::new(),
        });
        let waker = Waker::from(resume.clone());
        let cancellation = signal.cancelled();
        let mut cancellation = std::pin::pin!(cancellation);
        let value = self.wait();
        let mut value = std::pin::pin!(value);
        let mut suspended = false;
        poll_fn(|cx| {
            resume.outer.register(cx.waker());
            if let Poll::Ready(reason) = cancellation.as_mut().poll(cx) {
                // Until this await first suspends it is still the call's fast path:
                // a cross-thread completion that landed after the check above was
                // never notified, so read the cell itself before the waiter (C5).
                if !suspended && let Some(value) = self.completed() {
                    return Poll::Ready(value);
                }
                if matches!(resume.cancellation.get(), Some(None))
                    && let Some(Err(error)) = self.completed()
                {
                    return Poll::Ready(Err(error));
                }
                return Poll::Ready(Err(VendorError::Cancelled(reason)));
            }
            let polled = value.as_mut().poll(&mut Context::from_waker(&waker));
            suspended |= polled.is_pending();
            polled
        })
        .await
    }
}
// Notify invokes this only when the cell seals. Snapshot the waiter's request
// before forwarding the wake, since the executor can coalesce both wakes.
struct DeferredResume {
    signal: Arc<dyn CancellationSignal>,
    cancellation: OnceLock<Option<Cancellation>>,
    outer: AtomicWaker,
}
impl Wake for DeferredResume {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.cancellation.set(self.signal.reason());
        self.outer.wake();
    }
}

impl<T: Send + Sync> Deferred<T> for SharedDeferred<T> {
    fn complete(&self, value: VendorResult<T>) -> bool {
        let won = self.value.set(value.map(Arc::new)).is_ok();
        if won {
            self.changed.notify_waiters();
        }
        won
    }
    fn cancel(&self, reason: Cancellation) -> bool {
        self.complete(Err(VendorError::Cancelled(reason)))
    }
    fn completed(&self) -> Option<VendorResult<Arc<T>>> {
        self.value.get().cloned()
    }
    fn wait(&self) -> VendorFuture<'_, Arc<T>> {
        Box::pin(async move {
            loop {
                let changed = self.changed.notified();
                let mut changed = std::pin::pin!(changed);
                changed.as_mut().enable();
                if let Some(value) = self.completed() {
                    return value;
                }
                changed.await;
            }
        })
    }
}

enum Terminal {
    Closed(Option<Failure>),
}
/// The queue and in-flight counter let close remain nonblocking on core even
/// when a vendor sender is between admission and enqueue.
pub struct SharedChannel<T> {
    queue: SegQueue<T>,
    queued: AtomicUsize,
    sending: AtomicUsize,
    terminal: OnceLock<Terminal>,
    cancellation: OnceLock<Cancellation>,
    changed: Notify,
}
impl<T> Default for SharedChannel<T> {
    fn default() -> Self {
        Self {
            queue: SegQueue::new(),
            queued: AtomicUsize::new(0),
            sending: AtomicUsize::new(0),
            terminal: OnceLock::new(),
            cancellation: OnceLock::new(),
            changed: Notify::new(),
        }
    }
}
impl<T: Send> SharedChannel<T> {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn receive_ready(&self) -> Option<VendorResult<Option<T>>> {
        if let Some(reason) = self.cancellation.get() {
            while self.queue.pop().is_some() {
                self.queued.fetch_sub(1, Ordering::SeqCst);
            }
            return Some(Err(VendorError::Cancelled(reason.clone())));
        }
        if let Some(value) = self.queue.pop() {
            self.queued.fetch_sub(1, Ordering::SeqCst);
            return Some(Ok(Some(value)));
        }
        if self.sending.load(Ordering::SeqCst) == 0
            && let Some(Terminal::Closed(cause)) = self.terminal.get()
        {
            return Some(match cause {
                Some(f) => Err(VendorError::Other(f.clone())),
                None => Ok(None),
            });
        }
        None
    }
    pub async fn receive_in(&self, signal: Arc<dyn CancellationSignal>) -> VendorResult<Option<T>> {
        // Kotlin receive checks buffered/terminal state before cancellation;
        // prompt waiter cancellation applies only when it would suspend.
        if let Some(result) = self.receive_ready() {
            return result;
        }
        match race(signal.cancelled(), self.receive()).await {
            futures_util::future::Either::Left(reason) => Err(VendorError::Cancelled(reason)),
            futures_util::future::Either::Right(value) => value,
        }
    }
}
impl<T: Send> EventChannel<T> for SharedChannel<T> {
    fn try_send(&self, message: T) -> Result<(), SendError<T>> {
        self.sending.fetch_add(1, Ordering::SeqCst);
        let result = if self.cancellation.get().is_some() {
            Err(SendError { message, cause: None })
        } else if let Some(terminal) = self.terminal.get() {
            let cause = match terminal {
                Terminal::Closed(cause) => cause.clone(),
            };
            Err(SendError { message, cause })
        } else {
            self.queued.fetch_add(1, Ordering::SeqCst);
            self.queue.push(message);
            Ok(())
        };
        self.sending.fetch_sub(1, Ordering::SeqCst);
        self.changed.notify_waiters();
        result
    }
    fn receive(&self) -> VendorFuture<'_, Option<T>> {
        Box::pin(async move {
            loop {
                let changed = self.changed.notified();
                let mut changed = std::pin::pin!(changed);
                changed.as_mut().enable();
                if let Some(result) = self.receive_ready() {
                    return result;
                }
                changed.await;
            }
        })
    }
    fn close(&self, cause: Option<Failure>) {
        let _ = self.terminal.set(Terminal::Closed(cause));
        self.changed.notify_waiters();
    }
    fn cancel(&self, reason: Cancellation) {
        let _ = self.cancellation.set(reason);
        while self.queue.pop().is_some() {
            self.queued.fetch_sub(1, Ordering::SeqCst);
        }
        self.changed.notify_waiters();
    }
    fn state(&self) -> MailboxState {
        let discarded = self.cancellation.get().is_some();
        MailboxState {
            queued: if discarded {
                0
            } else {
                self.queued.load(Ordering::SeqCst)
            },
            closed_for_send: self.terminal.get().is_some() || discarded,
            discarded,
        }
    }
}
impl<T: Send> MailboxProbe for SharedChannel<T> {
    fn state(&self) -> MailboxState {
        EventChannel::state(self)
    }
}

/// Poll left first; keep task bodies borrowed so timeout/cancel never drops them.
pub(crate) async fn race<A: Future, B: Future>(
    left: A,
    right: B,
) -> futures_util::future::Either<A::Output, B::Output> {
    use futures_util::future::{Either, select};
    let left = std::pin::pin!(left);
    let right = std::pin::pin!(right);
    match select(left, right).await {
        Either::Left((value, _)) => Either::Left(value),
        Either::Right((value, _)) => Either::Right(value),
    }
}
