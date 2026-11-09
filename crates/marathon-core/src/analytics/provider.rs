//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/MetricsProviderImpl.kt
use super::*;
use crate::{
    error::OperationError,
    executor::{
        Signal,
        retry::with_retry_send,
        shared::{race, requested},
    },
    model::TestStatus,
    runtime::VendorTask,
};
use crossbeam_skiplist::SkipMap;
use futures_util::future::Either;
use jcompat::jnum::double_to_string;
use marathon_config::Defaults;
use marathon_error::FailureKind;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{
        MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Mutex as AsyncMutex;

/// The three attempts and 50 ms of Kotlin's `withRetry`, for a key's first query.
const FIRST_QUERY_ATTEMPTS: i32 = 3;
const FIRST_QUERY_RETRY_DELAY_MILLIS: i64 = 50;
/// D34: the waits before each round of re-queries, about 15 minutes in all. The length is also
/// how many re-queries a failed key gets. A key that fails while rounds are running gets the
/// same number, at the delays still to come and then at the last one.
const REFRESH_DELAYS_MILLIS: [i64; 8] = [5_000, 10_000, 20_000, 40_000, 80_000, 160_000, 300_000, 300_000];
/// D34: how long `close` waits for a cancelled refresh. An async query ends at its next await, at
/// once, so only a source that blocks inside its future takes this long. Waiting too little
/// costs a warning, and a close under a query that can no longer log or publish.
const CLOSE_JOIN_BOUND_MILLIS: i64 = 3_000;

#[derive(Clone, Default)]
struct State {
    measurements: Vec<(MetricsKey, Arc<BTreeMap<String, f64>>)>,
}
#[derive(Default)]
struct Observations {
    // Kotlin LinkedHashMap: averaging preserves first-observation order.
    values: Vec<Arc<Mutex<Vec<i64>>>>,
    index: HashMap<String, usize>,
}
/// D34. Touched off core only, and never held across an await.
#[derive(Default)]
struct Refresh {
    /// Keys whose last query failed, in first-failure order, which is the order a round
    /// queries them in, with the re-queries each has had.
    failed: Vec<(MetricsKey, usize)>,
    /// The one task that re-queries them, so a key is never queried twice at once.
    task: Option<Arc<dyn VendorTask>>,
    closed: bool,
}
pub struct MetricsProviderImpl {
    shared: Arc<Shared>,
    serial: AsyncMutex<()>,
}
struct Shared {
    source: Arc<dyn remote::RemoteDataSource>,
    defaults: Defaults,
    context: MetricsContext,
    observed: Mutex<Observations>,
    // Publish before removing the old generation. Readers always see a complete
    // immutable snapshot; no blocking lock, no retention of past generations.
    state: SkipMap<u64, Arc<State>>,
    warned_success: SkipMap<String, AtomicBool>,
    warned_time: SkipMap<String, AtomicBool>,
    // Also serialises writers of `state`.
    refresh: Mutex<Refresh>,
}
impl MetricsProviderImpl {
    /// Construct off core. Synchronous lookup callers must validate that the
    /// default duration fits milliseconds; MetricsProviderFactory does so before
    /// setup. Direct async callers can retain Kotlin's lazy overflow failure.
    pub fn new(
        source: Arc<dyn remote::RemoteDataSource>,
        defaults: Defaults,
        context: MetricsContext,
    ) -> OperationResult<Self> {
        context.register("MetricsProviderImpl")?;
        context.timings()?;
        let state = SkipMap::new();
        state.insert(0, Arc::new(State::default()));
        Ok(Self {
            shared: Arc::new(Shared {
                source,
                defaults,
                context,
                observed: Mutex::default(),
                state,
                warned_success: SkipMap::new(),
                warned_time: SkipMap::new(),
                refresh: Mutex::default(),
            }),
            serial: AsyncMutex::new(()),
        })
    }
    /// A failed query never waits here for its refresh: the cached defaults answer until one lands.
    pub async fn prefetch(&self, keys: &[MetricsKey]) -> OperationResult<()> {
        let shared = &self.shared;
        let _guard = self.serial.lock().await;
        for key in keys {
            if shared.snapshot().measurements.iter().any(|(cached, _)| cached == key) {
                continue;
            }
            let fetched = shared
                .fetch(
                    *key,
                    FIRST_QUERY_ATTEMPTS,
                    FIRST_QUERY_RETRY_DELAY_MILLIS,
                    shared.context.cancellation.clone(),
                )
                .await?;
            let mut refresh = shared.refresh();
            match fetched {
                Ok(rows) => shared.publish_values(&refresh, *key, rows),
                Err(_) => {
                    shared.publish_values(&refresh, *key, Vec::new());
                    shared.schedule_refresh(&mut refresh, *key);
                }
            }
        }
        Ok(())
    }
}
/// Clears a refresh that stops early, however it stops: at a cancelled await, or dropped by the
/// run's cancellation, at an await or before its first poll. A stale `task` would stop the next
/// failure starting a refresh. A refresh that runs out of keys disarms this under the lock, since
/// by the time this drops, a new failure may have started the next task.
struct RefreshEnd<'a> {
    shared: &'a Shared,
    armed: bool,
}
impl Drop for RefreshEnd<'_> {
    fn drop(&mut self) {
        if self.armed {
            let mut refresh = self.shared.refresh();
            refresh.failed.clear();
            refresh.task = None;
            drop(refresh);
            if std::thread::panicking() {
                self.shared.context.log(
                    "MetricsProviderImpl",
                    Level::Warn,
                    "The history refresh panicked, so its failed queries are not tried again. Tests without history use the defaults".into(),
                    None,
                );
            }
        }
    }
}
/// The history of one moment: what a comparator keeps for its queue's lifetime, and a batch
/// or a flakiness pass for its own (D34). What a test observed in this run stays live.
struct PinnedMetrics {
    shared: Arc<Shared>,
    state: Arc<State>,
}
impl Shared {
    fn snapshot(&self) -> Arc<State> {
        self.state
            .back()
            .expect("initial snapshot is never removed before replacement")
            .value()
            .clone()
    }
    fn publish(&self, state: State) {
        let old = self.state.back().expect("initialized snapshot");
        self.state.insert(old.key() + 1, Arc::new(state));
        old.remove();
    }
    fn refresh(&self) -> MutexGuard<'_, Refresh> {
        self.refresh.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn publish_values(&self, _writer: &MutexGuard<'_, Refresh>, key: MetricsKey, rows: Vec<(String, f64)>) {
        let values = Arc::new(rows.into_iter().collect());
        let mut next = (*self.snapshot()).clone();
        match next.measurements.iter_mut().find(|(cached, _)| *cached == key) {
            Some(measurement) => measurement.1 = values,
            None => next.measurements.push((key, values)),
        }
        self.publish(next);
    }
    fn describe(key: MetricsKey) -> (&'static str, String, &'static str) {
        match key {
            MetricsKey::SuccessRate { limit } => (
                "core.MetricsProviderImpl.successRate",
                format!("Success rate history query (since {limit})"),
                "Cannot fetch success rate from database",
            ),
            MetricsKey::ExecutionTime { percentile, limit } => (
                "core.MetricsProviderImpl.executionTimes",
                format!(
                    "Execution time history query (percentile {} since {limit})",
                    double_to_string(percentile)
                ),
                "Cannot fetch execution time from database",
            ),
        }
    }
    /// One query with its logs. The outer error is a cancellation or a replay failure, which
    /// logs nothing; the inner one is a backend failure, already logged.
    async fn fetch(
        &self,
        key: MetricsKey,
        attempts: i32,
        retry_delay_millis: i64,
        cancellation: Arc<dyn CancellationSignal>,
    ) -> OperationResult<Result<Vec<(String, f64)>, Failure>> {
        let (label, description, warning) = Self::describe(key);
        let started = self
            .context
            .monotonic
            .nanos(label)
            .map_err(|e| failure(FailureKind::Replay, &e.message.to_owned()))?;
        let fetched = with_retry_send(
            attempts,
            retry_delay_millis,
            self.context.clock.as_ref(),
            cancellation.clone(),
            || async {
                match key {
                    MetricsKey::SuccessRate { limit } => self
                        .source
                        .request_all_success_rates(limit)
                        .await
                        .map(|v| v.into_iter().map(|v| (v.test_name, v.mean)).collect::<Vec<_>>()),
                    MetricsKey::ExecutionTime { percentile, limit } => self
                        .source
                        .request_all_execution_times(percentile, limit)
                        .await
                        .map(|v| v.into_iter().map(|v| (v.test_name, v.percentile)).collect()),
                }
            },
        )
        .await;
        let fetched = match fetched {
            Ok(rows) => Ok(rows),
            Err(OperationError::Cancelled(reason)) => return Err(reason.into()),
            Err(OperationError::Failed(error)) => Err(error),
        };
        if let Some(reason) = cancellation.reason().or_else(|| self.context.cancellation.reason()) {
            return Err(reason.into());
        }
        if let Err(error) = &fetched {
            self.context
                .log("MetricsProviderImpl", Level::Warn, warning.into(), Some(error.clone()));
        }
        let elapsed = self
            .context
            .monotonic
            .nanos(label)
            .map_err(|e| failure(FailureKind::Replay, &e.message.to_owned()))?
            .wrapping_sub(started)
            / 1_000_000;
        let outcome = fetched
            .as_ref()
            .map_or_else(|_| "failed".to_owned(), |rows| format!("returned {} tests", rows.len()));
        self.context.log(
            "Timings",
            Level::Info,
            format!("{description} {outcome} in {elapsed} ms"),
            None,
        );
        if let (MetricsKey::ExecutionTime { .. }, Ok(rows)) = (key, &fetched) {
            use jcompat::writers::KotlinValue;
            let message = KotlinValue::List(
                rows.iter()
                    .map(|(name, value)| KotlinValue::DataClass {
                        name: "ExecutionTime".into(),
                        fields: vec![
                            ("testName".into(), KotlinValue::Text(name.clone())),
                            ("percentile".into(), KotlinValue::Text(double_to_string(*value))),
                        ],
                    })
                    .collect(),
            )
            .render();
            self.context.log("MetricsProviderImpl", Level::Debug, message, None);
        }
        Ok(fetched)
    }
    fn schedule_refresh(self: &Arc<Self>, refresh: &mut MutexGuard<'_, Refresh>, key: MetricsKey) {
        if refresh.closed {
            return;
        }
        refresh.failed.push((key, 0));
        if refresh.task.is_none() {
            let shared = self.clone();
            // On the vendor runtime, so replay's quiescence sees it like any other vendor work.
            refresh.task = Some(self.context.vendor.spawn_cancellable(Box::new(move |signal| {
                Box::pin(async move {
                    // Outside the race: the run's cancellation can win before `run_refresh` first runs.
                    let mut end = RefreshEnd {
                        shared: &shared,
                        armed: true,
                    };
                    let run = shared.context.cancellation.clone();
                    race(run.cancelled(), shared.run_refresh(signal, &mut end)).await;
                    Ok(())
                })
            })));
        }
    }
    /// Rounds, not one timer per key: one query at a time, in an order that depends only on the
    /// order the keys failed in. The refresh ends once no key is left to retry.
    async fn run_refresh(&self, signal: Arc<dyn CancellationSignal>, end: &mut RefreshEnd<'_>) {
        for round in 0.. {
            let delay = REFRESH_DELAYS_MILLIS[round.min(REFRESH_DELAYS_MILLIS.len() - 1)];
            if self.context.clock.delay(delay, signal.clone()).await.is_err() {
                // Cancelled, or its clock failed.
                return;
            }
            let keys: Vec<MetricsKey> = self.refresh().failed.iter().map(|(key, _)| *key).collect();
            for key in keys {
                // One attempt: the delays above are the retries.
                let Ok(fetched) = self.fetch(key, 1, 0, signal.clone()).await else {
                    // Cancelled, as in prefetch: nothing logged or cached. The key keeps its defaults.
                    return;
                };
                self.refreshed(key, fetched);
            }
            // Under the lock a new failure registers under, so none is left without a task.
            let mut refresh = self.refresh();
            if refresh.failed.is_empty() {
                refresh.task = None;
                end.armed = false;
                return;
            }
        }
    }
    fn refreshed(&self, key: MetricsKey, fetched: Result<Vec<(String, f64)>, Failure>) {
        let (_, description, _) = Self::describe(key);
        let mut refresh = self.refresh();
        let Some(index) = refresh.failed.iter().position(|(failed, _)| *failed == key) else {
            return;
        };
        match fetched {
            Ok(rows) => {
                let count = rows.len();
                refresh.failed.remove(index);
                self.publish_values(&refresh, key, rows);
                drop(refresh);
                // Still logged when empty: it says why this key's warnings have stopped.
                let effect = if count == 0 {
                    "There is no history to use, so nothing changes"
                } else {
                    "Queues built and batches composed from now on use it; a queue that already exists keeps its order"
                };
                self.context.log(
                    "MetricsProviderImpl",
                    Level::Info,
                    format!("{description} returned {count} tests after failing earlier. {effect}"),
                    None,
                );
            }
            Err(_) => {
                refresh.failed[index].1 += 1;
                if refresh.failed[index].1 < REFRESH_DELAYS_MILLIS.len() {
                    return;
                }
                refresh.failed.remove(index);
                drop(refresh);
                self.context.log(
                    "MetricsProviderImpl",
                    Level::Warn,
                    format!(
                        "{description} failed {} more times. Tests without history use the defaults for the rest of this run",
                        REFRESH_DELAYS_MILLIS.len()
                    ),
                    None,
                );
            }
        }
    }
    /// Joined so that nothing queries the source once it closes. Cancelling drops any query in
    /// flight at its next await, so this does not wait for the backend. A source that blocks
    /// inside its future never reaches an await, so the wait has a bound of its own.
    async fn stop_refresh(&self) {
        let task = {
            let mut refresh = self.refresh();
            refresh.closed = true;
            refresh.failed.clear();
            refresh.task.take()
        };
        let Some(task) = task else {
            return;
        };
        task.cancel(requested("The metrics provider is closed"));
        // Not the run's signal: its cancellation must not end this wait early.
        let bound = self.context.clock.delay(CLOSE_JOIN_BOUND_MILLIS, Signal::new());
        match race(task.join(), bound).await {
            Either::Left(_) => {}
            Either::Right(Ok(())) => self.context.log(
                "MetricsProviderImpl",
                Level::Warn,
                format!(
                    "The history refresh did not stop within {CLOSE_JOIN_BOUND_MILLIS} ms of being cancelled. Closing the analytics backend with its query still running"
                ),
                None,
            ),
            // A failed clock cannot bound anything, and only a replay has one that fails.
            Either::Right(Err(_)) => {
                let _ = task.join().await;
            }
        }
    }
    fn measurement<'a>(&self, state: &'a State, key: MetricsKey) -> Option<&'a BTreeMap<String, f64>> {
        let found = state
            .measurements
            .iter()
            .find(|(cached, _)| *cached == key)
            .map(|(_, values)| values.as_ref());
        debug_assert!(found.is_some(), "MetricsKey must be prefetched off core before lookup");
        found
    }
    fn observed(&self, name: &str, p: f64) -> Option<f64> {
        let min = self.defaults.observed_duration_min_tests?;
        let observed = self.observed.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = observed.index.get(name) {
            let values = observed.values[*index].clone();
            drop(observed);
            return Some(observed_percentile(&values, p));
        }
        if (observed.values.len() as i64) < i64::from(min) {
            return None;
        }
        let values = observed.values.clone();
        drop(observed);
        Some(values.iter().map(|v| observed_percentile(v, p)).sum::<f64>() / values.len() as f64)
    }
    fn default_millis(&self) -> OperationResult<i64> {
        self.defaults
            .duration
            .to_millis()
            .ok_or_else(|| failure(FailureKind::Arithmetic, "long overflow").into())
    }
    fn execution_time_value(
        &self,
        state: &State,
        test: &Test,
        percentile: f64,
        limit: Instant,
    ) -> OperationResult<f64> {
        let name = test.to_safe_test_name();
        if let Some(value) = self
            .measurement(state, MetricsKey::ExecutionTime { percentile, limit })
            .and_then(|m| m.get(&name))
        {
            return Ok(*value);
        }
        let expected = match self.observed(&name, percentile) {
            Some(value) => value,
            None => self.default_millis()? as f64,
        };
        if !self
            .warned_time
            .get_or_insert(name.clone(), AtomicBool::new(false))
            .value()
            .swap(true, Ordering::Relaxed)
        {
            let mut message = String::from("No execution time found for ");
            message.push_str(&name);
            message.push_str(&format!(". Using {} ms", double_to_string(expected)));
            self.context.log("MetricsProviderImpl", Level::Debug, message, None);
        }
        Ok(expected)
    }
    fn success_rate_value(&self, state: &State, test: &Test, limit: Instant) -> f64 {
        let name = test.to_safe_test_name();
        if let Some(value) = self
            .measurement(state, MetricsKey::SuccessRate { limit })
            .and_then(|m| m.get(&name))
        {
            return *value;
        }
        if !self
            .warned_success
            .get_or_insert(name.clone(), AtomicBool::new(false))
            .value()
            .swap(true, Ordering::Relaxed)
        {
            let mut message = String::from("No success rate found for ");
            message.push_str(&name);
            message.push_str(&format!(". Using {}", double_to_string(self.defaults.success_rate)));
            self.context.log("MetricsProviderImpl", Level::Warn, message, None);
        }
        self.defaults.success_rate
    }
    // Factory construction validates duration before exposing the core view.
    // Direct async callers retain Kotlin's lazy overflow error.
    fn execution_time_or_panic(&self, state: &State, test: &Test, percentile: f64, limit: Instant) -> f64 {
        self.execution_time_value(state, test, percentile, limit)
            .unwrap_or_else(|error| std::panic::panic_any(error))
    }
}
impl Drop for MetricsProviderImpl {
    fn drop(&mut self) {
        // A run that ends without close leaves no refresh querying a provider nobody can read.
        if let Some(task) = self.shared.refresh().task.take() {
            task.cancel(requested("The metrics provider is gone"));
        }
    }
}

