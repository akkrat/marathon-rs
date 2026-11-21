//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt
//!
//! D98 (x102): which simulators of the run have a busy video service, by worker and UDID. Rust-only. The state belongs
//! to the simulator, not to the device object that found it: a simulator that is recovered, added back or otherwise set
//! up again during the run gets a new device object, which must neither launch another recorder nor warn again, and the
//! run's end must shut each such simulator down once, whichever of its objects is the one to do it. Run-scoped: every
//! device object the run constructs shares the one instance through `SimulatorServices`. A restart of the simulator
//! (any confirmed shutdown) ends the state, since the next preparation boots it with a fresh video service.

use indexmap::IndexMap;
use std::sync::Mutex;

/// U260: the busy warning and every end-of-run shutdown line, which the 0.15.1 notes promise with `debug: false`.
/// `marathon_logging::filter` gives this target an INFO floor; the rest of the iOS crate stays at ERROR with debug off.
pub(crate) const TARGET: &str = "marathon_ios::busy_video";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    /// Video is off on the simulator; the run's end has not dealt with it yet.
    Off,
    /// Video is off, and one end-of-run step took the shutdown (or its warning); no other does.
    Claimed,
}

#[derive(Default)]
pub struct BusyVideoServices(Mutex<IndexMap<String, Busy>>);

impl BusyVideoServices {
    fn map(&self) -> std::sync::MutexGuard<'_, IndexMap<String, Busy>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Whether video is off on the simulator `key` for the rest of the run.
    pub fn is_busy(&self, key: &str) -> bool {
        self.map().contains_key(key)
    }
    /// Turns video off on `key`; true only the first time (the one warning).
    pub(crate) fn mark_busy(&self, key: &str) -> bool {
        let mut map = self.map();
        if map.contains_key(key) {
            return false;
        }
        map.insert(key.to_owned(), Busy::Off);
        true
    }
    /// The run's end takes the simulator's shutdown: true once per busy simulator, never for one that is not busy.
    pub(crate) fn claim_run_end(&self, key: &str) -> bool {
        match self.map().get_mut(key) {
            Some(state @ Busy::Off) => {
                *state = Busy::Claimed;
                true
            }
            _ => false,
        }
    }
    /// The simulator has been shut down: its next preparation boots it with a fresh video service.
    pub(crate) fn restarted(&self, key: &str) {
        self.map().shift_remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::BusyVideoServices;

    #[test]
    fn a_busy_simulator_warns_once_and_is_claimed_once_until_it_restarts() {
        let busy = BusyVideoServices::default();
        assert!(!busy.is_busy("a@local"));
        assert!(!busy.claim_run_end("a@local"));
        assert!(busy.mark_busy("a@local"));
        assert!(!busy.mark_busy("a@local"));
        assert!(busy.is_busy("a@local") && !busy.is_busy("b@local") && !busy.is_busy("a@worker"));
        assert!(busy.claim_run_end("a@local"));
        assert!(!busy.claim_run_end("a@local"));
        assert!(busy.is_busy("a@local"));
        busy.restarted("a@local");
        assert!(!busy.is_busy("a@local") && !busy.claim_run_end("a@local"));
        assert!(busy.mark_busy("a@local"));
    }
}
