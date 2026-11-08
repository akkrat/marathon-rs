//! Port of core/src/test/kotlin/com/malinskiy/marathon/report/ExecutionReportTest.kt
#![allow(dead_code)]
use jcompat::jtime::instant_of;
use marathon_core::{model::*, report::*};
use std::sync::Arc;
pub fn device() -> DeviceInfo {
    DeviceInfo {
        operating_system: OperatingSystem { version: "23".into() },
        serial_number: "xxyyzz".into(),
        model: "Android SDK built for x86".into(),
        manufacturer: "unknown".into(),
        network_state: NetworkState::Connected,
        device_features: vec![DeviceFeature::Screenshot, DeviceFeature::Video],
        healthy: true,
    }
}
pub fn test(method: &str) -> Test {
    Test {
        pkg: "com".into(),
        clazz: "example".into(),
        method: method.into(),
        meta_properties: vec![],
    }
}
pub fn pool(name: &str) -> DevicePoolId {
    DevicePoolId { name: name.into() }
}
pub fn result(method: &str, status: TestStatus) -> TestResult {
    TestResult {
        test: test(method),
        device: device(),
        test_batch_id: "stub-batch".into(),
        status,
        start_time: 0,
        end_time: 100,
        stacktrace: None,
        attachments: vec![],
    }
}
pub fn event(result: Arc<TestResult>, final_result: bool, time: i64) -> TestEvent {
    TestEvent {
        instant: instant_of(0, time * 1_000_000).unwrap(),
        pool_id: pool("myPool"),
        device: device(),
        test_result: result,
        final_result,
    }
}
pub fn report(events: Vec<TestEvent>) -> ExecutionReport {
    ExecutionReport::new(
        vec![DeviceConnectedEvent {
            instant: instant_of(0, 0).unwrap(),
            pool_id: pool("myPool"),
            device: device(),
        }],
        vec![],
        vec![],
        vec![],
        events,
    )
}
pub fn names(names: &[String]) -> Vec<String> {
    names.iter().map(|n| n.clone()).collect()
}

/// Fail a fixture setup explicitly; production services return their errors.
pub fn checked<T, E: std::fmt::Debug>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("fixture operation failed: {error:?}"),
    }
}

pub struct DropFlag<'a>(pub &'a std::sync::atomic::AtomicBool);
impl Drop for DropFlag<'_> {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

pub struct CountingRuntime {
    pub inner: Arc<dyn marathon_core::runtime::VendorRuntime>,
    pub spawns: std::sync::atomic::AtomicUsize,
    pub blocking: std::sync::atomic::AtomicUsize,
}
impl CountingRuntime {
    pub fn new(inner: Arc<dyn marathon_core::runtime::VendorRuntime>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            spawns: Default::default(),
            blocking: Default::default(),
        })
    }
}
impl marathon_core::runtime::VendorRuntime for CountingRuntime {
    fn spawn_cancellable(
        &self,
        body: marathon_core::runtime::VendorTaskBody,
    ) -> Arc<dyn marathon_core::runtime::VendorTask> {
        self.spawns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.spawn_cancellable(body)
    }
    fn spawn(
        &self,
        body: marathon_core::runtime::VendorFuture<'static, ()>,
    ) -> Arc<dyn marathon_core::runtime::VendorTask> {
        self.spawns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.spawn(body)
    }
    fn spawn_blocking(
        &self,
        body: Box<dyn FnOnce() -> marathon_core::error::VendorResult<()> + Send>,
    ) -> Arc<dyn marathon_core::runtime::VendorTask> {
        self.blocking.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.spawn_blocking(body)
    }
}
