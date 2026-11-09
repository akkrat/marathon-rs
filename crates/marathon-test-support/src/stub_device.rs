//! Port of vendor/vendor-test/src/main/kotlin/com/malinskiy/marathon/test/StubDevice.kt
//!
//! Each device keeps its own Test-keyed status cursor and synthetic 0→1→2 timestamps.
//! Results complete independently of `execute` returning: a result before a crash is kept,
//! a batch exception takes precedence over device loss, and only PASSED/FAILURE/INCOMPLETE
//! results are reported (ignored and assumption statuses are dropped).
use crate::lock;
use jcompat::logging::{Logger, MarathonLogging};
use marathon_config::Configuration;
use marathon_core::{
    error::*,
    executor::{SharedDeferred, shared::failure},
    model::*,
    runtime::*,
    vendor::{Device, DeviceRef, VendorContext},
};
use marathon_error::FailureKind;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

/// The constructor parameters, with the Kotlin defaults.
#[derive(Clone)]
pub struct StubDeviceOptions {
    pub prepare_time_millis: i64,
    pub test_time_millis: i64,
    pub operating_system: String,
    pub model: String,
    pub manufacturer: String,
    pub network_state: NetworkState,
    pub device_features: Vec<DeviceFeature>,
    pub abi: String,
    pub serial_number: String,
    pub healthy: bool,
    pub crash_with_test_batch_exception: bool,
    pub crash_with_device_lost_exception: bool,
    pub report_results_before_crash: bool,
    /// The replay driver's `scripted` behaviour (tools/oracle/.../ReplayDevice.kt): prepare and
    /// execute wait for the script's `prepare-*`/`batch-*` events instead of the stub delays.
    pub scripted: bool,
}
impl Default for StubDeviceOptions {
    fn default() -> Self {
        Self {
            prepare_time_millis: 5000,
            test_time_millis: 5000,
            operating_system: "25".into(),
            model: "test".into(),
            manufacturer: "test".into(),
            network_state: NetworkState::Connected,
            device_features: vec![],
            abi: "test".into(),
            serial_number: "serial-1".into(),
            healthy: true,
            crash_with_test_batch_exception: false,
            crash_with_device_lost_exception: false,
            report_results_before_crash: false,
            scripted: false,
        }
    }
}

#[derive(Default)]
struct Cursor {
    /// `lateinit var executionResults`: None until set.
    execution_results: Option<HashMap<Test, Vec<TestStatus>>>,
    execution_index_map: HashMap<Test, usize>,
    time_counter: i64,
}

/// The replay transcript's device-call log (tools/oracle/module/.../ReplayDevice.kt `call`).
#[derive(Clone)]
pub struct Recorder {
    pub log: Arc<Mutex<Vec<serde_json::Value>>>,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
}
impl Recorder {
    pub fn record(&self, entry: serde_json::Value) {
        let mut entry = entry;
        if let serde_json::Value::Object(fields) = &mut entry {
            let mut ordered = serde_json::Map::new();
            ordered.insert("t".into(), (self.now)().into());
            ordered.append(fields);
            entry = serde_json::Value::Object(ordered);
        }
        lock(&self.log).push(entry);
    }
}

/// A scripted device's suspended call and the event that resumes it.
enum Step {
    Prepare(Arc<SharedDeferred<serde_json::Value>>),
    Execute(Vec<String>, Arc<SharedDeferred<serde_json::Value>>),
}

