//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/internal/sub/ExecutionReport.kt
//! and PoolSummary.kt, Summary.kt; report/Reporter.kt.

use crate::{model::*, runtime::ServiceFuture};
use jcompat::{jtime::Instant, writers::JavaString};
use std::sync::{Arc, OnceLock};

#[derive(Clone, Debug)]
pub struct DeviceConnectedEvent {
    pub instant: Instant,
    pub pool_id: DevicePoolId,
    pub device: DeviceInfo,
}
#[derive(Clone, Debug)]
pub struct DeviceDisconnectedEvent {
    pub instant: Instant,
    pub pool_id: DevicePoolId,
    pub device: DeviceInfo,
}
#[derive(Clone, Debug)]
pub struct DevicePreparingEvent {
    pub start: Instant,
    pub finish: Instant,
    pub serial_number: JavaString,
}
#[derive(Clone, Debug)]
pub struct DeviceProviderPreparingEvent {
    pub start: Instant,
    pub finish: Instant,
    pub serial_number: JavaString,
}
#[derive(Clone, Debug)]
pub struct TestEvent {
    pub instant: Instant,
    pub pool_id: DevicePoolId,
    pub device: DeviceInfo,
    /// One allocation per public notification (approved P1); clones retain aliases.
    pub test_result: Arc<TestResult>,
    pub final_result: bool,
}
#[derive(Clone, Debug)]
pub enum Event {
    DeviceConnected(DeviceConnectedEvent),
    DeviceDisconnected(DeviceDisconnectedEvent),
    DevicePreparing(DevicePreparingEvent),
    DeviceProviderPreparing(DeviceProviderPreparingEvent),
    Test(TestEvent),
}

