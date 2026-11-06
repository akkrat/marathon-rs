//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/strategy/impl/batching/FixedSizeBatchingStrategy.kt
//! and impl/batching/{ClassName,Isolate}BatchingStrategy.kt, plus the `TestBatch`
//! constructor's id default (test/TestBatch.kt:5).
//!
//! Every queue operation can call the comparator, so the operations, their order
//! and their count follow Kotlin exactly.

use super::{MetricsKey, MetricsLookup};
use crate::{
    error::VendorError,
    model::{Test, TestBatch},
    time::{ReplayError, ReplayHooks},
    vendor::{BundleRef, TestBundleIdentifier},
};
use jcompat::{JavaPriorityQueue, jtime::Instant};
use marathon_config::schema::BatchingStrategyConfiguration;
use std::{cmp::Ordering, collections::HashSet};

const BATCH_ID: &str = "core.TestBatch.id";

#[derive(Clone, Debug)]
pub enum BatchingStrategy {
    FixedSize {
        size: i32,
        duration_millis: Option<i64>,
        percentile: Option<f64>,
        time_limit: Option<Instant>,
        last_mile_length: i32,
    },
    ClassName,
    Isolate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchingError {
    /// QueueActor checks emptiness before asking for a batch (lifecycle.md), so
    /// this is unreachable from the run. Kotlin differs per strategy here:
    /// Isolate builds a batch of one null test, FixedSize an empty batch with a
    /// fresh id, and ClassName throws an NPE from `peek()!!`.
    EmptyQueue,
    /// `TestBundleIdentifier.identify` threw. The tests FixedSize had already
    /// polled are lost with the exception, as in Kotlin.
    Bundle(VendorError),
    Replay(ReplayError),
}

impl BatchingStrategy {
    pub fn from_config(cnf: &BatchingStrategyConfiguration) -> Self {
        match cnf {
            BatchingStrategyConfiguration::FixedSize {
                size,
                duration_millis,
                percentile,
                time_limit,
                last_mile_length,
            } => Self::FixedSize {
                size: *size,
                duration_millis: *duration_millis,
                percentile: *percentile,
                time_limit: *time_limit,
                last_mile_length: *last_mile_length,
            },
            BatchingStrategyConfiguration::ClassName => Self::ClassName,
            BatchingStrategyConfiguration::Isolate => Self::Isolate,
        }
    }

    /// The history queries `process` can make, for the prefetch.
    pub fn metrics_keys(&self) -> Vec<MetricsKey> {
        match self {
            Self::FixedSize {
                duration_millis: Some(_),
                percentile: Some(percentile),
                time_limit: Some(limit),
                ..
            } => vec![MetricsKey::ExecutionTime {
                percentile: *percentile,
                limit: *limit,
            }],
            _ => Vec::new(),
        }
    }

    pub fn process<C>(
        &self,
        queue: &mut JavaPriorityQueue<Test, C>,
        metrics: &dyn MetricsLookup,
        identifier: Option<&dyn TestBundleIdentifier>,
        hooks: &dyn ReplayHooks,
    ) -> Result<TestBatch, BatchingError>
    where
        C: FnMut(&Test, &Test) -> Ordering,
    {
        if queue.is_empty() {
            return Err(BatchingError::EmptyQueue);
        }
        // D34: a refresh reaches the next batch, never half of this one. Batching only reads
        // durations, so unlike a comparator it can take new history at every batch.
        let view = metrics.view();
        let metrics = view.as_deref().unwrap_or(metrics);
        let tests = match self {
            Self::FixedSize {
                size,
                duration_millis,
                percentile,
                time_limit,
                last_mile_length,
            } => {
                let duration = match (duration_millis, percentile, time_limit) {
                    (Some(d), Some(p), Some(t)) => Some((*d, *p, *t)),
                    _ => None,
                };
                fixed_size(queue, *size, duration, *last_mile_length, metrics, identifier)?
            }
            Self::ClassName => {
                let selector = queue.peek().map(|t| t.to_class_name('.'));
                // toList() is backing-array order; the batch keeps it, duplicates included.
                let group: Vec<Test> = queue
                    .iter()
                    .filter(|t| Some(t.to_class_name('.')) == selector)
                    .cloned()
                    .collect();
                queue.remove_all(&group);
                group
            }
            Self::Isolate => queue.poll().into_iter().collect(),
        };
        // The UUID default parameter runs when TestBatch is constructed, after the
        // strategy's last queue operation.
        let id = hooks.allocate_id(BATCH_ID).map_err(BatchingError::Replay)?;
        Ok(TestBatch { tests, id })
    }
}

fn fixed_size<C>(
    queue: &mut JavaPriorityQueue<Test, C>,
    size: i32,
    duration: Option<(i64, f64, Instant)>,
    last_mile_length: i32,
    metrics: &dyn MetricsLookup,
    identifier: Option<&dyn TestBundleIdentifier>,
) -> Result<Vec<Test>, BatchingError>
where
    C: FnMut(&Test, &Test) -> Ordering,
{
    // Last mile: below the threshold batching stops so the tail runs in parallel.
    if i64::try_from(queue.len()).unwrap_or(i64::MAX) < i64::from(last_mile_length) {
        return Ok(queue.poll().into_iter().collect());
    }
    let mut counter = 0i32;
    let mut expected_batch_duration = 0.0f64;
    let mut unbatchable = Vec::new();
    // A LinkedHashSet: insertion order, with set membership.
    let mut result = Vec::new();
    let mut members = HashSet::new();
    let mut bundle: Option<BundleRef> = None;
    while counter < size {
        let Some(item) = queue.poll() else { break };
        // Every poll counts, including the ones that end up unbatchable.
        counter = counter.wrapping_add(1);
        let item_bundle = match identifier {
            Some(identifier) => identifier.identify(&item).map_err(BatchingError::Bundle)?,
            None => None,
        };
        if members.contains(&item) {
            // A flaky copy of a test already in the batch.
            unbatchable.push(item.clone());
        } else if bundle.is_some() && bundle != item_bundle {
            unbatchable.push(item.clone());
        } else {
            // Only a non-null bundle locks the batch: a null-bundle test leaves it open.
            bundle = item_bundle;
            members.insert(item.clone());
            result.push(item.clone());
        }
        if let Some((millis, percentile, limit)) = duration {
            let expected = metrics.execution_time(&item, percentile, limit);
            // Every polled item adds its duration, unbatchable ones too; at least one
            // test is taken even when it alone exceeds the limit.
            expected_batch_duration += expected;
            if expected_batch_duration >= millis as f64 {
                break;
            }
        }
    }
    if !unbatchable.is_empty() {
        queue.add_all(unbatchable);
    }
    Ok(result)
}