fn observed_percentile(values: &Mutex<Vec<i64>>, percentile: f64) -> f64 {
    // Copy only this test's samples; sorting never holds a provider lock.
    let mut sorted = values.lock().unwrap_or_else(|e| e.into_inner()).clone();
    sorted.sort();
    // NaN.toInt() == 0; Rust's saturating float cast has that same behaviour.
    let rank =
        ((percentile.clamp(0.0, 100.0) / 100.0 * sorted.len() as f64).ceil() as i32).clamp(1, sorted.len() as i32);
    sorted[(rank - 1) as usize] as f64
}
impl MetricsLookup for MetricsProviderImpl {
    fn success_rate(&self, test: &Test, limit: Instant) -> f64 {
        self.shared.success_rate_value(&self.shared.snapshot(), test, limit)
    }
    fn execution_time(&self, test: &Test, percentile: f64, limit: Instant) -> f64 {
        self.shared
            .execution_time_or_panic(&self.shared.snapshot(), test, percentile, limit)
    }
    fn view(&self) -> Option<Arc<dyn MetricsLookup>> {
        Some(Arc::new(PinnedMetrics {
            shared: self.shared.clone(),
            state: self.shared.snapshot(),
        }))
    }
}
impl MetricsLookup for PinnedMetrics {
    fn success_rate(&self, test: &Test, limit: Instant) -> f64 {
        self.shared.success_rate_value(&self.state, test, limit)
    }
    fn execution_time(&self, test: &Test, percentile: f64, limit: Instant) -> f64 {
        self.shared
            .execution_time_or_panic(&self.state, test, percentile, limit)
    }
}
impl MetricsProvider for MetricsProviderImpl {
    fn success_rate<'a>(&'a self, test: &'a Test, limit: Instant) -> ServiceFuture<'a, f64> {
        Box::pin(async move {
            self.prefetch(&[MetricsKey::SuccessRate { limit }]).await?;
            Ok(MetricsLookup::success_rate(self, test, limit))
        })
    }
    fn execution_time<'a>(&'a self, test: &'a Test, percentile: f64, limit: Instant) -> ServiceFuture<'a, f64> {
        Box::pin(async move {
            self.prefetch(&[MetricsKey::ExecutionTime { percentile, limit }])
                .await?;
            self.shared
                .execution_time_value(&self.shared.snapshot(), test, percentile, limit)
        })
    }
    fn observe_execution_times<'a>(&'a self, results: &'a [TestResult]) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            if self.shared.defaults.observed_duration_min_tests.is_none() {
                return Ok(());
            }
            for result in results {
                let duration = result.end_time.wrapping_sub(result.start_time);
                if !matches!(result.status, TestStatus::Passed | TestStatus::Failure) || duration < 0 {
                    continue;
                }
                let name = result.test.to_safe_test_name();
                let mut observed = self.shared.observed.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(index) = observed.index.get(&name) {
                    let values = observed.values[*index].clone();
                    drop(observed);
                    values.lock().unwrap_or_else(|e| e.into_inner()).push(duration);
                } else {
                    let index = observed.values.len();
                    observed.values.push(Arc::new(Mutex::new(vec![duration])));
                    observed.index.insert(name, index);
                }
            }
            Ok(())
        })
    }
    fn report_execution_time_coverage<'a>(
        &'a self,
        tests: &'a [Test],
        percentile: f64,
        limit: Instant,
        usage: &'a str,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            let names: BTreeSet<_> = tests.iter().map(Test::to_safe_test_name).collect();
            if names.is_empty() {
                return Ok(());
            }
            let key = MetricsKey::ExecutionTime { percentile, limit };
            self.prefetch(&[key]).await?;
            let state = self.shared.snapshot();
            let measurements = self.shared.measurement(&state, key);
            let with = names
                .iter()
                .filter(|name| measurements.is_some_and(|m| m.contains_key(*name)))
                .count();
            let without = names.len() - with;
            let mut message = String::from("Execution time history for ");
            message.push_str(usage);
            message.push_str(&format!(
                " (percentile {} since {limit}): {with} of {} tests have it",
                double_to_string(percentile),
                names.len()
            ));
            let mut level = Level::Info;
            if without > 0 {
                let millis = self.shared.default_millis()?;
                match self.shared.defaults.observed_duration_min_tests {
                    None => message.push_str(&format!(", {without} tests are expected to take the default {millis} ms")),
                    Some(min) => message.push_str(&format!(", {without} tests are estimated from this run, using the default {millis} ms until {min} tests have passed or failed")),
                }
                if without * 2 > names.len() {
                    level = Level::Warn;
                    message.push_str(". Most tests have no history; check that the analytics backend can be queried");
                    if self.shared.defaults.observed_duration_min_tests.is_none() {
                        message.push_str(", or set analyticsConfiguration.defaults.duration or analyticsConfiguration.defaults.observedDurationMinTests");
                    }
                }
            }
            self.shared.context.log("MetricsProviderImpl", level, message, None);
            Ok(())
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async move {
            self.shared.stop_refresh().await;
            self.shared.source.close().await
        })
    }
}