/// Immutable snapshots make the lazy summary independent of subsequent capture.
/// Constructor order is retained; the generator supplies timestamp-sorted lists.
#[derive(Clone, Debug)]
pub struct ExecutionReport {
    device_connected_events: Vec<DeviceConnectedEvent>,
    device_disconnected_events: Vec<DeviceDisconnectedEvent>,
    device_preparing_events: Vec<DevicePreparingEvent>,
    device_provider_preparing_events: Vec<DeviceProviderPreparingEvent>,
    test_events: Vec<TestEvent>,
    summary: OnceLock<Summary>,
}
impl ExecutionReport {
    pub fn new(
        connected: Vec<DeviceConnectedEvent>,
        disconnected: Vec<DeviceDisconnectedEvent>,
        preparing: Vec<DevicePreparingEvent>,
        provider: Vec<DeviceProviderPreparingEvent>,
        tests: Vec<TestEvent>,
    ) -> Self {
        Self {
            device_connected_events: connected,
            device_disconnected_events: disconnected,
            device_preparing_events: preparing,
            device_provider_preparing_events: provider,
            test_events: tests,
            summary: OnceLock::new(),
        }
    }
    pub fn device_connected_events(&self) -> &[DeviceConnectedEvent] {
        &self.device_connected_events
    }
    pub fn device_disconnected_events(&self) -> &[DeviceDisconnectedEvent] {
        &self.device_disconnected_events
    }
    pub fn device_preparing_events(&self) -> &[DevicePreparingEvent] {
        &self.device_preparing_events
    }
    pub fn device_provider_preparing_events(&self) -> &[DeviceProviderPreparingEvent] {
        &self.device_provider_preparing_events
    }
    pub fn test_events(&self) -> &[TestEvent] {
        &self.test_events
    }
    pub fn summary(&self) -> &Summary {
        self.summary.get_or_init(|| {
            let mut pools = Vec::new();
            for event in &self.device_connected_events {
                if !pools.iter().any(|p: &PoolSummary| p.pool_id == event.pool_id) {
                    pools.push(self.compile_pool(&event.pool_id));
                }
            }
            Summary { pools }
        })
    }
    fn compile_pool(&self, pool_id: &DevicePoolId) -> PoolSummary {
        let mut devices = Vec::new();
        for event in self.device_connected_events.iter().filter(|e| &e.pool_id == pool_id) {
            if !devices
                .iter()
                .any(|d: &DeviceInfo| d.serial_number == event.device.serial_number)
            {
                devices.push(event.device.clone());
            }
        }
        let events: Vec<_> = self.test_events.iter().filter(|e| &e.pool_id == pool_id).collect();
        let tests: Vec<_> = events
            .iter()
            .filter(|e| e.final_result && e.test_result.status != TestStatus::Incomplete)
            .map(|e| e.test_result.clone())
            .collect();
        let mut retries: Vec<(Arc<TestResult>, Vec<TestEvent>)> = Vec::new();
        for result in &tests {
            let attempts = events
                .iter()
                .filter(|e| e.test_result.test == result.test && !Arc::ptr_eq(&e.test_result, result))
                .map(|e| (*e).clone())
                .collect();
            // LinkedHashMap replaces equal values without moving the first key.
            if let Some((_, value)) = retries.iter_mut().find(|(key, _)| key == result) {
                *value = attempts;
            } else {
                retries.push((result.clone(), attempts));
            }
        }
        let names = |statuses: &[TestStatus], unique: bool, finals: bool| {
            let mut names = Vec::new();
            for event in &events {
                if (!finals || event.final_result) && statuses.contains(&event.test_result.status) {
                    let name = event.test_result.test.to_test_name('.', '#');
                    if !unique || !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
            names
        };
        PoolSummary {
            pool_id: pool_id.clone(),
            duration_millis: duration(tests.iter().map(Arc::as_ref)),
            raw_duration_millis: duration(events.iter().map(|e| e.test_result.as_ref())),
            passed: names(&[TestStatus::Passed], true, true),
            ignored: names(&[TestStatus::Ignored, TestStatus::AssumptionFailure], true, true),
            failed: names(&[TestStatus::Failure], true, true),
            raw_passed: names(&[TestStatus::Passed], false, false),
            raw_ignored: names(&[TestStatus::Ignored, TestStatus::AssumptionFailure], false, false),
            raw_failed: names(&[TestStatus::Failure], false, false),
            raw_incomplete: names(&[TestStatus::Incomplete], false, false),
            tests,
            retries,
            flaky: 0,
            devices,
        }
    }
}
fn duration<'a>(results: impl Iterator<Item = &'a TestResult>) -> i64 {
    results
        .filter(|r| r.start_time != 0 && r.end_time != 0)
        .fold(0i64, |sum, r| sum.wrapping_add(r.end_time.wrapping_sub(r.start_time)))
}
#[derive(Clone, Debug)]
pub struct PoolSummary {
    pub pool_id: DevicePoolId,
    pub tests: Vec<Arc<TestResult>>,
    /// Ordered structural keys; retry values use reference identity for exclusion.
    pub retries: Vec<(Arc<TestResult>, Vec<TestEvent>)>,
    /// Insertion-ordered sets, exposed as lists for reporter iteration.
    pub passed: Vec<JavaString>,
    pub ignored: Vec<JavaString>,
    pub failed: Vec<JavaString>,
    pub flaky: i32,
    pub duration_millis: i64,
    pub devices: Vec<DeviceInfo>,
    pub raw_passed: Vec<JavaString>,
    pub raw_ignored: Vec<JavaString>,
    pub raw_failed: Vec<JavaString>,
    pub raw_incomplete: Vec<JavaString>,
    pub raw_duration_millis: i64,
}
#[derive(Clone, Debug)]
pub struct Summary {
    pub pools: Vec<PoolSummary>,
}
pub trait Reporter: Send + Sync {
    fn generate<'a>(&'a self, report: &'a ExecutionReport) -> ServiceFuture<'a, ()>;
}

mod junit;
pub mod projection;
mod reporters;
pub use junit::{JUnitReporter, junit_xml};
pub use reporters::*;
