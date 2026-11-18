//! Native-only runs of the production run graph with the Stub vendor (no Kotlin counterpart): D82, where a test that comes
//! back from a device without a worker (Android's devices have none) is left to the other devices of its pool while
//! anything else is queued, as a retry from a worker already is.
//!
//! The devices answer INCOMPLETE as the Android runner reports the tests of a batch whose instrumentation ended before a
//! result (D25, `a_stream_cut_off_fails_the_run_and_keeps_the_device_d25` in marathon-android). Execution-time sorting, as
//! on the farm where this was seen, puts the longest test first again each time it is queued.
use marathon_config::schema::SortingStrategyConfiguration;
use marathon_core::{analytics::Analytics, model::*, strategy::MetricsLookup};
use marathon_test_support::{
    factory::FakeTimer,
    scenario::{self, Outcome},
    stub_device::{StubDevice, StubDeviceOptions},
};
use serde_json::Value;
use std::sync::Arc;

const SLOW: &str = "slow";

fn test(method: &str) -> Test {
    scenario::test("test", "Placement", method)
}
/// The slow test first, then the others in their order.
struct History;
impl MetricsLookup for History {
    fn success_rate(&self, _: &Test, _: jcompat::jtime::Instant) -> f64 {
        1.0
    }
    fn execution_time(&self, test: &Test, _: f64, _: jcompat::jtime::Instant) -> f64 {
        if test.method == SLOW { 60_000.0 } else { 1_000.0 }
    }
}
struct Device {
    serial: &'static str,
    prepare_millis: i64,
    test_millis: i64,
    status: TestStatus,
    /// The status of the slow test, when it differs from the others'.
    slow_status: Option<TestStatus>,
}
/// Runs `methods` (the slow test first) on `devices`, each of which answers every test with its own status, with
/// `uncompletedTestRetryQuota: 3`.
fn run(methods: &[&str], devices: &[Device]) -> Outcome {
    let (mut factory, mut script) = scenario::load("success.json");
    let tests: Vec<Test> = methods.iter().map(|m| test(m)).collect();
    factory.tests = tests.clone();
    let configuration = &mut factory.configuration.configuration;
    configuration.uncompleted_test_retry_quota = 3;
    configuration.sorting_strategy = SortingStrategyConfiguration::ExecutionTime {
        percentile: 90.0,
        time_limit: jcompat::jtime::instant_of(0, 0).unwrap(),
    };
    factory.analytics = Some(Box::new(|_| Ok(Analytics::from_lookup(Arc::new(History)))));
    script.devices.clear();
    script.events.clear();
    for device in devices {
        let stub = StubDevice::new(StubDeviceOptions {
            serial_number: device.serial.into(),
            prepare_time_millis: device.prepare_millis,
            test_time_millis: device.test_millis,
            ..StubDeviceOptions::default()
        });
        let status = |t: &Test| match device.slow_status {
            Some(status) if t.method == SLOW => status,
            _ => device.status,
        };
        stub.set_execution_results(tests.iter().map(|t| (t.clone(), vec![status(t); 64])).collect());
        script.devices.insert(device.serial.into(), stub);
        script.events.push((
            1000,
            "connect".into(),
            Some(device.serial.into()),
            serde_json::json!({"event": "connect", "device": device.serial}),
        ));
    }
    script
        .events
        .push((600_000, "end".into(), None, serde_json::json!({"event": "end"})));
    let outcome = scenario::drive(
        factory,
        script,
        Some(Box::new(|core, factory| {
            factory.timer = Some(Arc::new(FakeTimer(core.clock().now_source())));
        })),
        None,
    );
    assert!(outcome.completed, "the run did not complete");
    outcome
}
/// raw.json as (method, device serial, status), in the order the results came.
fn executions(outcome: &Outcome) -> Vec<(String, String, String)> {
    let raw: Value = serde_json::from_str(&outcome.read("test_result/raw.json")).unwrap();
    raw.as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let field = |name: &str| row[name].as_str().unwrap().to_owned();
            (field("method"), field("deviceSerial"), field("status"))
        })
        .collect()
}
fn of(executions: &[(String, String, String)], method: &str) -> Vec<(String, String)> {
    executions
        .iter()
        .filter(|(m, ..)| m == method)
        .map(|(_, device, status)| (device.clone(), status.clone()))
        .collect()
}
fn s(device: &str, status: &str) -> (String, String) {
    (device.into(), status.into())
}

fn device(serial: &'static str, prepare_millis: i64, test_millis: i64, status: TestStatus) -> Device {
    Device {
        serial,
        prepare_millis,
        test_millis,
        status,
        slow_status: None,
    }
}

