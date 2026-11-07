//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/strategy/impl/batching/FixedSizeBatchingStrategy.kt
//! and impl/batching/{ClassName,Isolate}BatchingStrategy.kt, plus the `TestBatch`
//! constructor's id default (test/TestBatch.kt:5).
//!
//! Every queue operation can call the comparator, so the operations, their order
//! and their count follow Kotlin exactly, apart from D50 in `fixed_size`.

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
/// D50: rejected polls a fixed-size batch may make per slot. Two fill a batch from two evenly mixed test APKs,
/// and a run makes about as many polls in all as Kotlin, which spent the same polls over more batches.
const REJECTED_POLLS_PER_SLOT: i64 = 2;

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
    // D50: only accepted tests use a slot or count towards the duration limit. Kotlin (C33) charged rejected
    // polls too, which ended batches early. With nothing rejected, the operations are exactly Kotlin's.
    let mut accepted = 0i32;
    let mut expected_batch_duration = 0.0f64;
    let mut unbatchable = Vec::new();
    // Polling through a long run of another bundle would cost far more than the batch is worth, so rejected
    // polls have their own limit. i64: size can be Integer.MAX_VALUE.
    let rejected_limit = i64::from(size) * REJECTED_POLLS_PER_SLOT;
    let mut rejected = 0i64;
    // A LinkedHashSet: insertion order, with set membership.
    let mut result = Vec::new();
    let mut members = HashSet::new();
    let mut bundle: Option<BundleRef> = None;
    while accepted < size && rejected < rejected_limit {
        let Some(item) = queue.poll() else { break };
        let item_bundle = match identifier {
            Some(identifier) => identifier.identify(&item).map_err(BatchingError::Bundle)?,
            None => None,
        };
        // A flaky copy of a test already in the batch, or a test of another bundle.
        if members.contains(&item) || (bundle.is_some() && bundle != item_bundle) {
            rejected += 1;
            unbatchable.push(item);
            continue;
        }
        // Only a non-null bundle locks the batch: a null-bundle test leaves it open.
        bundle = item_bundle;
        accepted += 1;
        members.insert(item.clone());
        result.push(item.clone());
        if let Some((millis, percentile, limit)) = duration {
            // At least one test is taken even when it alone exceeds the limit.
            expected_batch_duration += metrics.execution_time(&item, percentile, limit);
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