pub struct StubDevice {
    options: StubDeviceOptions,
    pending: Mutex<Option<Step>>,
    recorder: Mutex<Option<Recorder>>,
    logger: Logger,
    cursor: Mutex<Cursor>,
    me: Weak<StubDevice>,
}
impl StubDevice {
    pub fn new(options: StubDeviceOptions) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            options,
            pending: Mutex::new(None),
            recorder: Mutex::new(None),
            // `MarathonLogging.logger(StubDevice::class.java.simpleName)`; the stub's own
            // debug lines (Preparing/Disposing) are not routed to the console here (P2).
            logger: MarathonLogging::default().slf4j_logger("StubDevice"),
            cursor: Mutex::default(),
            me: me.clone(),
        })
    }
    pub fn with_serial(serial: &str) -> Arc<Self> {
        Self::new(StubDeviceOptions {
            serial_number: serial.into(),
            ..StubDeviceOptions::default()
        })
    }
    pub fn record_to(&self, recorder: Recorder) {
        *lock(&self.recorder) = Some(recorder);
    }
    fn call(&self, name: &str, fields: serde_json::Value) {
        if let Some(recorder) = lock(&self.recorder).clone() {
            let mut entry = serde_json::json!({"device": self.options.serial_number.to_owned(), "call": name});
            if let (serde_json::Value::Object(entry), serde_json::Value::Object(fields)) = (&mut entry, fields) {
                entry.extend(fields);
            }
            recorder.record(entry);
        }
    }
    pub fn set_execution_results(&self, results: HashMap<Test, Vec<TestStatus>>) {
        lock(&self.cursor).execution_results = Some(results);
    }
    pub fn reference(&self) -> DeviceRef {
        match self.me.upgrade() {
            Some(me) => DeviceRef(me),
            None => unreachable!("a device is alive while it is referenced"),
        }
    }
    fn results(&self, batch: &TestBatch, info: &DeviceInfo) -> VendorResult<Vec<TestResult>> {
        let mut cursor = lock(&self.cursor);
        let mut results = Vec::with_capacity(batch.tests.len());
        for test in &batch.tests {
            let i = cursor.execution_index_map.get(test).copied().unwrap_or(0);
            let statuses = match &cursor.execution_results {
                None => {
                    return Err(VendorError::Other(failure(
                        FailureKind::UninitializedProperty,
                        "lateinit property executionResults has not been initialized",
                    )));
                }
                Some(results) => results.get(test).ok_or({
                    VendorError::Other(Failure {
                        kind: FailureKind::NullReference,
                        message: None,
                        cause: None,
                        stacktrace: None,
                    })
                })?,
            };
            let status = *statuses.get(i).ok_or_else(|| {
                VendorError::Other(failure(
                    FailureKind::ArrayIndexOutOfBounds,
                    &format!("Index {i} out of bounds for length {}", statuses.len()),
                ))
            })?;
            cursor.execution_index_map.insert(test.clone(), i + 1);
            let start = cursor.time_counter;
            results.push(TestResult {
                test: test.clone(),
                device: info.clone(),
                test_batch_id: batch.id.clone(),
                status,
                start_time: start,
                end_time: start + 1,
                stacktrace: None,
                attachments: vec![],
            });
            cursor.time_counter += 1;
        }
        Ok(results)
    }
}
fn only(results: &[TestResult], status: TestStatus) -> Vec<TestResult> {
    results.iter().filter(|r| r.status == status).cloned().collect()
}
impl Device for StubDevice {
    fn info(&self) -> DeviceInfo {
        let o = &self.options;
        DeviceInfo {
            operating_system: OperatingSystem {
                version: o.operating_system.clone(),
            },
            serial_number: o.serial_number.clone(),
            model: o.model.clone(),
            manufacturer: o.manufacturer.clone(),
            network_state: o.network_state,
            device_features: o.device_features.clone(),
            healthy: o.healthy,
        }
    }
    fn abi(&self) -> String {
        self.options.abi.clone()
    }
    fn worker_id(&self) -> Option<String> {
        None
    }
    fn logger(&self) -> &Logger {
        &self.logger
    }
    fn prepare<'a>(&'a self, _configuration: Arc<Configuration>, context: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.call("prepare", serde_json::json!({}));
            let result = if self.options.scripted {
                self.prepare_scripted(&context).await
            } else {
                context
                    .clock
                    .delay(self.options.prepare_time_millis, context.cancellation.clone())
                    .await
            };
            *lock(&self.pending) = None;
            match &result {
                Ok(()) => self.call("prepared", serde_json::json!({})),
                Err(VendorError::Cancelled(_)) => self.call("prepare-cancelled", serde_json::json!({})),
                Err(_) => {}
            }
            result
        })
    }
    fn execute<'a>(
        &'a self,
        _configuration: Arc<Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<dyn Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let id = batch.id.to_owned();
            let tests: Vec<String> = batch
                .tests
                .iter()
                .map(|t| t.to_test_name('.', '#').to_owned())
                .collect();
            self.call(
                "execute",
                serde_json::json!({"pool": pool.name.to_owned(), "batch": id, "tests": &tests}),
            );
            let result = if self.options.scripted {
                self.execute_scripted(&batch, tests, result, context).await
            } else {
                self.execute_stub(batch, result, context).await
            };
            *lock(&self.pending) = None;
            match &result {
                Ok(()) => self.call("executed", serde_json::json!({"batch": id})),
                Err(VendorError::Cancelled(_)) => self.call("execute-cancelled", serde_json::json!({"batch": id})),
                Err(e) => {
                    let class = e.failure().kind.legacy_class();
                    let simple = class.rsplit('.').next().unwrap_or_default().to_owned();
                    self.call("execute-threw", serde_json::json!({"batch": id, "exception": simple}));
                }
            }
            result
        })
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        self.call("dispose", serde_json::json!({}));
        Box::pin(async { Ok(()) })
    }
}
impl StubDevice {
    /// Delivers a device event of the script; an error when the device is not in a state that takes it.
    pub fn deliver(&self, event: &serde_json::Value) -> Option<String> {
        let kind = event["event"].as_str().unwrap_or_default();
        let serial = self.options.serial_number.to_owned();
        let pending = lock(&self.pending);
        match (kind, &*pending) {
            ("prepare-done" | "prepare-failed", Some(Step::Prepare(done))) => {
                done.complete(Ok(event.clone()));
            }
            ("prepare-done" | "prepare-failed", _) => {
                return Some(format!("{kind} for {serial}, which is not preparing"));
            }
            ("batch-result" | "batch-failed", Some(Step::Execute(running, done))) => {
                if kind == "batch-result" {
                    let mut reported: Vec<&str> = event["results"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|r| r["test"].as_str().unwrap_or_default())
                        .collect();
                    reported.sort_unstable();
                    let mut batch: Vec<&str> = running.iter().map(String::as_str).collect();
                    batch.sort_unstable();
                    if reported != batch {
                        return Some(format!(
                            "batch-result for {serial} reports {reported:?}, but the batch is {batch:?}"
                        ));
                    }
                }
                done.complete(Ok(event.clone()));
            }
            ("batch-result" | "batch-failed", _) => {
                return Some(format!("{kind} for {serial}, which is not executing a batch"));
            }
            _ => return Some(format!("{kind} is not a device event")),
        }
        None
    }
    async fn prepare_scripted(&self, context: &VendorContext) -> VendorResult<()> {
        let done = SharedDeferred::new();
        *lock(&self.pending) = Some(Step::Prepare(done.clone()));
        let event = done.wait_in(context.cancellation.clone()).await?;
        if event["event"] == "prepare-failed" {
            self.call("prepare-failed", serde_json::json!({}));
            let message = event["message"].as_str().unwrap_or("prepare failed");
            return Err(VendorError::Other(failure(FailureKind::Runtime, message)));
        }
        Ok(())
    }
    async fn execute_scripted(
        &self,
        batch: &BatchRef,
        tests: Vec<String>,
        result: Arc<dyn Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorResult<()> {
        let done = SharedDeferred::new();
        *lock(&self.pending) = Some(Step::Execute(tests.clone(), done.clone()));
        let event = done.wait_in(context.cancellation.clone()).await?;
        let device = self.reference();
        if event["event"] == "batch-failed" {
            if event["reportEmptyResultsFirst"].as_bool().unwrap_or(false) {
                result.complete(Ok(TestBatchResults {
                    device,
                    finished: vec![],
                    failed: vec![],
                    uncompleted: vec![],
                }));
            }
            let message = event["message"].as_str().unwrap_or("scripted failure");
            return Err(match event["exception"].as_str().unwrap_or_default() {
                "TestBatchExecutionException" => {
                    VendorError::BatchExecution(failure(FailureKind::BatchExecution, message))
                }
                "DeviceLostException" => {
                    let cause = failure(FailureKind::Runtime, message);
                    VendorError::DeviceLost(Failure {
                        kind: FailureKind::DeviceLost,
                        message: Some(cause.to_java_string()),
                        cause: Some(Box::new(cause)),
                        stacktrace: None,
                    })
                }
                "RuntimeException" => VendorError::Other(failure(FailureKind::Runtime, message)),
                kind => VendorError::Other(failure(
                    FailureKind::IllegalArgument,
                    &format!("batch-failed: exception {kind} is not supported"),
                )),
            });
        }
        let info = self.info();
        let mut results = vec![];
        let mut cursor = lock(&self.cursor);
        for r in event["results"].as_array().into_iter().flatten() {
            // `deliver` checked that the results name exactly the batch's tests.
            let name = r["test"].as_str().unwrap_or_default();
            let Some(at) = tests.iter().position(|t| t == name) else {
                continue;
            };
            let status = match r["status"].as_str().unwrap_or_default() {
                "PASSED" => TestStatus::Passed,
                "FAILURE" => TestStatus::Failure,
                "INCOMPLETE" => TestStatus::Incomplete,
                "IGNORED" => TestStatus::Ignored,
                _ => TestStatus::AssumptionFailure,
            };
            let start = r["start"].as_i64().unwrap_or(cursor.time_counter);
            cursor.time_counter += 1;
            results.push(TestResult {
                test: batch.tests[at].clone(),
                device: info.clone(),
                test_batch_id: batch.id.clone(),
                status,
                start_time: start,
                end_time: r["end"].as_i64().unwrap_or(start + 1),
                stacktrace: r["stacktrace"].as_str().map(Into::into),
                attachments: vec![],
            });
        }
        drop(cursor);
        result.complete(Ok(TestBatchResults {
            device,
            finished: only(&results, TestStatus::Passed),
            failed: only(&results, TestStatus::Failure),
            uncompleted: only(&results, TestStatus::Incomplete),
        }));
        Ok(())
    }
    /// `StubDevice.execute`, statement by statement.
    async fn execute_stub(
        &self,
        batch: BatchRef,
        result: Arc<dyn Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorResult<()> {
        {
            context
                .clock
                .delay(self.options.test_time_millis, context.cancellation.clone())
                .await?;
            let device = self.reference();
            if self.options.report_results_before_crash {
                result.complete(Ok(TestBatchResults {
                    device: device.clone(),
                    finished: vec![],
                    failed: vec![],
                    uncompleted: vec![],
                }));
            }
            if self.options.crash_with_test_batch_exception {
                return Err(VendorError::BatchExecution(failure(
                    FailureKind::BatchExecution,
                    "user requested the device to crash via crashWithTestBatchException parameter",
                )));
            }
            if self.options.crash_with_device_lost_exception {
                // DeviceLostException(cause): Throwable(cause) takes the cause's toString as its message.
                let cause = failure(
                    FailureKind::Runtime,
                    "user requested the device to crash via crashWithDeviceLostException parameter",
                );
                return Err(VendorError::DeviceLost(Failure {
                    kind: FailureKind::DeviceLost,
                    message: Some(cause.to_java_string()),
                    cause: Some(Box::new(cause)),
                    stacktrace: None,
                }));
            }
            let results = self.results(&batch, &self.info())?;
            result.complete(Ok(TestBatchResults {
                device,
                finished: only(&results, TestStatus::Passed),
                failed: only(&results, TestStatus::Failure),
                uncompleted: only(&results, TestStatus::Incomplete),
            }));
            Ok(())
        }
    }
}
