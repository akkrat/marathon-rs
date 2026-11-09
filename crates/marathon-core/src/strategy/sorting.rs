//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/strategy/impl/sorting/ExecutionTimeSortingStrategy.kt
//! and impl/sorting/{NoSorting,RandomOrder,SuccessRate}SortingStrategy.kt.
//!
//! D60 extracts keys on insertion, with a queue-lifetime history view and FIFO ties.

use super::{MetricsKey, MetricsLookup};
use crate::{model::Test, time::ReplayHooks};
use jcompat::jtime::Instant;
use marathon_config::schema::SortingStrategyConfiguration;
use std::{
    cmp::{Ordering, Reverse},
    collections::HashMap,
    sync::{Arc, Mutex, TryLockError},
};

pub type TestKey = super::queue::KeyFunction<Test, SortKey>;
pub type TestQueue = super::queue::OrderedQueue<Test, SortKey>;

/// Canonical NaNs preserve Double.compare's all-NaN ties and placement above infinity.
#[derive(Clone, Copy, Debug)]
pub struct FloatKey(f64);
impl FloatKey {
    pub fn new(value: f64) -> Self {
        Self(if value.is_nan() {
            f64::from_bits(0x7ff8_0000_0000_0000)
        } else {
            value
        })
    }
}
impl PartialEq for FloatKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}
impl Eq for FloatKey {}
impl PartialOrd for FloatKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for FloatKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SortKey {
    Duration(Reverse<FloatKey>),
    Rate(FloatKey),
    DescendingRate(Reverse<FloatKey>),
    Random(i32),
    Constant,
}

const RANDOM_KEY: &str = "core.RandomOrderSortingStrategy.key";

pub enum SortingStrategy {
    ExecutionTime {
        percentile: f64,
        time_limit: Instant,
    },
    NoSorting,
    /// The key cache belongs to the strategy instance and is never cleared, so
    /// every key function it hands out shares the keys already drawn.
    RandomOrder {
        cache: Arc<Mutex<HashMap<Test, i32>>>,
    },
    SuccessRate {
        time_limit: Instant,
        ascending: bool,
    },
}

impl SortingStrategy {
    pub fn from_config(cnf: &SortingStrategyConfiguration) -> Self {
        match cnf {
            SortingStrategyConfiguration::ExecutionTime { percentile, time_limit } => Self::ExecutionTime {
                percentile: *percentile,
                time_limit: *time_limit,
            },
            SortingStrategyConfiguration::NoSorting => Self::NoSorting,
            SortingStrategyConfiguration::RandomOrder => Self::RandomOrder { cache: Arc::default() },
            SortingStrategyConfiguration::SuccessRate { time_limit, ascending } => Self::SuccessRate {
                time_limit: *time_limit,
                ascending: *ascending,
            },
        }
    }

    /// The history queries key extraction can make, for the prefetch.
    pub fn metrics_keys(&self) -> Vec<MetricsKey> {
        match self {
            Self::ExecutionTime { percentile, time_limit } => vec![MetricsKey::ExecutionTime {
                percentile: *percentile,
                limit: *time_limit,
            }],
            Self::SuccessRate { time_limit, .. } => vec![MetricsKey::SuccessRate { limit: *time_limit }],
            Self::NoSorting | Self::RandomOrder { .. } => Vec::new(),
        }
    }

    pub fn process(&self, metrics: Arc<dyn MetricsLookup>, hooks: Arc<dyn ReplayHooks>) -> TestKey {
        // D34 pins backend history for the queue. Run-local observations remain live for new entries.
        let metrics = metrics.view().unwrap_or(metrics);
        match self {
            Self::ExecutionTime { percentile, time_limit } => {
                let (percentile, limit) = (*percentile, *time_limit);
                Box::new(move |test| {
                    SortKey::Duration(Reverse(FloatKey::new(metrics.execution_time(test, percentile, limit))))
                })
            }
            Self::NoSorting => Box::new(|_| SortKey::Constant),
            Self::RandomOrder { cache } => {
                let cache = cache.clone();
                Box::new(move |test| {
                    // Strategy instances run serially on core; do not block the executor on a lock.
                    let mut cache = cache.try_lock().unwrap_or_else(|error| match error {
                        TryLockError::Poisoned(error) => error.into_inner(),
                        TryLockError::WouldBlock => panic!("random-order keys must run serially"),
                    });
                    // Failed draws retain WP 3.2's zero key; replay verification reports the failure.
                    SortKey::Random(
                        *cache
                            .entry(test.clone())
                            .or_insert_with(|| hooks.draw_int(RANDOM_KEY).unwrap_or(0)),
                    )
                })
            }
            Self::SuccessRate { time_limit, ascending } => {
                let (limit, ascending) = (*time_limit, *ascending);
                Box::new(move |test| {
                    let key = FloatKey::new(metrics.success_rate(test, limit));
                    if ascending {
                        SortKey::Rate(key)
                    } else {
                        SortKey::DescendingRate(Reverse(key))
                    }
                })
            }
        }
    }
}
