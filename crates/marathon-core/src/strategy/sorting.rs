//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/strategy/impl/sorting/ExecutionTimeSortingStrategy.kt
//! and impl/sorting/{NoSorting,RandomOrder,SuccessRate}SortingStrategy.kt.
//!
//! The comparators feed `JavaPriorityQueue` only, whose comparison sequence is
//! observable: the order in which a comparator extracts its two keys decides
//! which metrics lookups and random draws happen first.

use super::{MetricsKey, MetricsLookup};
use crate::{model::Test, time::ReplayHooks};
use jcompat::{JavaPriorityQueue, jnum::double_compare, jtime::Instant};
use marathon_config::schema::SortingStrategyConfiguration;
use std::{
    cmp::Ordering,
    collections::HashMap,
    sync::{Arc, Mutex, TryLockError},
};

/// A pool queue's comparator. `Send`, so 3.7 may own the queue wherever it runs.
pub type TestComparator = Box<dyn FnMut(&Test, &Test) -> Ordering + Send>;
pub type TestQueue = JavaPriorityQueue<Test, TestComparator>;

const RANDOM_KEY: &str = "core.RandomOrderSortingStrategy.key";

pub enum SortingStrategy {
    ExecutionTime {
        percentile: f64,
        time_limit: Instant,
    },
    NoSorting,
    /// The key cache belongs to the strategy instance and is never cleared, so
    /// every comparator it hands out shares the keys already drawn.
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

    /// The history queries the comparator can make, for the prefetch.
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

    pub fn process(&self, metrics: Arc<dyn MetricsLookup>, hooks: Arc<dyn ReplayHooks>) -> TestComparator {
        // D34: the heap holds keys ordered by earlier answers, so a comparator's answers must
        // never change. A refresh that lands later reaches the next comparator, not this one.
        let metrics = metrics.view().unwrap_or(metrics);
        match self {
            Self::ExecutionTime { percentile, time_limit } => {
                let (percentile, limit) = (*percentile, *time_limit);
                // Fresh per process call and keyed by Test equality: a queued test keeps
                // the duration it had at its first comparison, even after observed
                // durations change what the provider would return.
                let mut expected = HashMap::<Test, f64>::new();
                let mut key = move |test: &Test| {
                    *expected
                        .entry(test.clone())
                        .or_insert_with(|| metrics.execution_time(test, percentile, limit))
                };
                // comparingDouble(..).reversed() is compare(b, a): b's key comes first.
                Box::new(move |a, b| {
                    let kb = key(b);
                    let ka = key(a);
                    double_compare(kb, ka)
                })
            }
            Self::NoSorting => Box::new(|_, _| Ordering::Equal),
            Self::RandomOrder { cache } => {
                let cache = cache.clone();
                let key = move |test: &Test| {
                    // Comparators for a strategy run serially on the owning core.
                    // Enforce that invariant without a blocking lock on core.
                    let mut cache = cache.try_lock().unwrap_or_else(|error| match error {
                        TryLockError::Poisoned(error) => error.into_inner(),
                        TryLockError::WouldBlock => panic!("random-order comparators must run serially"),
                    });
                    // A failed draw keys the test 0 (WP 3.2 decision 5); the hooks record
                    // the failure and verify_consumed fails the replay.
                    *cache
                        .entry(test.clone())
                        .or_insert_with(|| hooks.draw_int(RANDOM_KEY).unwrap_or(0))
                };
                // comparingInt draws the first argument's key first.
                Box::new(move |a, b| {
                    let ka = key(a);
                    let kb = key(b);
                    ka.cmp(&kb)
                })
            }
            Self::SuccessRate { time_limit, ascending } => {
                let limit = *time_limit;
                // Not memoised: every comparison asks the provider again.
                if *ascending {
                    Box::new(move |a, b| {
                        let ka = metrics.success_rate(a, limit);
                        let kb = metrics.success_rate(b, limit);
                        double_compare(ka, kb)
                    })
                } else {
                    Box::new(move |a, b| {
                        let kb = metrics.success_rate(b, limit);
                        let ka = metrics.success_rate(a, limit);
                        double_compare(kb, ka)
                    })
                }
            }
        }
    }
}
