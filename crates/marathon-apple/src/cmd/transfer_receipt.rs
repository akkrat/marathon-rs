//! D91: a transfer as the device that started it sees it; Rust-only lifecycle support for the ports of
//! AppleSimulatorDevice.kt and RsyncFileBridge.kt. Kotlin blocks in the transfer (A104), so its device never has a
//! transfer that outlives the call that made it.

use marathon_core::{
    error::{Cancellation, Failure, VendorResult},
    executor::shared::{SharedDeferred, Signal},
    runtime::{CancellationSignal, VendorClock},
};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

/// How a transfer's processes ended, once nothing of it runs on this host any more as far as the bridge can tell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferCleanup {
    /// Its process was reaped, its output read to the end or given up, and nothing it started was left (or nothing was
    /// started at all).
    Confirmed,
    /// The bridge could not confirm that: what it could not confirm, with the process's pid and the worker.
    Unconfirmed(Failure),
}

/// D91: a device's hold on one transfer, registered before the transfer starts under the lock its disposal takes. The
/// device stops the transfer through it, and waits on it for the transfer's end, which the transfer's owner publishes
/// when it lets go of the [`super::TransferStop`] it was given, whoever still awaits the transfer: a waiter that was
/// dropped does not end the ownership.
pub struct TransferReceipt {
    stop: Arc<Signal>,
    pull_clock: Option<Arc<dyn VendorClock>>,
    /// An earlier end than the bridge's own for its cleanup, on the vendor clock; `i64::MAX` for none.
    by: AtomicI64,
    pull_at: AtomicI64,
    /// Told of every stop and deadline, so that a step of the cleanup already running sees an earlier deadline.
    changed: tokio::sync::Notify,
    ended: Arc<SharedDeferred<TransferCleanup>>,
}
impl TransferReceipt {
    pub fn new() -> Arc<Self> {
        Self::with_clock(None)
    }
    fn with_clock(pull_clock: Option<Arc<dyn VendorClock>>) -> Arc<Self> {
        Arc::new(Self {
            stop: Signal::new(),
            pull_clock,
            by: AtomicI64::new(i64::MAX),
            pull_at: AtomicI64::new(i64::MAX),
            changed: tokio::sync::Notify::new(),
            ended: SharedDeferred::new(),
        })
    }
    /// Stops the transfer with `reason`, as its caller's cancellation would. `by`, on the vendor clock, is when its cleanup
    /// must have ended if that is sooner than the bridge's own bound; the earliest given is kept, also when it comes with a
    /// later stop and the cleanup is under way.
    pub fn stop(&self, reason: Cancellation, by: Option<i64>) {
        if let Some(by) = by {
            self.limit_to(by);
        }
        self.stop.cancel(reason);
        self.changed.notify_waiters();
    }
    fn limit_to(&self, deadline: i64) {
        self.limit_pull(
            deadline.saturating_sub(super::remote::rsync::rsync_file_bridge::STOP_CLEANUP_MILLIS),
            deadline,
        );
    }
    fn limit_pull(&self, pull_at: i64, deadline: i64) {
        self.by.fetch_min(deadline.min(i64::MAX - 1), Ordering::AcqRel);
        self.pull_at.fetch_min(pull_at, Ordering::AcqRel);
        self.changed.notify_waiters();
    }
    /// How it ended, once it has.
    pub fn ended(&self) -> Option<TransferCleanup> {
        match self.ended.completed()? {
            Ok(cleanup) => Some((*cleanup).clone()),
            Err(_) => None,
        }
    }
    /// Waits for the end; `signal` ends the wait, not the transfer.
    pub async fn wait_in(&self, signal: Arc<dyn CancellationSignal>) -> VendorResult<TransferCleanup> {
        Ok((*self.ended.wait_in(signal).await?).clone())
    }
    fn pull_stop_at(&self) -> Option<i64> {
        self.pull_clock.as_ref()?;
        Some(self.pull_at.load(Ordering::Acquire)).filter(|at| *at != i64::MAX)
    }
    pub(crate) fn pull_deadline_reason() -> Cancellation {
        super::cancelled("Device shutdown window has no time left for the artifact pull")
    }
    pub(crate) fn stop_reason(&self) -> Option<Cancellation> {
        self.stop.reason().or_else(|| {
            let clock = self.pull_clock.as_ref()?;
            (clock.now_millis() >= self.pull_stop_at()?).then(Self::pull_deadline_reason)
        })
    }
    pub(crate) async fn stopped(&self) -> Cancellation {
        loop {
            let changed = self.changed.notified();
            let mut changed = std::pin::pin!(changed);
            changed.as_mut().enable();
            if let Some(reason) = self.stop_reason() {
                return reason;
            }
            if let (Some(clock), Some(at)) = (&self.pull_clock, self.pull_stop_at()) {
                tokio::select! { biased;
                    reason = self.stop.cancelled() => return reason,
                    _ = &mut changed => {},
                    _ = clock.delay(at.saturating_sub(clock.now_millis()), Signal::new()) => {
                        return Self::pull_deadline_reason();
                    }
                }
            } else {
                tokio::select! { biased;
                    reason = self.stop.cancelled() => return reason,
                    _ = changed => {},
                }
            }
        }
    }
    /// Ready at the next stop or deadline given; enable it before reading `deadline`, so that none is missed.
    pub(crate) fn changed(&self) -> tokio::sync::futures::Notified<'_> {
        self.changed.notified()
    }
    pub(crate) fn deadline(&self) -> Option<i64> {
        Some(self.by.load(Ordering::Acquire)).filter(|by| *by != i64::MAX)
    }
    pub(crate) fn end(&self, cleanup: TransferCleanup) {
        self.ended.complete(Ok(cleanup));
    }
}

/// Registered under the device's seal lock so both existing and late artifact
/// pulls share the stop's deadline. Receipts carry the clock only for pulls.
#[derive(Default)]
pub struct DevicePulls {
    receipts: Vec<Arc<TransferReceipt>>,
    deadline: Option<i64>,
    signal: Option<marathon_core::signal_finish::SignalFinish>,
}
impl DevicePulls {
    pub fn register(&mut self, clock: Arc<dyn VendorClock>) -> Arc<TransferReceipt> {
        self.receipts.retain(|r| r.ended() != Some(TransferCleanup::Confirmed));
        let receipt = TransferReceipt::with_clock(Some(clock));
        if let Some(by) = self.deadline {
            receipt.limit_to(by);
        }
        if let Some(finish) = self.signal {
            receipt.limit_pull(finish.drain, finish.receipts);
        }
        self.receipts.push(receipt.clone());
        receipt
    }
    pub fn limit_to(&mut self, deadline: i64) {
        let by = self.deadline.map_or(deadline, |prior| prior.min(deadline));
        if self.deadline != Some(by) {
            self.deadline = Some(by);
            for receipt in &self.receipts {
                receipt.limit_to(by);
            }
        }
    }
    pub fn limit_for_signal(&mut self, finish: marathon_core::signal_finish::SignalFinish) {
        self.signal.get_or_insert(finish);
        for receipt in &self.receipts {
            receipt.limit_pull(finish.drain, finish.receipts);
        }
    }
    pub fn snapshot(&self) -> Vec<Arc<TransferReceipt>> {
        self.receipts.clone()
    }
}
