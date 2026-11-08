//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/progress/PoolProgressAccumulatorTest.kt
#![allow(dead_code)]
use jcompat::jtime::Instant;
use marathon_config::schema::{ExecutionMode, ExecutionStrategyConfiguration};
use marathon_core::{
    ci::Ci,
    error::*,
    execution::progress::*,
    executor::{CoreExecutor, Signal, shared::failure},
    logging::OutputRecord,
    model::*,
    output::{ConsoleSink, OutputDriver},
    runtime::*,
    strategy::retry::RetryProgress,
    wiring::Tracker,
};
use std::{
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub fn test(method: &str) -> Test {
    Test {
        pkg: "com.example".into(),
        clazz: "Class".into(),
        method: method.into(),
        meta_properties: vec![],
    }
}
pub fn device(serial: &str) -> DeviceInfo {
    DeviceInfo {
        operating_system: OperatingSystem { version: "1".into() },
        serial_number: serial.into(),
        model: "model".into(),
        manufacturer: "manufacturer".into(),
        network_state: NetworkState::Connected,
        device_features: vec![],
        healthy: true,
    }
}
pub fn result(test: &Test, device: &DeviceInfo, batch: &str, status: TestStatus, start: i64, end: i64) -> TestResult {
    TestResult {
        test: test.clone(),
        device: device.clone(),
        test_batch_id: batch.into(),
        status,
        start_time: start,
        end_time: end,
        stacktrace: None,
        attachments: vec![],
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Output(OutputRecord),
    Track(DevicePoolId, DeviceInfo, Box<TestResult>, bool),
}
#[derive(Default)]
pub struct Capture {
    pub effects: Mutex<Vec<Effect>>,
    pub fail: AtomicBool,
    pub cancel: AtomicBool,
}
impl ConsoleSink for Capture {
    fn write(&self, records: Vec<OutputRecord>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            assert_ne!(std::thread::current().name(), Some("marathon-core"));
            self.effects
                .lock()
                .unwrap()
                .extend(records.into_iter().map(Effect::Output));
            Ok(())
        })
    }
}
impl Tracker for Capture {
    fn device_connected<'a>(&'a self, _: &'a DevicePoolId, _: &'a DeviceInfo) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn device_disconnected<'a>(&'a self, _: &'a DevicePoolId, _: &'a DeviceInfo) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn device_provider_init<'a>(&'a self, _: &'a str, _: Instant, _: Instant) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn device_preparing<'a>(&'a self, _: &'a str, _: Instant, _: Instant) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn test<'a>(
        &'a self,
        pool: &'a DevicePoolId,
        device: &'a DeviceInfo,
        result: &'a TestResult,
        final_result: bool,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            assert_ne!(std::thread::current().name(), Some("marathon-core"));
            tokio::task::yield_now().await;
            self.effects.lock().unwrap().push(Effect::Track(
                pool.clone(),
                device.clone(),
                Box::new(result.clone()),
                final_result,
            ));
            if self.cancel.load(Ordering::SeqCst) {
                return Err(VendorError::Cancelled(marathon_core::executor::shared::requested(
                    "tracker cancelled",
                )));
            }
            if self.fail.load(Ordering::SeqCst) {
                Err(VendorError::Other(failure(
                    "java.lang.IllegalStateException",
                    "tracker failed",
                )))
            } else {
                Ok(())
            }
        })
    }
}
pub struct Harness {
    pub reporter: PoolProgressAccumulator,
    pub capture: Arc<Capture>,
    pub signal: Arc<Signal>,
}
impl Harness {
    pub fn new(core: &Rc<CoreExecutor>, tests: Vec<Test>, mode: ExecutionMode, fast: bool) -> Self {
        Self::with_ci(core, tests, mode, fast, Ci::None)
    }
    pub fn with_ci(core: &Rc<CoreExecutor>, tests: Vec<Test>, mode: ExecutionMode, fast: bool, ci: Ci) -> Self {
        let capture = Arc::new(Capture::default());
        let reporter = PoolProgressAccumulator::new(
            DevicePoolId { name: "test".into() },
            &TestShard {
                tests,
                flaky_tests: vec![],
            },
            ExecutionStrategyConfiguration { mode, fast },
            ci,
            ProgressServices {
                tracker: capture.clone(),
                logs: core.log_queue(),
                clock: core.clock(),
                output: OutputDriver {
                    vendor: core.vendor(),
                    sink: capture.clone(),
                },
            },
        );
        Self {
            reporter,
            capture,
            signal: Signal::new(),
        }
    }
    pub async fn start(&mut self, device: &DeviceInfo, test: &Test) {
        self.reporter
            .test_started(device, test, self.signal.clone())
            .await
            .unwrap();
    }
    pub async fn end(&mut self, result: TestResult, final_result: bool) -> Option<TestAction> {
        self.reporter
            .test_ended(&result.device, &result, final_result, self.signal.clone())
            .await
            .unwrap()
    }
    pub fn retry(&mut self, test: &Test) -> Option<TestAction> {
        self.reporter.retry_test(test)
    }
    pub fn remove(&mut self, test: &Test, count: i32) -> Option<TestAction> {
        self.reporter.remove_test(test, count)
    }
    pub fn aggregate(&self) -> bool {
        self.reporter.aggregate_result().unwrap()
    }
    pub fn raw(&self) -> Vec<String> {
        self.capture
            .effects
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                Effect::Output(OutputRecord::Raw(s)) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }
}
pub fn run(body: impl FnOnce(Rc<CoreExecutor>) -> LocalFuture<'static, OperationResult<()>> + Send + 'static) {
    CoreExecutor::run(body).unwrap();
}