/// The farm's case (U204): A's instrumentation ends before the slow test starts, while B, the pool's other device, is busy
/// with a batch it has already started and other tests are queued. A passes the other tests. B's batch (virtual time) ends
/// at 2,600 ms, after A's fourth attempt at the slow test would have ended (2,400 ms: before D82, A took it back at once
/// three times and it failed on `uncompletedTestRetryQuota`, so the run failed) and while tests are still queued for A
/// (A passes one every 100 ms until 2,900 ms). Now A takes the queued tests and B, once its batch ends, the slow test.
#[test]
fn an_incomplete_test_is_left_to_a_busy_device_while_other_tests_are_queued_d82() {
    let methods = ["slow", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9"];
    let outcome = run(
        &methods,
        &[
            // Ready at 2,000 ms: takes the slow test, gives it back at 2,100 ms.
            Device {
                slow_status: Some(TestStatus::Incomplete),
                ..device("serial-A", 1000, 100, TestStatus::Passed)
            },
            // Ready at 2,050 ms: takes t1 and runs it until 2,600 ms.
            device("serial-B", 1050, 550, TestStatus::Passed),
        ],
    );
    let executions = executions(&outcome);
    assert_eq!(
        of(&executions, SLOW),
        [s("serial-A", "INCOMPLETE"), s("serial-B", "PASSED")],
        "the slow test's attempts: {executions:?}"
    );
    let position = |device: &str, method: &str| {
        executions
            .iter()
            .position(|(m, d, _)| m == method && d == device)
            .unwrap_or_else(|| panic!("{method} on {device}: {executions:?}"))
    };
    let b_free = position("serial-B", "t1");
    // B's batch ended after A had given four results (the baseline's four attempts at the slow test) ...
    assert!(
        executions[..b_free].iter().filter(|(_, d, _)| d == "serial-A").count() >= 4,
        "{executions:?}"
    );
    // ... and A still had queued tests to run after B took the slow test.
    assert!(
        executions[b_free..].iter().any(|(_, d, _)| d == "serial-A"),
        "{executions:?}"
    );
    assert!(
        executions.iter().all(|(m, _, status)| m == SLOW || status == "PASSED"),
        "{executions:?}"
    );
    assert!(matches!(outcome.value, Some(Ok(true))), "{:?}", outcome.value);
}

/// A device that fails every test: before D82 it ran the slow test four times within 300 ms and failed it while B was
/// getting ready. Now it tries every test once before it takes any back, and B, once ready, takes the slow test.
///
/// Such a device still uses up the quota of the tests nothing else can take in time (t1–t7, t9 here): keeping it out of
/// the pool is a separate question (docs/wp/x80-findings.md).
#[test]
fn a_device_that_fails_every_test_tries_each_once_before_its_own_retries_d82() {
    let methods = ["slow", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9"];
    let outcome = run(
        &methods,
        &[
            device("serial-A", 1000, 100, TestStatus::Incomplete),
            device("serial-B", 1550, 2000, TestStatus::Passed),
        ],
    );
    let executions = executions(&outcome);
    assert_eq!(
        of(&executions, SLOW),
        [s("serial-A", "INCOMPLETE"), s("serial-B", "PASSED")],
        "the slow test's attempts: {executions:?}"
    );
    let on_a: Vec<&str> = executions
        .iter()
        .filter(|(_, device, _)| device == "serial-A")
        .map(|(m, ..)| m.as_str())
        .collect();
    assert_eq!(on_a[..methods.len()], methods, "{on_a:?}");
}

/// The retry still goes back to the device that gave it back when nothing else is queued and the other device is busy:
/// it is not held for a device that may not come, and the run ends.
#[test]
fn an_incomplete_test_goes_back_to_its_device_when_nothing_else_is_queued_d82() {
    let outcome = run(
        &["slow", "t1"],
        &[
            device("serial-A", 1500, 100, TestStatus::Incomplete),
            device("serial-B", 1000, 10_000, TestStatus::Passed),
        ],
    );
    let executions = executions(&outcome);
    assert_eq!(of(&executions, SLOW), [s("serial-B", "PASSED")]);
    assert_eq!(
        of(&executions, "t1"),
        [
            s("serial-A", "INCOMPLETE"),
            s("serial-A", "INCOMPLETE"),
            s("serial-A", "INCOMPLETE"),
            s("serial-A", "FAILURE")
        ]
    );
    assert!(matches!(outcome.value, Some(Ok(false))), "{:?}", outcome.value);
}

/// An idle device of the pool is asked first when a retry is queued, so it takes the retry. This held before D82 too.
#[test]
fn an_idle_device_takes_an_incomplete_test() {
    let outcome = run(
        &["slow"],
        &[
            device("serial-A", 1000, 1000, TestStatus::Incomplete),
            device("serial-B", 1500, 1000, TestStatus::Passed),
        ],
    );
    assert_eq!(
        of(&executions(&outcome), SLOW),
        [s("serial-A", "INCOMPLETE"), s("serial-B", "PASSED")]
    );
    assert!(matches!(outcome.value, Some(Ok(true))), "{:?}", outcome.value);
}
