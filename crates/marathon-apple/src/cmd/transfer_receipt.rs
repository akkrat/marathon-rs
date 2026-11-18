//! D91: a transfer as the device that started it sees it; Rust-only lifecycle support for the ports of
//! AppleSimulatorDevice.kt and RsyncFileBridge.kt. Kotlin blocks in the transfer (A104), so its device never has a
//! transfer that outlives the call that made it.

use marathon_core::{
    error::{Cancellation, Failure, VendorResult},
    executor::shared::{SharedDeferred, Signal},
    runtime::CancellationSignal,
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
    /// An earlier end than the bridge's own for its cleanup, on the vendor clock; `i64::MAX` for none.
    by: AtomicI64,
    ended: Arc<SharedDeferred<TransferCleanup>>,
}
impl TransferReceipt {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            stop: Signal::new(),
            by: AtomicI64::new(i64::MAX),
            ended: SharedDeferred::new(),
        })
    }
    /// Stops the transfer with `reason`, as its caller's cancellation would. `by`, on the vendor clock, is when its cleanup
    /// must have ended if that is sooner than the bridge's own bound; the first stop's is kept.
    pub fn stop(&self, reason: Cancellation, by: Option<i64>) {
        if let Some(by) = by {
            let _ = self
                .by
                .compare_exchange(i64::MAX, by.min(i64::MAX - 1), Ordering::AcqRel, Ordering::Acquire);
        }
        self.stop.cancel(reason);
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
    pub(crate) fn stop_signal(&self) -> Arc<Signal> {
        self.stop.clone()
    }
    pub(crate) fn deadline(&self) -> Option<i64> {
        Some(self.by.load(Ordering::Acquire)).filter(|by| *by != i64::MAX)
    }
    pub(crate) fn end(&self, cleanup: TransferCleanup) {
        self.ended.complete(Ok(cleanup));
    }
}
