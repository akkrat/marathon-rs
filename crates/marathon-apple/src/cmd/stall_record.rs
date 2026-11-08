//! U163: no Kotlin source. What lets the sends queued behind a stalled one fail at once, instead of each spending its own
//! limit on the same worker one after another.

use marathon_core::error::VendorError;
use std::sync::Mutex;

/// A count of the stalls seen and the last one's error. A send takes a ticket when it starts waiting, and a stall recorded
/// after that is one it waited behind. A send that starts later has not waited, so it tries: the worker may have come back.
#[derive(Default)]
pub(crate) struct StallRecord(Mutex<(u64, Option<VendorError>)>);
impl StallRecord {
    fn state(&self) -> std::sync::MutexGuard<'_, (u64, Option<VendorError>)> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn ticket(&self) -> u64 {
        self.state().0
    }
    pub(crate) fn since(&self, ticket: u64) -> Option<VendorError> {
        let state = self.state();
        if state.0 == ticket { None } else { state.1.clone() }
    }
    pub(crate) fn record(&self, error: VendorError) {
        let mut state = self.state();
        state.0 += 1;
        state.1 = Some(error);
    }
}
