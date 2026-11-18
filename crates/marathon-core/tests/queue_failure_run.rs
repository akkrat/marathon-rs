//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/DevicePoolActor.kt
//! and execution/Scheduler.kt (D68: a whole run whose queue fails).
//!
//! The production run graph with the Stub vendor and the real scheduler. Before D68 a queue that ended with an error
//! left its pool waiting for work that never came, and the run never ended (U168).
use marathon_config::schema::BatchingStrategyConfiguration;
use marathon_core::{
    error::*,
    executor::shared::failure,
    model::Test,
    vendor::{BundleRef, TestBundleIdentifier},
};
use marathon_error::FailureKind;
use marathon_logging::Level;
use marathon_test_support::{
    console::Line,
    scenario::{self, Entry},
    stub_device::{StubDevice, StubDeviceOptions},
};
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

const CAUSE: &str = "no bundle on the second batch";

/// Identifies the first batch's test, then fails: a batching error is one of the queue's remaining handler errors.
struct SecondCallFails(AtomicU32);
impl TestBundleIdentifier for SecondCallFails {
    fn identify(&self, _test: &Test) -> VendorResult<Option<BundleRef>> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 1 {
            return Err(VendorError::Other(failure(FailureKind::IllegalState, CAUSE)));
        }
        Ok(None)
    }
}

fn stub(serial: &str, model: &str, prepare_time_millis: i64) -> Arc<StubDevice> {
    StubDevice::new(StubDeviceOptions {
        serial_number: serial.into(),
        model: model.into(),
        prepare_time_millis,
        ..StubDeviceOptions::default()
    })
}

/// U168 → D68: pool A has a batch running on `serial-1` when its queue fails batching `serial-3`'s request, and pool B
/// is alive, still preparing `serial-2`. The run ends at once with the queue's cause through the ordinary run-failure
/// path: reports written, `Marathon run failed` with the cause, and the existing failure status (`false` without
/// `ignoreFailures`), never a success read from the stopped pools' progress.
#[test]
fn a_failed_queue_ends_the_run_with_its_cause_while_another_pool_is_alive_d68() {
    let (mut factory, mut script) = scenario::load("two-pools-disconnect.json");
    factory.configuration.configuration.batching_strategy = BatchingStrategyConfiguration::FixedSize {
        size: 1,
        duration_millis: None,
        percentile: None,
        time_limit: None,
        last_mile_length: 0,
    };
    factory.bundle_identifier = Some(Arc::new(SecondCallFails(AtomicU32::new(0))));
    script.entry = Entry::Run;
    script.devices.clear();
    for device in [
        stub("serial-1", "modelA", 5_000),
        stub("serial-2", "modelZ", 600_000),
        stub("serial-3", "modelA", 5_000),
    ] {
        script
            .devices
            .insert(device.reference().0.info().serial_number.to_owned(), device);
    }
    script.events = [
        (0, "connect", "serial-1"),
        (100, "connect", "serial-2"),
        (200, "connect", "serial-3"),
    ]
    .into_iter()
    .map(|(at, event, serial)| (at, event.to_owned(), Some(serial.to_owned()), serde_json::Value::Null))
    .chain([(120_000, "end".to_owned(), None, serde_json::Value::Null)])
    .collect();

    let outcome = scenario::drive(factory, script, None, None);

    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert!(
        matches!(outcome.value, Some(Ok(false))),
        "the run's mapped outcome: {:?}",
        outcome.value
    );
    // `serial-3` asks at 5,200 ms; `serial-1`'s test would end at 10,000 ms and `serial-2` is ready at 600,100 ms.
    let finished = outcome.completed_at.unwrap();
    assert!(finished < 10_000, "finished at {finished} ms");
    let failed = outcome
        .lines
        .iter()
        .filter_map(|line| match line {
            Line::Log {
                level: Level::ERROR,
                message,
                cause: Some(cause),
                ..
            } if message == "Marathon run failed" => Some(cause.message.as_deref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(failed, [Some(CAUSE)], "{:?}", outcome.lines);
    // The reports of the ordinary failure path: the run wrote its results directory.
    assert!(
        outcome.output_dir.join("test_result/raw.json").is_file(),
        "no reports in {}",
        outcome.output_dir.display()
    );
    assert!(
        outcome.provider_calls.contains(&"terminate_after_stop"),
        "{:?}",
        outcome.provider_calls
    );
}
