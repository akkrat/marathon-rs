//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/MetricsProviderImpl.kt
use super::*;
use crate::{error::OperationError, executor::retry::with_retry_send, model::TestStatus};
use crossbeam_skiplist::SkipMap;
use jcompat::jnum::double_to_string;
use marathon_config::Defaults;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::sync::Mutex as AsyncMutex;

#[derive(Clone, Default)]
struct State {
    measurements: Vec<(MetricsKey, Arc<BTreeMap<JavaString, f64>>)>,
}
#[derive(Default)]
struct Observations {
    // Kotlin LinkedHashMap: averaging preserves first-observation order.
    values: Vec<Arc<Mutex<Vec<i64>>>>,
    index: HashMap<JavaString, usize>,
}
pub struct MetricsProviderImpl {
    source: Arc<dyn remote::RemoteDataSource>,
    defaults: Defaults,
    context: MetricsContext,
    serial: AsyncMutex<()>,
    observed: Mutex<Observations>,
    // Publish before removing the old generation. Readers always see a complete
    // immutable snapshot; no blocking lock, no retention of past generations.
    state: SkipMap<u64, Arc<State>>,
    warned_success: SkipMap<JavaString, AtomicBool>,
    warned_time: SkipMap<JavaString, AtomicBool>,
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
            source,
            defaults,
            context,
            serial: AsyncMutex::new(()),
            observed: Mutex::default(),
            state,
            warned_success: SkipMap::new(),
            warned_time: SkipMap::new(),
        })
    }
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
    pub async fn prefetch(&self, keys: &[MetricsKey]) -> OperationResult<()> {
        let _guard = self.serial.lock().await;
        for key in keys {
            if self.snapshot().measurements.iter().any(|(cached, _)| cached == key) {
                continue;
            }
            let (label, description, warning) = match key {
                MetricsKey::SuccessRate { limit } => (
                    "core.MetricsProviderImpl.successRate",
                    format!("Success rate history query (since {limit})"),
                    "Cannot fetch success rate from database",
                ),
                MetricsKey::ExecutionTime { percentile, limit } => (
                    "core.MetricsProviderImpl.executionTimes",
                    format!(
                        "Execution time history query (percentile {} since {limit})",
                        double_to_string(*percentile)
                    ),
                    "Cannot fetch execution time from database",
                ),
            };
            let started = self
                .context
                .monotonic
                .nanos(label)
                .map_err(|e| failure("marathon.ReplayError", &e.message.to_string_lossy()))?;
            let fetched = with_retry_send(
                3,
                50,
                self.context.clock.as_ref(),
                self.context.cancellation.clone(),
                || async {
                    match *key {
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
            if let Err(OperationError::Cancelled(reason)) = fetched {
                return Err(reason.into());
            }
            if let Some(reason) = self.context.cancellation.reason() {
                return Err(reason.into());
            }
            if let Err(OperationError::Failed(error)) = &fetched {
                self.context
                    .log("MetricsProviderImpl", Level::Warn, warning.into(), Some(error.clone()));
            }
            let elapsed = self
                .context
                .monotonic
                .nanos(label)
                .map_err(|e| failure("marathon.ReplayError", &e.message.to_string_lossy()))?
                .wrapping_sub(started)
                / 1_000_000;
            let outcome = fetched
                .as_ref()
                .map_or_else(|_| "failed".to_owned(), |rows| format!("returned {} tests", rows.len()));
            self.context.log(
                "Timings",
                Level::Info,
                format!("{description} {outcome} in {elapsed} ms").into(),
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
                                ("percentile".into(), KotlinValue::Text(double_to_string(*value).into())),
                            ],
                        })
                        .collect(),
                )
                .render();
                self.context.log("MetricsProviderImpl", Level::Debug, message, None);
            }
            let mut next = (*self.snapshot()).clone();
            next.measurements
                .push((*key, Arc::new(fetched.unwrap_or_default().into_iter().collect())));
            self.publish(next);
        }
        Ok(())
    }
    fn measurement<'a>(&self, state: &'a State, key: MetricsKey) -> Option<&'a BTreeMap<JavaString, f64>> {
        let found = state
            .measurements
            .iter()
            .find(|(cached, _)| *cached == key)
            .map(|(_, values)| values.as_ref());
        debug_assert!(found.is_some(), "MetricsKey must be prefetched off core before lookup");
        found
    }
    fn observed(&self, name: &JavaString, p: f64) -> Option<f64> {
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
            .ok_or_else(|| failure("java.lang.ArithmeticException", "long overflow").into())
    }
}
impl MetricsProviderImpl {
    fn execution_time_value(&self, test: &Test, percentile: f64, limit: Instant) -> OperationResult<f64> {
        let state = self.snapshot();
        let name = test.to_safe_test_name();
        if let Some(value) = self
            .measurement(&state, MetricsKey::ExecutionTime { percentile, limit })
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
            let mut message = JavaString::from("No execution time found for ");
            message.append(name);
            message.push_str(&format!(". Using {} ms", double_to_string(expected)));
            self.context.log("MetricsProviderImpl", Level::Debug, message, None);
        }
        Ok(expected)
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
        let state = self.snapshot();
        let name = test.to_safe_test_name();
        if let Some(value) = self
            .measurement(&state, MetricsKey::SuccessRate { limit })
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
            let mut message = JavaString::from("No success rate found for ");
            message.append(name);
            message.push_str(&format!(". Using {}", double_to_string(self.defaults.success_rate)));
            self.context.log("MetricsProviderImpl", Level::Warn, message, None);
        }
        self.defaults.success_rate
    }
    fn execution_time(&self, test: &Test, percentile: f64, limit: Instant) -> f64 {
        // Factory construction validates duration before exposing the core view.
        // Direct async callers retain Kotlin's lazy overflow error.
        self.execution_time_value(test, percentile, limit)
            .unwrap_or_else(|error| std::panic::panic_any(error))
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
            self.execution_time_value(test, percentile, limit)
        })
    }
    fn observe_execution_times<'a>(&'a self, results: &'a [TestResult]) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            if self.defaults.observed_duration_min_tests.is_none() {
                return Ok(());
            }
            for result in results {
                let duration = result.end_time.wrapping_sub(result.start_time);
                if !matches!(result.status, TestStatus::Passed | TestStatus::Failure) || duration < 0 {
                    continue;
                }
                let name = result.test.to_safe_test_name();
                let mut observed = self.observed.lock().unwrap_or_else(|e| e.into_inner());
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
        usage: &'a JavaString,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            let names: BTreeSet<_> = tests.iter().map(Test::to_safe_test_name).collect();
            if names.is_empty() {
                return Ok(());
            }
            let key = MetricsKey::ExecutionTime { percentile, limit };
            self.prefetch(&[key]).await?;
            let state = self.snapshot();
            let measurements = self.measurement(&state, key);
            let with = names
                .iter()
                .filter(|name| measurements.is_some_and(|m| m.contains_key(*name)))
                .count();
            let without = names.len() - with;
            let mut message = JavaString::from("Execution time history for ");
            message.append(usage.clone());
            message.push_str(&format!(
                " (percentile {} since {limit}): {with} of {} tests have it",
                double_to_string(percentile),
                names.len()
            ));
            let mut level = Level::Info;
            if without > 0 {
                let millis = self.default_millis()?;
                match self.defaults.observed_duration_min_tests {
                    None => message.push_str(&format!(", {without} tests are expected to take the default {millis} ms")),
                    Some(min) => message.push_str(&format!(", {without} tests are estimated from this run, using the default {millis} ms until {min} tests have passed or failed")),
                }
                if without * 2 > names.len() {
                    level = Level::Warn;
                    message.push_str(". Most tests have no history; check that the analytics backend can be queried");
                    if self.defaults.observed_duration_min_tests.is_none() {
                        message.push_str(", or set analyticsConfiguration.defaults.duration or analyticsConfiguration.defaults.observedDurationMinTests");
                    }
                }
            }
            self.context.log("MetricsProviderImpl", level, message, None);
            Ok(())
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        self.source.close()
    }
}
