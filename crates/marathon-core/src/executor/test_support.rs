//! Port of core/src/main/kotlin/com/malinskiy/marathon/Marathon.kt
//! (WP 0.2b tools/oracle/module controlled dispatcher seam).
use super::{CoreExecutor, clock::TimerRequest};
use crate::{error::OperationResult, runtime::*, time::*};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, atomic::Ordering},
};

const DEFAULT_WATCHDOG: std::time::Duration = std::time::Duration::from_secs(120);
pub struct ControlledReplay {
    core: Rc<CoreExecutor>,
    timers: RefCell<Vec<TimerRequest>>,
    hooks: RefCell<Option<Arc<dyn ReplayHooks>>>,
    failure: RefCell<Option<ReplayError>>,
}
impl CoreExecutor {
    pub fn run_virtual<T: Send + 'static>(
        body: impl FnOnce(Rc<Self>, Rc<ControlledReplay>) -> LocalFuture<'static, OperationResult<T>> + Send + 'static,
    ) -> OperationResult<T> {
        Self::run_virtual_with_watchdog(DEFAULT_WATCHDOG, body)
    }
    /// A wall-clock watchdog outside the executor fails the scenario if the core
    /// thread stalls, e.g. on a future blocking inside poll (carried item C2, S5).
    pub fn run_virtual_with_watchdog<T: Send + 'static>(
        limit: std::time::Duration,
        body: impl FnOnce(Rc<Self>, Rc<ControlledReplay>) -> LocalFuture<'static, OperationResult<T>> + Send + 'static,
    ) -> OperationResult<T> {
        Self::run_mode(true, Some(limit), move |core| {
            let driver = Rc::new(ControlledReplay {
                core: core.clone(),
                timers: RefCell::new(Vec::new()),
                hooks: RefCell::new(None),
                failure: RefCell::new(None),
            });
            core.clock.timeline.driver.store(true, Ordering::SeqCst);
            body(core, driver)
        })
    }
}
impl ControlledReplay {
    /// The production cancellable adapter, with a physical completion fence.
    /// This relies on Tokio 1.53.2 dropping the future before firing the join
    /// waker (verified in task/core.rs and task/harness.rs), not a documented
    /// JoinHandle guarantee. Recheck that ordering when upgrading Tokio.
    pub fn spawn_vendor_cancellable(&self, body: VendorTaskBody) -> (Arc<dyn VendorTask>, tokio::task::JoinHandle<()>) {
        self.core.vendor.spawn_cancellable_with_join(body)
    }
    pub fn set_hooks(&self, hooks: Arc<dyn ReplayHooks>) {
        *self.hooks.borrow_mut() = Some(hooks);
    }
    fn fail<T>(&self, label: &str, message: &str) -> ClockResult<T> {
        let error = ReplayError {
            label: label.into(),
            message: message.into(),
        };
        if self.failure.borrow().is_none() {
            *self.failure.borrow_mut() = Some(error.clone());
        }
        Err(error)
    }
    fn collect(&self) {
        while let Some(timer) = self.core.clock.timeline.timers.pop() {
            self.timers.borrow_mut().push(timer);
        }
        self.timers.borrow_mut().retain(|t| !t.fired.is_closed());
    }
    fn next(&self) -> Option<i64> {
        self.collect();
        self.timers.borrow().iter().map(|t| t.deadline).min()
    }
    fn fire_next(&self) -> bool {
        self.collect();
        let now = self.core.clock.timeline.now();
        let mut timers = self.timers.borrow_mut();
        timers.sort_by_key(|t| (t.deadline, t.sequence));
        if timers.first().is_some_and(|timer| timer.deadline <= now) {
            let timer = timers.remove(0);
            let _ = timer.fired.send(());
            true
        } else {
            false
        }
    }
    async fn drain(&self) -> ClockResult<()> {
        for _ in 0..100_000 {
            let changed = self.core.tracker.changed.notified();
            let mut changed = std::pin::pin!(changed);
            changed.as_mut().enable();
            tokio::task::yield_now().await;
            if self.core.tracker.idle() {
                if self.fire_next() {
                    continue;
                }
                return Ok(());
            }
            // Work may be on a vendor worker, so wait for its poll acknowledgement
            // rather than guessing how many local yields make it quiescent.
            changed.await;
        }
        self.fail("scheduler", "runnable work failed to settle")
    }
    /// Delivers equal-time events in caller/tape order with both drain boundaries.
    pub async fn event(&self, millis: i64, deliver: impl FnOnce() -> ClockResult<()>) -> ClockResult<()> {
        self.advance_to(millis).await?;
        deliver()?;
        self.drain_current().await
    }
    /// `TestCoroutineScheduler.advanceUntilIdle`: run timers in deadline order until
    /// none is left; the clock stops at the last deadline.
    pub async fn advance_until_idle(&self) -> ClockResult<()> {
        self.drain().await?;
        while let Some(deadline) = self.next() {
            self.advance_to(deadline.max(self.core.clock.timeline.now())).await?;
        }
        Ok(())
    }
    pub fn pending_timers(&self) -> usize {
        self.collect();
        self.timers.borrow().len()
    }
}
impl ReplayDriver for ControlledReplay {
    fn register_mailbox(&self, label: &'static str, probe: Rc<dyn MailboxProbe>) {
        self.core.mailboxes.borrow_mut().push((label, probe));
    }
    fn advance_to(&self, millis: i64) -> LocalFuture<'_, ClockResult<()>> {
        Box::pin(async move {
            if millis < self.core.clock.timeline.now() {
                return self.fail("scheduler", "scheduler time cannot move backwards");
            }
            self.drain().await?;
            while let Some(deadline) = self.next().filter(|deadline| *deadline <= millis) {
                self.core.clock.timeline.now.store(deadline, Ordering::SeqCst);
                self.drain().await?;
            }
            self.core.clock.timeline.now.store(millis, Ordering::SeqCst);
            self.drain().await
        })
    }
    fn drain_current(&self) -> LocalFuture<'_, ClockResult<()>> {
        Box::pin(self.drain())
    }
    fn assert_quiescent(&self) -> ClockResult<()> {
        if !self.core.tracker.idle() {
            return self.fail("scheduler", "runnable work remains");
        }
        for (label, probe) in self.core.mailboxes.borrow().iter() {
            let state = probe.state();
            if state.queued != 0 && !state.discarded {
                return self.fail(label, "stalled buffered mailbox");
            }
        }
        Ok(())
    }
    fn finish(&self) -> ClockResult<()> {
        if let Some(error) = self.failure.borrow().clone() {
            return Err(error);
        }
        self.assert_quiescent()?;
        if let Some(hooks) = self.hooks.borrow().as_ref() {
            hooks.verify_consumed()?;
        }
        Ok(())
    }
}

impl super::RuntimeClock {
    pub fn virtual_clock() -> Rc<Self> {
        Rc::new(Self::new(true))
    }
}
