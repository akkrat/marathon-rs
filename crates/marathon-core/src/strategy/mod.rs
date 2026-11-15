//! Port of core/src/main/kotlin/com/malinskiy/marathon/extension/ConfigurationExtensions.kt
//! (the strategy factories; `toTestFilter` is WP 3.3's) and the six interfaces in
//! execution/strategy/{Pooling,Sharding,Sorting,Batching,Flakiness,Retry}Strategy.kt.
//!
//! The configuration sets are closed, so each family is an enum built from its
//! configuration enum by `from_config`. Strategies have no equals/hashCode/toString
//! (WP 3.2 P1): nothing at e47352f2 compares, hashes or prints a strategy.

pub mod batching;
pub mod flakiness;
pub mod pooling;
pub mod queue;
pub mod retry;
pub mod sharding;
pub mod sorting;

pub use batching::{BatchingError, BatchingStrategy};
pub use flakiness::FlakinessStrategy;
pub use pooling::PoolingStrategy;
pub use retry::{FixedQuotaRetryStrategy, PoolTestFailureAccumulator, RetryProgress, RetryStrategy, RetryWatchdog};
pub use sharding::ShardingStrategy;
pub use sorting::{SortKey, SortingStrategy, TestKey, TestQueue};

use crate::model::Test;
use jcompat::jtime::Instant;
use std::sync::Arc;

/// The synchronous view of `MetricsProvider` the strategies call, as Kotlin's
/// `MetricsProvider` is synchronous (its implementation blocks).
///
/// WP 3.2 decision 3 (P2, superseding 0.6 D4 for queue construction): the
/// history behind every [`MetricsKey`] a strategy declares is fetched off core
/// before the strategy runs, so implementations answer from memory and never
/// block. Fetch failures are absorbed into Kotlin's defaults by the provider
/// (WP 3.4a), hence infallible results.
///
/// D34: a failed query's history can still arrive later, from off core. Whoever reads
/// more than once and needs the answers to agree reads through [`Self::view`].
pub trait MetricsLookup: Send + Sync {
    fn success_rate(&self, test: &Test, limit: Instant) -> f64;
    fn execution_time(&self, test: &Test, percentile: f64, limit: Instant) -> f64;
    /// A view of the history as it is now, which a later refresh leaves alone.
    /// `None` means this lookup's history never changes, so it can be read directly.
    fn view(&self) -> Option<Arc<dyn MetricsLookup>> {
        None
    }
    /// D71: whether the success-rate history behind `limit` can be read. It cannot when analytics
    /// is disabled, its backend could not be set up, or the query failed or did not finish and has
    /// not recovered since. A working backend's answer counts even when it is empty: tests it does
    /// not name then take the configured default. When the history cannot be read, the first call
    /// in a run logs why, followed by `consequence`.
    fn success_rate_available(&self, limit: Instant, consequence: &str) -> bool {
        let _ = (limit, consequence);
        true
    }
}

/// A history query a strategy may make: the `(percentile, limit)` of
/// `executionTime` or the `limit` of `successRate`.
#[derive(Clone, Copy, Debug)]
pub enum MetricsKey {
    SuccessRate { limit: Instant },
    ExecutionTime { percentile: f64, limit: Instant },
}

// MetricsProviderImpl's MeasurementKey is a Kotlin data class: Double.equals
// canonicalises NaNs and distinguishes the two zeroes for prefetch deduplication.
impl PartialEq for MetricsKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::SuccessRate { limit: a }, Self::SuccessRate { limit: b }) => a == b,
            (
                Self::ExecutionTime {
                    percentile: a,
                    limit: al,
                },
                Self::ExecutionTime {
                    percentile: b,
                    limit: bl,
                },
            ) => jcompat::jnum::double_compare(*a, *b).is_eq() && al == bl,
            _ => false,
        }
    }
}
impl Eq for MetricsKey {}
