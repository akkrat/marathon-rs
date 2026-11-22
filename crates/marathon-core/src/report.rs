//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/internal/sub/ExecutionReport.kt
//! and PoolSummary.kt, Summary.kt; report/Reporter.kt.

use crate::{model::*, runtime::ServiceFuture};
use jcompat::jtime::Instant;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
};

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
    pub serial_number: String,
}
#[derive(Clone, Debug)]
pub struct DeviceProviderPreparingEvent {
    pub start: Instant,
    pub finish: Instant,
    pub serial_number: String,
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
            // One pass over each event list; a pool's events are grouped here once, not filtered per pool or
            // per test, which made the summary quadratic in the number of results (U263).
            let mut index: HashMap<&DevicePoolId, usize> = HashMap::new();
            let mut pools: Vec<PoolEvents<'_>> = Vec::new();
            for event in &self.device_connected_events {
                let slot = *index.entry(&event.pool_id).or_insert_with(|| {
                    pools.push(PoolEvents {
                        pool_id: &event.pool_id,
                        serials: HashSet::new(),
                        devices: Vec::new(),
                        events: Vec::new(),
                    });
                    pools.len() - 1
                });
                let pool = &mut pools[slot];
                if pool.serials.insert(&event.device.serial_number) {
                    pool.devices.push(event.device.clone());
                }
            }
            for event in &self.test_events {
                if let Some(&slot) = index.get(&event.pool_id) {
                    pools[slot].events.push(event);
                }
            }
            Summary {
                pools: pools.into_iter().map(compile_pool).collect(),
            }
        })
    }
}
/// One pool's connected devices (distinct by serial, first seen kept) and test events, in event order.
struct PoolEvents<'a> {
    pool_id: &'a DevicePoolId,
    serials: HashSet<&'a str>,
    devices: Vec<DeviceInfo>,
    events: Vec<&'a TestEvent>,
}
fn compile_pool(pool: PoolEvents<'_>) -> PoolSummary {
    let PoolEvents {
        pool_id,
        devices,
        events,
        ..
    } = pool;
    let tests: Vec<_> = events
        .iter()
        .filter(|e| e.final_result && e.test_result.status != TestStatus::Incomplete)
        .map(|e| e.test_result.clone())
        .collect();
    let mut by_test: HashMap<&Test, Vec<&TestEvent>> = HashMap::new();
    for event in &events {
        by_test.entry(&event.test_result.test).or_default().push(event);
    }
    let mut retries: Vec<(Arc<TestResult>, Vec<TestEvent>)> = Vec::new();
    // Equal results have equal tests, so a key is looked up among its own test's keys only.
    let mut keys: HashMap<&Test, Vec<usize>> = HashMap::new();
    for result in &tests {
        let attempts = by_test.get(&result.test).map_or_else(Vec::new, |same_test| {
            same_test
                .iter()
                .filter(|e| !Arc::ptr_eq(&e.test_result, result))
                .map(|e| (*e).clone())
                .collect()
        });
        // LinkedHashMap replaces equal values without moving the first key.
        let slots = keys.entry(&result.test).or_default();
        if let Some(&slot) = slots.iter().find(|&&slot| retries[slot].0 == *result) {
            retries[slot].1 = attempts;
        } else {
            slots.push(retries.len());
            retries.push((result.clone(), attempts));
        }
    }
    let names = |statuses: &[TestStatus], unique: bool, finals: bool| {
        let mut names = Vec::new();
        let mut seen = HashSet::new();
        for event in &events {
            if (!finals || event.final_result) && statuses.contains(&event.test_result.status) {
                let name = event.test_result.test.to_test_name('.', '#');
                if !unique || seen.insert(name.clone()) {
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
    pub passed: Vec<String>,
    pub ignored: Vec<String>,
    pub failed: Vec<String>,
    pub flaky: i32,
    pub duration_millis: i64,
    pub devices: Vec<DeviceInfo>,
    pub raw_passed: Vec<String>,
    pub raw_ignored: Vec<String>,
    pub raw_failed: Vec<String>,
    pub raw_incomplete: Vec<String>,
    pub raw_duration_millis: i64,
}
#[derive(Clone, Debug)]
pub struct Summary {
    pub pools: Vec<PoolSummary>,
}
pub trait Reporter: Send + Sync {
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
    fn generate<'a>(&'a self, report: &'a ExecutionReport) -> ServiceFuture<'a, ()>;
}

pub mod json;
mod junit;
pub mod projection;
mod reporters;
pub mod yaml;
pub use junit::{JUnitReporter, junit_xml};
pub use reporters::*;
