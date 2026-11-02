//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/queue/TestResultReporterAllSuccessTest.kt
//!
//! Only PoolProgressAccumulator and Track, as in Kotlin; the mock's `inOrder` and
//! `verifyNoMoreInteractions` become the ordered list of recorded Track calls.
#[path = "support/progress.rs"]
mod support;
use marathon_config::schema::ExecutionMode::{self, *};
use marathon_core::model::*;
use support::*;

fn generate_test() -> Test {
    Test {
        pkg: "pkg".into(),
        clazz: "clazz".into(),
        method: "method".into(),
        meta_properties: vec![],
    }
}
fn create_device_info() -> DeviceInfo {
    DeviceInfo {
        operating_system: OperatingSystem {
            version: "Fake OS".into(),
        },
        serial_number: "fake serial".into(),
        model: "fake model".into(),
        manufacturer: "fake manufacturer".into(),
        network_state: NetworkState::Connected,
        device_features: vec![],
        healthy: true,
    }
}

/// The default configuration is ANY_SUCCESS with fast; the strict one ALL_SUCCESS with fast.
fn check(mode: ExecutionMode, statuses: [TestStatus; 3], finals: [bool; 3]) {
    run(move |core| {
        Box::pin(async move {
            let test = generate_test();
            let device_info = create_device_info();
            let mut filter = Harness::new(&core, vec![test.clone(); 3], mode, true);
            let results: Vec<TestResult> = statuses
                .iter()
                .zip([(0, 1), (2, 3), (4, 5)])
                .map(|(status, (start, end))| result(&test, &device_info, "stub-batch", *status, start, end))
                .collect();
            for r in &results {
                filter.end(r.clone(), false).await;
            }
            let tracked: Vec<Effect> = filter
                .capture
                .effects
                .lock()
                .unwrap()
                .iter()
                .filter(|effect| matches!(effect, Effect::Track(..)))
                .cloned()
                .collect();
            let pool = DevicePoolId { name: "test".into() };
            let expected: Vec<Effect> = results
                .iter()
                .zip(finals)
                .map(|(r, last)| Effect::Track(pool.clone(), device_info.clone(), Box::new(r.clone()), last))
                .collect();
            assert_eq!(tracked, expected);
            Ok(())
        })
    });
}

#[test]
fn default_config_success_failure_failure_should_report_success() {
    use TestStatus::*;
    check(AnySuccess, [Passed, Failure, Failure], [true, false, false]);
}

#[test]
fn default_config_failure_failure_success_should_report_success() {
    use TestStatus::*;
    check(AnySuccess, [Failure, Failure, Passed], [false, false, true]);
}

#[test]
fn strict_config_success_failure_failure_should_report_failure() {
    use TestStatus::*;
    check(AllSuccess, [Passed, Failure, Failure], [false, true, false]);
}

#[test]
fn strict_config_failure_success_success_should_report_failure() {
    use TestStatus::*;
    check(AllSuccess, [Failure, Passed, Passed], [true, false, false]);
}
