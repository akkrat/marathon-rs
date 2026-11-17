//! D77 allowance for the JVM progress and queue recordings: with `fast: false` the JVM tracks the run
//! that completes a test as final (C87); the native accounting tracks the run `fast: true` would
//! complete the test on. Only the final flag of a tracked run moves; the recordings stay as they are.
#![allow(dead_code)]
use std::collections::HashSet;

pub struct D77<K> {
    any_success: bool,
    decided: HashSet<K>,
}
impl<K: std::hash::Hash + Eq> D77<K> {
    pub fn new(any_success: bool) -> Self {
        Self {
            any_success,
            decided: HashSet::new(),
        }
    }
    /// A test whose final run is already tracked: one that is terminal, or (fast's view) has a pass
    /// under ANY_SUCCESS or a failure under ALL_SUCCESS.
    pub fn decide(&mut self, key: K) {
        self.decided.insert(key);
    }
    /// The native final flag of a tracked run of `key` whose recorded flag is `recorded`, given in
    /// tracking order. The first run that completes the test or decides its verdict is final; none later.
    pub fn final_flag(&mut self, key: K, status: &str, recorded: bool) -> bool {
        if self.decided.contains(&key) {
            return false;
        }
        // A final INCOMPLETE is tracked as FAILURE; it completes the test, so `recorded` is set anyway.
        let decisive = if self.any_success {
            matches!(status, "PASSED" | "IGNORED" | "ASSUMPTION_FAILURE")
        } else {
            status == "FAILURE"
        };
        let flag = recorded || decisive;
        if flag {
            self.decided.insert(key);
        }
        flag
    }
}
