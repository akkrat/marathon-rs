//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/PendingWorker.kt

use marathon_config::devices::AppleTarget;
use std::sync::Mutex;

/// D44: generic so that the provider's retry can keep each target's generation with it.
pub struct PendingWorker<T = AppleTarget> {
    state: Mutex<(bool, Vec<T>)>,
}
impl<T: Clone> PendingWorker<T> {
    pub fn new(targets: Vec<T>) -> Self {
        Self {
            state: Mutex::new((true, targets)),
        }
    }
    pub fn offer(&self, targets: &[T]) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.0 {
            return false;
        }
        state.1.extend_from_slice(targets);
        true
    }
    pub fn take(&self) -> Option<Vec<T>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.1.is_empty() {
            state.0 = false;
            None
        } else {
            Some(std::mem::take(&mut state.1))
        }
    }
    /// D44: keeps what a retry still works for. A worker left with nothing takes no more offers, so that a later one
    /// starts a retry of its own rather than joining one that is ending.
    pub fn retain(&self, keep: impl FnMut(&T) -> bool) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.1.retain(keep);
        if state.1.is_empty() {
            state.0 = false;
        }
        !state.1.is_empty()
    }
    /// D44: what waits now, left in place.
    pub fn peek(&self) -> Vec<T> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).1.clone()
    }
    pub fn close(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).0 = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_worker_drains_duplicates_and_close_retains_targets() {
        let fixture: serde_json::Value = include_str!("../../../../fixtures/oracle/simulator-recovery/probes.jsonl")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|v| v["case"] == "pending-worker-drain-close")
            .unwrap()["value"]
            .clone();
        let target = AppleTarget::Simulator { udid: "a".into() };
        let pending = PendingWorker::new(vec![target.clone(), target.clone()]);
        assert_eq!(
            pending.take().unwrap().len() as u64,
            fixture["firstCount"].as_u64().unwrap()
        );
        assert!(pending.offer(std::slice::from_ref(&target)));
        pending.close();
        assert_eq!(
            pending.offer(std::slice::from_ref(&target)),
            fixture["offerAfterClose"].as_bool().unwrap()
        );
        assert_eq!(
            pending.take().unwrap().len() as u64,
            fixture["retainedCount"].as_u64().unwrap()
        );
        assert_eq!(pending.take().is_none(), fixture["emptyTake"].as_bool().unwrap());
        assert_eq!(pending.offer(&[target]), fixture["offerAfterEmpty"].as_bool().unwrap());
    }
}
