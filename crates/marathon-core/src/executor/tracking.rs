//! Port of core/src/main/kotlin/com/malinskiy/marathon/Marathon.kt
//! (controlled runBlocking/dispatcher seam).
use futures_util::task::AtomicWaker;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};
use tokio::sync::Notify;
/// Counts tracked work that is runnable (a woken ticket), running (a poll in
/// progress) or blocking, as one number. Work passes from one of these to
/// another by taking its new unit before giving up the old one, so `busy` stays
/// above zero across every hand-off. Two counters read one after the other
/// cannot give that: a hand-off between the two loads (a vendor poll waking a
/// core task, then returning) reads as zero work.
#[derive(Default)]
pub(crate) struct Tracker {
    busy: AtomicUsize,
    pub changed: Notify,
}
impl Tracker {
    pub fn acquire(&self) {
        self.busy.fetch_add(1, Ordering::SeqCst);
    }
    pub fn release(&self) {
        self.busy.fetch_sub(1, Ordering::SeqCst);
    }
    /// No tracked work is runnable or running, in a single snapshot.
    /// Only the controlled replay driver asks, so production builds leave it out.
    #[cfg(any(test, feature = "test-support"))]
    pub fn idle(&self) -> bool {
        self.busy.load(Ordering::SeqCst) == 0
    }
}
struct Ticket {
    tracker: Arc<Tracker>,
    state: AtomicU8,
    outer: AtomicWaker,
}
impl Wake for Ticket {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_with(|| {});
    }
}
impl Ticket {
    // The hook lets tests preempt a wake after its ticket decision.
    fn wake_with(&self, before_forward: impl FnOnce()) {
        self.tracker.acquire();
        let mut state = self.state.load(Ordering::SeqCst);
        let forward = loop {
            if state == 2 || state == 1 {
                break false;
            }
            match self
                .state
                .compare_exchange(state, 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => {
                    self.tracker.acquire();
                    break true;
                }
                Err(actual) => state = actual,
            }
        };
        before_forward();
        // A delayed duplicate must not enqueue a poll after the counted ticket is consumed.
        if forward {
            self.outer.wake();
        }
        self.tracker.release();
        self.tracker.changed.notify_waiters();
    }
}
pub(crate) struct Tracked<F> {
    future: Option<Pin<Box<F>>>,
    ticket: Arc<Ticket>,
}
impl<F> Tracked<F> {
    pub fn new(future: F, tracker: Arc<Tracker>) -> Self {
        tracker.acquire();
        Self {
            future: Some(Box::pin(future)),
            ticket: Arc::new(Ticket {
                tracker,
                state: AtomicU8::new(1),
                outer: AtomicWaker::new(),
            }),
        }
    }
}
impl<F> Drop for Tracked<F> {
    fn drop(&mut self) {
        // Even a suspended future's destructor can hand work to another task.
        self.ticket.tracker.acquire();
        let _dropping = Running(&self.ticket);
        // Retire before destruction so retained wakers cannot revive this task.
        let _queued = (self.ticket.state.swap(2, Ordering::SeqCst) == 1).then(|| Running(&self.ticket));
        drop(self.future.take());
    }
}
struct Running<'a>(&'a Ticket);
impl Drop for Running<'_> {
    fn drop(&mut self) {
        // On unwind, transfer this unit to a ticket until Tracked drops its future.
        // An already-woken ticket supplies that unit instead; retired tickets release.
        if !std::thread::panicking()
            || self
                .0
                .state
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            self.0.tracker.release();
        }
        // Unwinding assumes Tokio/replay wakers do not panic; a panicking waker here aborts.
        self.0.tracker.changed.notify_waiters();
    }
}
impl<F: Future> Future for Tracked<F> {
    type Output = F::Output;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let ticket = self.ticket.clone();
        ticket.outer.register(cx.waker());
        ticket.tracker.acquire();
        // A poll can unwind before the executor drops this tracked future.
        let _running = Running(&ticket);
        if ticket.state.swap(0, Ordering::SeqCst) == 1 {
            ticket.tracker.release();
        }
        let waker = Waker::from(ticket.clone());
        let result = self
            .future
            .as_mut()
            .expect("tracked future is only removed during drop")
            .as_mut()
            .poll(&mut Context::from_waker(&waker));
        if result.is_ready() && ticket.state.swap(2, Ordering::SeqCst) == 1 {
            ticket.tracker.release();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Barrier, Mutex, atomic::AtomicBool},
        thread,
    };

    struct DropHandOff {
        tracker: Arc<Tracker>,
        idle_during_drop: Arc<AtomicBool>,
        next: Waker,
        panic_on_poll: bool,
    }
    impl Future for DropHandOff {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
            assert!(!self.panic_on_poll, "hand-off poll panic");
            Poll::Pending
        }
    }
    impl Drop for DropHandOff {
        fn drop(&mut self) {
            self.idle_during_drop.store(self.tracker.idle(), Ordering::SeqCst);
            self.next.wake_by_ref();
        }
    }
    fn drop_hand_off(panic_on_poll: bool, wake_before_drop: bool) {
        let tracker = Arc::new(Tracker::default());
        let idle_during_drop = Arc::new(AtomicBool::new(false));
        let waker = futures_util::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut next = Box::pin(Tracked::new(std::future::pending::<()>(), tracker.clone()));
        assert!(next.as_mut().poll(&mut cx).is_pending());
        let mut task = Box::pin(Tracked::new(
            DropHandOff {
                tracker: tracker.clone(),
                idle_during_drop: idle_during_drop.clone(),
                next: Waker::from(next.ticket.clone()),
                panic_on_poll,
            },
            tracker.clone(),
        ));
        let idle_after_unwind = if panic_on_poll {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { task.as_mut().poll(&mut cx) })).is_err()
            );
            tracker.idle()
        } else {
            assert!(task.as_mut().poll(&mut cx).is_pending());
            false
        };
        if wake_before_drop {
            task.ticket.wake_by_ref();
        }
        drop(task);
        assert!(!tracker.idle(), "the destructor must wake the next tracked task");
        drop(next);
        assert!(tracker.idle(), "the hand-off must release all work after both drops");
        assert!(
            !idle_during_drop.load(Ordering::SeqCst),
            "busy hit zero during pending-drop hand-off"
        );
        assert!(!idle_after_unwind, "busy hit zero before unwound future destruction");
    }

    #[test]
    fn pending_drop_hand_off_never_reads_as_idle() {
        drop_hand_off(false, true);
    }

    #[test]
    fn suspended_drop_hand_off_never_reads_as_idle() {
        drop_hand_off(false, false);
    }

    #[test]
    fn unwinding_drop_hand_off_never_reads_as_idle() {
        drop_hand_off(true, false);
    }

    #[test]
    fn dropping_an_unpolled_tracked_future_releases_its_ticket() {
        let tracker = Arc::new(Tracker::default());
        let task = Tracked::new(std::future::pending::<()>(), tracker.clone());
        assert!(!tracker.idle());
        drop(task);
        assert!(tracker.idle(), "dropping an unpolled future leaked its runnable ticket");
    }

    #[test]
    fn dropping_a_woken_tracked_future_releases_its_ticket_and_ignores_stale_wakes() {
        let tracker = Arc::new(Tracker::default());
        let mut task = Box::pin(Tracked::new(std::future::pending::<()>(), tracker.clone()));
        let waker = futures_util::task::noop_waker();
        assert!(task.as_mut().poll(&mut Context::from_waker(&waker)).is_pending());
        assert!(tracker.idle());
        let ticket = task.ticket.clone();
        ticket.wake_by_ref();
        assert!(!tracker.idle());
        drop(task);
        assert!(tracker.idle(), "dropping a woken future leaked its runnable ticket");
        ticket.wake_by_ref();
        assert!(tracker.idle(), "a stale wake reacquired a dropped future's ticket");
    }

    #[test]
    fn dropping_a_suspended_tracked_future_ignores_stale_wakes() {
        let tracker = Arc::new(Tracker::default());
        let mut task = Box::pin(Tracked::new(std::future::pending::<()>(), tracker.clone()));
        let waker = futures_util::task::noop_waker();
        assert!(task.as_mut().poll(&mut Context::from_waker(&waker)).is_pending());
        let ticket = task.ticket.clone();
        drop(task);
        ticket.wake_by_ref();
        assert!(tracker.idle(), "a stale wake reacquired a dropped future's ticket");
    }

    #[test]
    fn panicking_tracked_poll_releases_running_and_runnable_work() {
        let tracker = Arc::new(Tracker::default());
        let mut task = Box::pin(Tracked::new(
            std::future::poll_fn(|cx| -> Poll<()> {
                cx.waker().wake_by_ref();
                panic!("tracked poll");
            }),
            tracker.clone(),
        ));
        let waker = futures_util::task::noop_waker();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                task.as_mut().poll(&mut Context::from_waker(&waker))
            }))
            .is_err()
        );
        drop(task);
        assert!(tracker.idle(), "a panicking poll leaked running or runnable work");
    }

    #[tokio::test]
    async fn aborting_a_tracked_task_releases_its_ticket() {
        let tracker = Arc::new(Tracker::default());
        let task = tokio::spawn(Tracked::new(std::future::pending::<()>(), tracker.clone()));
        task.abort();
        let joined = tokio::time::timeout(std::time::Duration::from_secs(60), task).await;
        assert!(matches!(joined, Ok(Err(error)) if error.is_cancelled()));
        assert!(tracker.idle(), "aborting a tracked task leaked its runnable ticket");
    }

    /// Runs one tracked future on its own thread, as the vendor runtime and the
    /// core thread each do; a wake through the ticket reaches `woken`.
    struct Wakeup {
        woken: AtomicBool,
        thread: thread::Thread,
    }
    impl Wake for Wakeup {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.woken.store(true, Ordering::SeqCst);
            self.thread.unpark();
        }
    }
    fn run(mut task: Pin<Box<dyn Future<Output = ()> + Send>>) {
        let wakeup = Arc::new(Wakeup {
            woken: AtomicBool::new(true),
            thread: thread::current(),
        });
        let waker = Waker::from(wakeup.clone());
        loop {
            if wakeup.woken.swap(false, Ordering::SeqCst) {
                if task.as_mut().poll(&mut Context::from_waker(&waker)).is_ready() {
                    return;
                }
            } else {
                thread::park();
            }
        }
    }

    #[test]
    fn a_delayed_duplicate_wake_does_not_queue_uncounted_work() {
        let tracker = Arc::new(Tracker::default());
        let mut polls = 0;
        let mut task = Box::pin(Tracked::new(
            std::future::poll_fn(move |_| {
                polls += 1;
                if polls == 3 { Poll::Ready(()) } else { Poll::Pending }
            }),
            tracker.clone(),
        ));
        let outer = Arc::new(Wakeup {
            woken: AtomicBool::new(false),
            thread: thread::current(),
        });
        let waker = Waker::from(outer.clone());
        let mut cx = Context::from_waker(&waker);
        assert!(task.as_mut().poll(&mut cx).is_pending());
        assert!(tracker.idle());

        let ticket = task.ticket.clone();
        ticket.wake_by_ref();
        assert!(!tracker.idle());
        let entered = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let duplicate = {
            let (ticket, entered, resume) = (ticket.clone(), entered.clone(), resume.clone());
            thread::spawn(move || {
                ticket.wake_with(|| {
                    entered.wait();
                    resume.wait();
                });
            })
        };
        entered.wait();
        // The duplicate has observed the queued ticket but cannot forward yet.
        assert!(outer.woken.swap(false, Ordering::SeqCst));
        assert!(task.as_mut().poll(&mut cx).is_pending());
        assert!(!tracker.idle()); // The paused wake still holds its temporary unit.
        resume.wait();
        duplicate.join().unwrap();
        assert!(tracker.idle());
        assert!(!outer.woken.load(Ordering::SeqCst));

        // A fresh wake still counts and forwards; a completed ticket does neither.
        ticket.wake_by_ref();
        assert!(!tracker.idle());
        assert!(outer.woken.swap(false, Ordering::SeqCst));
        assert!(task.as_mut().poll(&mut cx).is_ready());
        assert!(tracker.idle());
        ticket.wake_by_ref();
        assert!(tracker.idle());
        assert!(!outer.woken.load(Ordering::SeqCst));
    }

    /// Two tracked tasks on two threads hand a turn back and forth: each poll
    /// wakes the other task, then returns. Work is runnable or running across
    /// every hand-off, so a reader on a third thread must never see the tracker
    /// idle before the last poll. A reader that loaded a runnable count and a
    /// polling count one after the other saw zero work in the gap (WP 3.6
    /// post-merge Linux failure).
    #[test]
    fn a_hand_off_between_threads_never_reads_as_idle() {
        const ROUNDS: usize = 200_000;
        let tracker = Arc::new(Tracker::default());
        let turn = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let wakers: Arc<[Mutex<Option<Waker>>; 2]> = Arc::new([Mutex::new(None), Mutex::new(None)]);
        let player = |me: usize| {
            let (turn, done, wakers) = (turn.clone(), done.clone(), wakers.clone());
            let tracked = Tracked::new(
                std::future::poll_fn(move |cx| {
                    *wakers[me].lock().unwrap() = Some(cx.waker().clone());
                    let now = turn.load(Ordering::SeqCst);
                    if now >= ROUNDS {
                        // Set while this poll still counts as work.
                        done.store(true, Ordering::SeqCst);
                        return Poll::Ready(());
                    }
                    if now % 2 == me {
                        turn.store(now + 1, Ordering::SeqCst);
                        // None: the other task is unpolled, still runnable from `Tracked::new`.
                        let other = wakers[1 - me].lock().unwrap().clone();
                        if let Some(other) = other {
                            other.wake();
                        }
                        if now + 1 >= ROUNDS {
                            return Poll::Ready(());
                        }
                    }
                    Poll::Pending
                }),
                tracker.clone(),
            );
            let task: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(tracked);
            thread::spawn(move || run(task))
        };
        let players = [player(0), player(1)];
        let mut checks = 0usize;
        loop {
            if tracker.idle() {
                assert!(
                    done.load(Ordering::SeqCst),
                    "idle at turn {} of {ROUNDS}",
                    turn.load(Ordering::SeqCst)
                );
                break;
            }
            checks += 1;
        }
        for player in players {
            player.join().unwrap();
        }
        assert!(checks > 0);
    }
}
