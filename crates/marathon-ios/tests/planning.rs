//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt:683–774
//! Supplemental tests for the extracted helper, without claiming provider integration.
mod support;
use marathon_apple::bin::xcrun::simctl::model::decode_list_devices_output;
use marathon_config::devices::{AppleTarget, Transport};
use marathon_core::time::{ClockResult, SystemTimer, WallClock};
use marathon_ios::device::{provisioning_plan::*, timing::ProvisioningTiming};
use marathon_test_support::apple::fake_simulator_worker::ok;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use support::*;

#[tokio::test]
async fn profiles_reuse_first_available_unused_records_once_and_preserve_input_order() -> TestResult {
    let h = Harness::new();
    let p = profile();
    let kind = p.fully_qualified_device_type_id().to_string_lossy();
    let data = format!(
        r#"{{"devicetypes":[],"runtimes":[],"devices":{{"r":[
        {{"udid":"explicit","name":"e","state":"Booted","isAvailable":true,"deviceTypeIdentifier":"{kind}"}},
        {{"udid":"registered","name":"r","state":"Booted","isAvailable":true,"deviceTypeIdentifier":"{kind}"}},
        {{"udid":"null","name":"n","state":"Booted","deviceTypeIdentifier":"{kind}"}},
        {{"udid":"reuse-first","name":"a","state":"Shutdown","isAvailable":true,"deviceTypeIdentifier":"{kind}"}},
        {{"udid":"reuse-first","name":"duplicate","state":"Shutdown","isAvailable":true,"deviceTypeIdentifier":"other"}},
        {{"udid":"reuse-second","name":"b","state":"Shutdown","isAvailable":true,"deviceTypeIdentifier":"{kind}"}}
    ]}}}}"#
    );
    h.cycle
        .refresh_with(h.worker.clone(), || async {
            Ok(decode_list_devices_output(&data).unwrap())
        })
        .await?;
    let (logging, _) = logging();
    let timing = ProvisioningTiming::new(Arc::new(SystemTimer::new(h.clock.clone()).unwrap()), logging.clone());
    let planner = PlanCalculator::new(logging.clone(), timing, h.clock.as_ref())?;
    let targets = vec![
        AppleTarget::Host,
        AppleTarget::Simulator {
            udid: "explicit".into(),
        },
        AppleTarget::Simulator { udid: "null".into() },
        AppleTarget::Physical {
            udid: "unvalidated".into(),
        },
        AppleTarget::SimulatorProfile(p.clone()),
        AppleTarget::SimulatorProfile(p.clone()),
        AppleTarget::SimulatorProfile(p.clone()),
    ];
    let plan = planner
        .plan(
            &Transport::Local,
            &h.bin,
            &targets,
            &h.cycle,
            || vec!["registered".into()],
            false,
            || {
                Ok(CurrentDevices {
                    eligible_disposed: vec!["disposed".into()],
                    kept: vec!["registered".into()],
                })
            },
            &h.context,
        )
        .await?;
    assert_eq!(
        plan.existing_simulators.iter().map(String::as_str).collect::<Vec<_>>(),
        ["explicit", "disposed", "reuse-first", "reuse-second"]
    );
    assert_eq!(
        serde_json::json!({"reused":plan.reused_profiles.keys().collect::<Vec<_>>(), "created":plan.needs_provisioning.len()}),
        witness("planner-reuse-order")
    );
    assert_eq!(plan.needs_provisioning, vec![p.clone()]);
    assert_eq!(
        plan.reused_profiles.into_values().collect::<Vec<_>>(),
        vec![p.clone(), p]
    );
    assert_eq!(plan.physical_devices.into_iter().collect::<Vec<_>>(), ["unvalidated"]);
    Ok(())
}
#[tokio::test]
async fn shutdown_is_serial_and_final_eligibility_is_read_after_the_last_await() -> TestResult {
    let h = Harness::new();
    let data = r#"{"devicetypes":[],"runtimes":[],"devices":{"r":[{"udid":"a","name":"a","state":"Booted","isAvailable":true},{"udid":"b","name":"b","state":"Booted","isAvailable":true}]}}"#;
    h.cycle
        .refresh_with(h.worker.clone(), || async {
            Ok(decode_list_devices_output(data).unwrap())
        })
        .await?;
    let order = Arc::new(Mutex::new(Vec::new()));
    let calls = order.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |args| {
        let calls = calls.clone();
        Box::pin(async move {
            if args.get(2).is_some_and(|s| s == "shutdown") {
                calls.lock().unwrap().push(format!("start:{}", args[3]));
                tokio::task::yield_now().await;
                calls.lock().unwrap().push(format!("end:{}", args[3]));
                return Ok(Some(ok(Vec::<String>::new())));
            }
            Ok(None)
        })
    });
    let (logging, logs) = logging();
    let timing = ProvisioningTiming::new(Arc::new(SystemTimer::new(h.clock.clone()).unwrap()), logging.clone());
    let planner = PlanCalculator::new(logging.clone(), timing, h.clock.as_ref())?;
    let expected = order.clone();
    let plan = planner
        .plan(
            &Transport::Local,
            &h.bin,
            &[],
            &h.cycle,
            Vec::new,
            true,
            || {
                assert_eq!(*expected.lock().unwrap(), vec!["start:a", "end:a", "start:b", "end:b"]);
                Ok(CurrentDevices {
                    eligible_disposed: vec!["disposed".into()],
                    kept: vec!["disposed".into()],
                })
            },
            &h.context,
        )
        .await?;
    assert_eq!(serde_json::json!(*order.lock().unwrap()), witness("planner-shutdown"));
    assert!(plan.existing_simulators.is_empty());
    assert!(
        logs.lock()
            .unwrap()
            .iter()
            .any(|(name, _, message, _)| name == "Timings"
                && message.starts_with("Worker local: shut down 2 unused simulators in "))
    );
    Ok(())
}
struct Tape(Mutex<VecDeque<i64>>);
impl WallClock for Tape {
    fn millis(&self, label: &'static str) -> ClockResult<i64> {
        assert_eq!(label, "core.di.Clock");
        Ok(self.0.lock().unwrap().pop_front().unwrap())
    }
    fn instant(&self, _: &'static str) -> ClockResult<jcompat::jtime::Instant> {
        unreachable!()
    }
}
#[test]
fn timing_evaluates_the_message_before_its_separate_elapsed_read_with_debug_disabled() -> TestResult {
    let clock = Arc::new(Tape(Mutex::new(VecDeque::from([1000, 1010, 1025]))));
    let (logging, logs) = logging();
    let timing = ProvisioningTiming::new(Arc::new(SystemTimer::new(clock.clone()).unwrap()), logging);
    timing
        .cycle(&Transport::Local, 2, 3, 4, 5)
        .map_err(|e| e.failure().to_java_string().to_string_lossy())?;
    assert_eq!(
        logs.lock().unwrap()[0].2,
        "Worker local: connected in 2 ms, planned 4 simulators in 3 ms, provisioned them in 5 ms (+25 ms since start)"
    );
    assert_eq!(
        serde_json::json!(logs.lock().unwrap()[0].2),
        witness("timing-read-order")
    );
    assert!(clock.0.lock().unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn registration_during_inventory_is_excluded_from_profile_reuse() -> TestResult {
    let h = Harness::new();
    h.worker.add(["registered-during-load"]);
    let registered = Arc::new(Mutex::new(Vec::new()));
    let registration = registered.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |args| {
        let registration = registration.clone();
        Box::pin(async move {
            if args == marathon_test_support::apple::fake_simulator_worker::LIST {
                registration.lock().unwrap().push("registered-during-load".into());
            }
            Ok(None)
        })
    });
    let (logging, _) = logging();
    let timing = ProvisioningTiming::new(Arc::new(SystemTimer::new(h.clock.clone()).unwrap()), logging.clone());
    let planner = PlanCalculator::new(logging.clone(), timing, h.clock.as_ref())?;
    let p = profile();
    let plan = planner
        .plan(
            &Transport::Local,
            &h.bin,
            &[AppleTarget::SimulatorProfile(p.clone())],
            &h.cycle,
            || registered.lock().unwrap().clone(),
            false,
            || {
                Ok(CurrentDevices {
                    eligible_disposed: vec![],
                    kept: vec![],
                })
            },
            &h.context,
        )
        .await?;
    assert!(plan.reused_profiles.is_empty());
    assert_eq!(plan.needs_provisioning, vec![p]);
    Ok(())
}

#[tokio::test]
async fn host_warning_precedes_missing_simulator_error() -> TestResult {
    let h = Harness::new();
    let (logging, logs) = logging();
    let timing = ProvisioningTiming::new(Arc::new(SystemTimer::new(h.clock.clone()).unwrap()), logging.clone());
    let planner = PlanCalculator::new(logging.clone(), timing, h.clock.as_ref())?;
    logging.registry.lock().unwrap().logger(
        "com.malinskiy.marathon.apple.ios.device.AppleSimulatorProvider",
        Some(jcompat::logging::Level::Warn),
    );
    planner
        .plan(
            &worker("worker"),
            &h.bin,
            &[AppleTarget::Simulator { udid: "missing".into() }, AppleTarget::Host],
            &h.cycle,
            Vec::new,
            false,
            || {
                Ok(CurrentDevices {
                    eligible_disposed: vec![],
                    kept: vec![],
                })
            },
            &h.context,
        )
        .await?;
    let logs = logs.lock().unwrap();
    assert_eq!(
        logs.iter().map(|(_, level, _, _)| level.as_str()).collect::<Vec<_>>(),
        ["WARN", "ERROR"]
    );
    assert_eq!(
        serde_json::json!(logs.iter().map(|(_, level, _, _)| level.as_str()).collect::<Vec<_>>()),
        witness("planner-log-order")
    );
    Ok(())
}
