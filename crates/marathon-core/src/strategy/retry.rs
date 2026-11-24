//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/strategy/impl/retry/fixedquota/FixedQuotaRetryStrategy.kt
//! and impl/retry/fixedquota/{PoolTestFailureAccumulator,RetryWatchdog}.kt, impl/retry/NoRetryStrategy.kt.
//!
//! Failure counters are Java `Int`s, so they wrap. D99 replaced Kotlin's check-then-decrement watchdog (C5).

use crate::model::{DevicePoolId, Test, TestAction, TestResult, TestShard};
use marathon_config::schema::RetryStrategyConfiguration;
use std::collections::HashMap;

/// The one `PoolProgressAccumulator` call the retry strategy makes: `retryTest`,
/// the `AddRetry` transition (WP 3.5 implements it).
pub trait RetryProgress {
    fn retry_test(&mut self, test: &Test) -> Option<TestAction>;
}

/// Built once per pool queue (`QueueActor.kt:48`), so the total quota is per
/// pool, not per run as the configuration docs say (C30).
#[derive(Clone, Debug)]
pub enum RetryStrategy {
    NoRetry,
    FixedQuota(FixedQuotaRetryStrategy),
}

impl RetryStrategy {
    pub fn from_config(cnf: &RetryStrategyConfiguration) -> Self {
        match cnf {
            RetryStrategyConfiguration::FixedQuota {
                total_allowed_retry_quota,
                retry_per_test_quota,
            } => Self::FixedQuota(FixedQuotaRetryStrategy::new(
                *total_allowed_retry_quota,
                *retry_per_test_quota,
            )),
            RetryStrategyConfiguration::NoRetry => Self::NoRetry,
        }
    }

    pub fn process(
        &mut self,
        pool: &DevicePoolId,
        failed: &[TestResult],
        shard: &TestShard,
        progress: &mut dyn RetryProgress,
    ) -> Vec<TestResult> {
        self.retried(pool, failed, shard, progress)
            .into_iter()
            .map(|index| failed[index].clone())
            .collect()
    }

    /// As `process`, by position in `failed`: D69 tells copies of one test apart, which equal
    /// results cannot do (C46).
    pub fn retried(
        &mut self,
        pool: &DevicePoolId,
        failed: &[TestResult],
        shard: &TestShard,
        progress: &mut dyn RetryProgress,
    ) -> Vec<usize> {
        match self {
            Self::NoRetry => Vec::new(),
            Self::FixedQuota(strategy) => strategy.retried(pool, failed, shard, progress),
        }
    }

    /// D69: gives back the total quota a granted retry took, when the queue does not run it
    /// because the attempt it was granted for completed the test (C45). The per-test limit
    /// counts failures, not retries, so it has nothing to give back.
    pub fn refund(&mut self) {
        if let Self::FixedQuota(strategy) = self {
            strategy.watchdog.refund();
        }
    }
}

#[derive(Clone, Debug)]
pub struct FixedQuotaRetryStrategy {
    watchdog: RetryWatchdog,
    accumulator: PoolTestFailureAccumulator,
}

impl FixedQuotaRetryStrategy {
    pub fn new(total_allowed_retry_quota: i32, retry_per_test_quota: i32) -> Self {
        Self {
            watchdog: RetryWatchdog::new(total_allowed_retry_quota, retry_per_test_quota),
            accumulator: PoolTestFailureAccumulator::default(),
        }
    }

    pub fn watchdog(&self) -> &RetryWatchdog {
        &self.watchdog
    }

    pub fn accumulator(&self) -> &PoolTestFailureAccumulator {
        &self.accumulator
    }

    pub fn process(
        &mut self,
        pool: &DevicePoolId,
        failed: &[TestResult],
        shard: &TestShard,
        progress: &mut dyn RetryProgress,
    ) -> Vec<TestResult> {
        self.retried(pool, failed, shard, progress)
            .into_iter()
            .map(|index| failed[index].clone())
            .collect()
    }

    /// The positions in `failed` of the results granted a retry, in order.
    pub fn retried(
        &mut self,
        pool: &DevicePoolId,
        failed: &[TestResult],
        shard: &TestShard,
        progress: &mut dyn RetryProgress,
    ) -> Vec<usize> {
        (0..failed.len())
            .filter(|&index| {
                let result = &failed[index];
                self.accumulator.record(pool, &result.test);
                let flaky = shard.flaky_tests.iter().filter(|t| **t == result.test).count() as i32;
                let failures = self.accumulator.count(pool, &result.test).wrapping_add(flaky);
                // D99 (C5): one call decides and charges, so AddRetry runs only for a retry the quota grants.
                self.watchdog.grant(failures, || {
                    progress.retry_test(&result.test) != Some(TestAction::Complete)
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct RetryWatchdog {
    max_retry_per_test_quota: i32,
    total_allowed_retry_left: i32,
}

impl RetryWatchdog {
    pub fn new(total_allowed_retry_quota: i32, max_retry_per_test_quota: i32) -> Self {
        Self {
            max_retry_per_test_quota,
            total_allowed_retry_left: total_allowed_retry_quota,
        }
    }

    /// Whether a test with `failures` failures gets a retry, charging the total quota when it does.
    ///
    /// D99 (C5): Kotlin checked `left >= 0` before AddRetry and refused only in the decrement after it, so a
    /// failure that met exactly 0 retries left gained a phantom total and never ended its test. Here the one
    /// boundary, `left > 0`, is checked before `add_retry` (AddRetry) runs, and the charge follows it. As in
    /// Kotlin, `add_retry` answering false (the test is already complete) neither grants nor charges.
    pub fn grant(&mut self, failures: i32, add_retry: impl FnOnce() -> bool) -> bool {
        if self.total_allowed_retry_left <= 0 || failures > self.max_retry_per_test_quota || !add_retry() {
            return false;
        }
        self.total_allowed_retry_left -= 1;
        true
    }

    /// D69: undoes the charge of a granted retry that is not run.
    pub fn refund(&mut self) {
        self.total_allowed_retry_left = self.total_allowed_retry_left.wrapping_add(1);
    }

    pub fn total_allowed_retry_left(&self) -> i32 {
        self.total_allowed_retry_left
    }
}

/// Per-pool failure counts by Test equality (metadata ignored).
#[derive(Clone, Debug, Default)]
pub struct PoolTestFailureAccumulator {
    map: HashMap<DevicePoolId, HashMap<Test, i32>>,
}

impl PoolTestFailureAccumulator {
    pub fn record(&mut self, pool: &DevicePoolId, test: &Test) {
        let counter = self
            .map
            .entry(pool.clone())
            .or_default()
            .entry(test.clone())
            .or_insert(0);
        *counter = counter.wrapping_add(1);
    }

    pub fn count(&self, pool: &DevicePoolId, test: &Test) -> i32 {
        self.map
            .get(pool)
            .and_then(|tests| tests.get(test))
            .copied()
            .unwrap_or(0)
    }
}
