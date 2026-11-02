//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/strategy/impl/retry/fixedquota/FixedQuotaRetryStrategy.kt
//! and impl/retry/fixedquota/{PoolTestFailureAccumulator,RetryWatchdog}.kt, impl/retry/NoRetryStrategy.kt.
//!
//! Counters are Java `Int`s, so arithmetic wraps.

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
        match self {
            Self::NoRetry => Vec::new(),
            Self::FixedQuota(strategy) => strategy.process(pool, failed, shard, progress),
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
        failed
            .iter()
            .filter(|result| {
                self.accumulator.record(pool, &result.test);
                let flaky = shard.flaky_tests.iter().filter(|t| **t == result.test).count() as i32;
                let failures = self.accumulator.count(pool, &result.test).wrapping_add(flaky);
                // C5: retryPossible accepts left == 0, AddRetry then runs, and only the
                // decrement in requestRetry refuses: a phantom total+1 with no retry.
                self.watchdog.retry_possible(failures)
                    && progress.retry_test(&result.test) != Some(TestAction::Complete)
                    && self.watchdog.request_retry(failures)
            })
            .cloned()
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

    /// Decrements first, then checks; the decrement happens even when refused.
    pub fn request_retry(&mut self, failures: i32) -> bool {
        self.total_allowed_retry_left = self.total_allowed_retry_left.wrapping_sub(1);
        let total_allowed = self.total_allowed_retry_left >= 0;
        total_allowed && failures <= self.max_retry_per_test_quota
    }

    pub fn retry_possible(&self, failures: i32) -> bool {
        self.total_allowed_retry_left >= 0 && failures <= self.max_retry_per_test_quota
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
