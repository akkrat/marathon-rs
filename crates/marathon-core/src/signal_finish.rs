//! D97 deadlines on the run's shared scheduler timeline.

use crate::{executor::shared::Signal, runtime::VendorClock};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug)]
pub struct SignalFinish {
    pub reports: i64,
    pub drain: i64,
    pub receipts: i64,
    pub devices: i64,
    pub exit: i64,
}

impl SignalFinish {
    pub fn from_start(at: i64) -> Self {
        Self {
            reports: at.saturating_add(crate::marathon::CANCEL_REPORTS_HEAD_START_MILLIS),
            drain: at.saturating_add(12_000),
            receipts: at.saturating_add(18_000),
            devices: at.saturating_add(crate::marathon::CANCEL_LIMIT_MILLIS - crate::marathon::CANCEL_FLUSH_MILLIS),
            exit: at.saturating_add(crate::marathon::CANCEL_LIMIT_MILLIS),
        }
    }
}

/// A wait that began during routine finish must see a later signal as well.
#[derive(Default)]
pub struct SignalDeadline(watch::Sender<Option<SignalFinish>>);

impl SignalDeadline {
    pub fn set(&self, finish: SignalFinish) {
        self.0.send_if_modified(|current| {
            if current.is_some() {
                return false;
            }
            *current = Some(finish);
            true
        });
    }

    pub fn current(&self) -> Option<SignalFinish> {
        *self.0.borrow()
    }

    pub fn limit(&self, deadline: i64, cutoff: fn(SignalFinish) -> i64) -> i64 {
        self.current().map_or(deadline, |finish| deadline.min(cutoff(finish)))
    }

    pub async fn wait(&self, clock: &dyn VendorClock, deadline: i64, cutoff: fn(SignalFinish) -> i64) {
        let mut changes = self.0.subscribe();
        loop {
            let finish = *changes.borrow_and_update();
            let at = finish.map_or(deadline, |finish| deadline.min(cutoff(finish)));
            let left = at.saturating_sub(clock.now_millis());
            if left <= 0 {
                return;
            }
            if finish.is_some() {
                let _ = clock.delay(left, Signal::new()).await;
                return;
            }
            tokio::select! { biased;
                _ = changes.changed() => {},
                _ = clock.delay(left, Signal::new()) => return,
            }
        }
    }
}
