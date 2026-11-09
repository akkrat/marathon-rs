//! Port of core/src/test/kotlin/com/malinskiy/marathon/MetricsProviderStub.kt
//! and TestGenerator.kt, TestResultsGenerator.kt, DeviceInfoGenerator.kt, device/DeviceStub.kt,
//! plus the recording doubles that replay fixtures/oracle/strategies.
#![allow(dead_code)]

use jcompat::{
    jnum,
    jtime::Instant,
    logging::{Logger, MarathonLogging},
};
use marathon_config::Configuration;
use marathon_core::{
    error::VendorResult,
    model::*,
    runtime::{Deferred, VendorFuture},
    strategy::{MetricsLookup, RetryProgress, TestKey},
    time::*,
    vendor::{BundleRef, Device, TestBundle, TestBundleIdentifier, VendorContext},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

// ---- TestGenerator.kt / TestResultsGenerator.kt / DeviceInfoGenerator.kt ---------------------------------

pub fn generate_test(pkg: &str, clazz: &str, method: &str) -> Test {
    Test {
        pkg: pkg.into(),
        clazz: clazz.into(),
        method: method.into(),
        meta_properties: Vec::new(),
    }
}
pub fn default_test() -> Test {
    generate_test("pkg", "clazz", "method")
}
pub fn generate_tests(count: usize) -> Vec<Test> {
    generate_tests_in("pkg", count)
}
pub fn generate_tests_in(pkg: &str, count: usize) -> Vec<Test> {
    (0..count)
        .map(|i| generate_test(&format!("{pkg}{i}"), &format!("clazz{i}"), &format!("method{i}")))
        .collect()
}
pub fn generate_class_group_tests(class_counts: usize, method_counts: usize) -> Vec<Test> {
    let mut tests = Vec::new();
    for class in 0..class_counts {
        for method in 0..method_counts {
            tests.push(generate_test(
                "pkg",
                &format!("clazz{class}"),
                &format!("method{method}"),
            ));
        }
    }
    tests
}
pub fn create_device_info() -> DeviceInfo {
    DeviceInfo {
        operating_system: OperatingSystem {
            version: "Fake OS".into(),
        },
        serial_number: "fake serial".into(),
        model: "fake model".into(),
        manufacturer: "fake manufacturer".into(),
        network_state: NetworkState::Connected,
        device_features: Vec::new(),
        healthy: true,
    }
}
pub fn generate_test_result(test: &Test, status: TestStatus) -> TestResult {
    TestResult {
        test: test.clone(),
        device: create_device_info(),
        test_batch_id: "stub-batch".into(),
        status,
        start_time: 0,
        end_time: 10000,
        stacktrace: None,
        attachments: Vec::new(),
    }
}
pub fn generate_test_results(tests: &[Test]) -> Vec<TestResult> {
    tests
        .iter()
        .map(|t| generate_test_result(t, TestStatus::Passed))
        .collect()
}
pub fn limit() -> Instant {
    jcompat::jtime::instant_of(1_767_225_600, 0).expect("2026-01-01T00:00:00Z")
}

// ---- MetricsProviderStub.kt ----------------------------------------------------------------------------

/// Map lookups with the Kotlin defaults 0.5 and 1500.0. The maps can change
/// after a queue was built, as the Kotlin test's mutable map does.
pub struct MetricsProviderStub {
    pub success_rates: Mutex<HashMap<Test, f64>>,
    pub execution_times: Mutex<HashMap<Test, f64>>,
    pub success_rate: f64,
    pub execution_time: f64,
}
impl Default for MetricsProviderStub {
    fn default() -> Self {
        Self {
            success_rates: Mutex::default(),
            execution_times: Mutex::default(),
            success_rate: 0.5,
            execution_time: 1500.0,
        }
    }
}
impl MetricsProviderStub {
    pub fn with_success_rate(success_rate: f64) -> Self {
        Self {
            success_rate,
            ..Self::default()
        }
    }
    pub fn with_success_rates(rates: impl IntoIterator<Item = (Test, f64)>) -> Self {
        Self {
            success_rates: Mutex::new(rates.into_iter().collect()),
            ..Self::default()
        }
    }
    pub fn with_execution_times(times: impl IntoIterator<Item = (Test, f64)>) -> Self {
        Self {
            execution_times: Mutex::new(times.into_iter().collect()),
            ..Self::default()
        }
    }
    pub fn set_execution_time(&self, test: &Test, value: f64) {
        lock(&self.execution_times).insert(test.clone(), value);
    }
}
impl MetricsLookup for MetricsProviderStub {
    fn success_rate(&self, test: &Test, _limit: Instant) -> f64 {
        lock(&self.success_rates)
            .get(test)
            .copied()
            .unwrap_or(self.success_rate)
    }
    fn execution_time(&self, test: &Test, _percentile: f64, _limit: Instant) -> f64 {
        lock(&self.execution_times)
            .get(test)
            .copied()
            .unwrap_or(self.execution_time)
    }
}
pub fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---- device/DeviceStub.kt ------------------------------------------------------------------------------

pub struct DeviceStub {
    pub info: DeviceInfo,
    pub abi: String,
    logger: Logger,
}
impl DeviceStub {
    /// OS "25", abi "x64", model "model", manufacturer "manufacturer".
    pub fn new() -> Self {
        Self {
            info: DeviceInfo {
                operating_system: OperatingSystem { version: "25".into() },
                serial_number: "serialNumber".into(),
                model: "model".into(),
                manufacturer: "manufacturer".into(),
                network_state: NetworkState::Connected,
                device_features: Vec::new(),
                healthy: true,
            },
            abi: "x64".into(),
            logger: MarathonLogging::default().logger("DeviceStub", None),
        }
    }
    pub fn abi(mut self, abi: &str) -> Self {
        self.abi = abi.into();
        self
    }
    pub fn model(mut self, model: &str) -> Self {
        self.info.model = model.into();
        self
    }
    pub fn manufacturer(mut self, manufacturer: &str) -> Self {
        self.info.manufacturer = manufacturer.into();
        self
    }
    pub fn operating_system(mut self, version: &str) -> Self {
        self.info.operating_system = OperatingSystem {
            version: version.into(),
        };
        self
    }
    pub fn serial(mut self, serial: &str, network_state: NetworkState, healthy: bool) -> Self {
        self.info.serial_number = serial.into();
        self.info.network_state = network_state;
        self.info.healthy = healthy;
        self
    }
}
impl Device for DeviceStub {
    fn info(&self) -> DeviceInfo {
        self.info.clone()
    }
    fn abi(&self) -> String {
        self.abi.clone()
    }
    fn worker_id(&self) -> Option<String> {
        None
    }
    fn logger(&self) -> &Logger {
        &self.logger
    }
    fn prepare<'a>(&'a self, _configuration: Arc<Configuration>, _context: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn execute<'a>(
        &'a self,
        _configuration: Arc<Configuration>,
        _pool: DevicePoolId,
        _batch: BatchRef,
        _result: Arc<dyn Deferred<TestBatchResults>>,
        _context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

// ---- bundles -------------------------------------------------------------------------------------------

pub struct StubTestBundle(pub String);
impl TestBundle for StubTestBundle {
    fn id(&self) -> &str {
        &self.0
    }
}
pub fn bundle(id: &str) -> BundleRef {
    BundleRef(Arc::new(StubTestBundle(id.into())))
}
#[derive(Default)]
pub struct StubTestBundleIdentifier {
    pub bundles: Vec<(Test, Option<BundleRef>)>,
    pub trace: Option<Trace>,
}
impl StubTestBundleIdentifier {
    pub fn put(&mut self, test: &Test, bundle: BundleRef) {
        self.bundles.push((test.clone(), Some(bundle)));
    }
}
impl TestBundleIdentifier for StubTestBundleIdentifier {
    fn identify(&self, test: &Test) -> VendorResult<Option<BundleRef>> {
        if let Some(trace) = &self.trace {
            trace.push(json!(["identify", tj(test)]));
        }
        Ok(self
            .bundles
            .iter()
            .find(|(t, _)| t == test)
            .and_then(|(_, b)| b.clone()))
    }
}

// ---- retry ---------------------------------------------------------------------------------------------

/// PoolProgressAccumulator stand-in for tests whose tests are all freshly Added:
/// AddRetry there has no side effect (WP 3.2 decision 1).
pub struct NoActionProgress;
impl RetryProgress for NoActionProgress {
    fn retry_test(&mut self, _test: &Test) -> Option<TestAction> {
        None
    }
}

// ---- replay hooks --------------------------------------------------------------------------------------

/// Scripted draws and sequential batch ids. A missing draw is recorded as a
/// failure that verify_consumed reports, like RuntimeHooks.
#[derive(Default)]
pub struct ScriptedHooks {
    pub draws: Mutex<std::collections::VecDeque<i32>>,
    pub drawn: Mutex<Vec<i32>>,
    pub ids: Mutex<u32>,
    pub failure: Mutex<Option<ReplayError>>,
    pub trace: Option<Trace>,
}
impl ScriptedHooks {
    pub fn with_draws(draws: impl IntoIterator<Item = i32>, trace: Option<Trace>) -> Arc<Self> {
        Arc::new(Self {
            draws: Mutex::new(draws.into_iter().collect()),
            trace,
            ..Self::default()
        })
    }
}
impl WallClock for ScriptedHooks {
    fn millis(&self, _label: &'static str) -> ClockResult<i64> {
        Ok(0)
    }
    fn instant(&self, _label: &'static str) -> ClockResult<Instant> {
        Ok(limit())
    }
}
impl MonotonicClock for ScriptedHooks {
    fn nanos(&self, _label: &'static str) -> ClockResult<i64> {
        Ok(0)
    }
}
impl ReplayHooks for ScriptedHooks {
    fn draw_int(&self, label: &'static str) -> ClockResult<i32> {
        match lock(&self.draws).pop_front() {
            Some(value) => {
                lock(&self.drawn).push(value);
                if let Some(trace) = &self.trace {
                    trace.push(json!(["draw", value]));
                }
                Ok(value)
            }
            None => {
                let error = ReplayError {
                    label: label.into(),
                    message: "draw tape missing or exhausted".into(),
                };
                lock(&self.failure).get_or_insert(error.clone());
                Err(error)
            }
        }
    }
    fn choose_key(&self, label: &'static str, _candidates: &[String]) -> ClockResult<String> {
        Err(ReplayError {
            label: label.into(),
            message: "unused".into(),
        })
    }
    fn allocate_id(&self, _label: &'static str) -> ClockResult<String> {
        let mut ids = lock(&self.ids);
        *ids += 1;
        if let Some(trace) = &self.trace {
            trace.push(json!(["allocateId"]));
        }
        Ok(format!("batch-{ids}"))
    }
    fn verify_consumed(&self) -> ClockResult<()> {
        lock(&self.failure).clone().map_or(Ok(()), Err)
    }
}

// ---- oracle replay -------------------------------------------------------------------------------------

/// The ordered calls of one replay, in the fixture's JSON shape.
#[derive(Clone, Default)]
pub struct Trace(pub Arc<Mutex<Vec<Value>>>);
impl Trace {
    pub fn push(&self, event: Value) {
        lock(&self.0).push(event);
    }
    pub fn drain(&self) -> Vec<Value> {
        std::mem::take(&mut *lock(&self.0))
    }
}

/// The probe's RecordingMetrics: MetricsProviderStub plus a call trace.
pub struct RecordingMetrics {
    pub stub: MetricsProviderStub,
    pub trace: Trace,
}
impl RecordingMetrics {
    pub fn from_json(metrics: &Value, trace: Trace) -> Self {
        let map = |key: &str| -> HashMap<Test, f64> {
            metrics[key]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| (test_of(&e[0]), double_of(&e[1])))
                .collect()
        };
        Self {
            stub: MetricsProviderStub {
                success_rates: Mutex::new(map("successRates")),
                execution_times: Mutex::new(map("executionTimes")),
                success_rate: double_of(&metrics["successRate"]),
                execution_time: double_of(&metrics["executionTime"]),
            },
            trace,
        }
    }
}
impl MetricsLookup for RecordingMetrics {
    fn success_rate(&self, test: &Test, limit: Instant) -> f64 {
        self.trace.push(json!(["successRate", tj(test)]));
        self.stub.success_rate(test, limit)
    }
    fn execution_time(&self, test: &Test, percentile: f64, limit: Instant) -> f64 {
        self.trace.push(json!(["executionTime", tj(test)]));
        self.stub.execution_time(test, percentile, limit)
    }
}

pub fn tj(test: &Test) -> Value {
    json!([test.pkg.to_owned(), test.clazz.to_owned(), test.method.to_owned()])
}
pub fn tsj(tests: &[Test]) -> Value {
    Value::Array(tests.iter().map(tj).collect())
}
pub fn test_of(value: &Value) -> Test {
    let part = |i: usize| value[i].as_str().unwrap_or_else(|| panic!("bad test {value}"));
    generate_test(part(0), part(1), part(2))
}
pub fn tests_of(value: &Value) -> Vec<Test> {
    value.as_array().into_iter().flatten().map(test_of).collect()
}
/// Doubles are Java Double.toString strings in the fixtures.
pub fn double_of(value: &Value) -> f64 {
    let text = value.as_str().unwrap_or_else(|| panic!("bad double {value}"));
    jnum::parse_double(text).unwrap_or_else(|| panic!("bad double {text}"))
}
pub fn rows(table: &str, family: &str) -> Vec<Value> {
    let rows: Vec<Value> = table
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("bad row: {e}")))
        .collect();
    assert!(!rows.is_empty() && rows.iter().all(|r| r["family"] == family));
    rows
}

/// Records insertion-time key extraction independently of metrics calls and draws.
pub fn recording(mut inner: TestKey, trace: Trace) -> TestKey {
    Box::new(move |test| {
        trace.push(json!(["key", tj(test)]));
        inner(test)
    })
}
