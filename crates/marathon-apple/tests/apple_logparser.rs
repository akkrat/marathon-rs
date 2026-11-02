//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/logparser/parser/{DeviceFailureParserTest,TestRunProgressParserTest}.kt
mod support;
use jcompat::{logging::MarathonLogging, writers::JavaString};
use marathon_apple::{
    cmd::remote::ssh::sshj::config::SshLogging,
    logparser::{
        XctestEventProducer,
        parser::{DeviceFailureParser, DeviceFailureReason, ParseError, TestRunProgressParser},
    },
    test::TestEvent,
};
use marathon_core::time::{ClockResult, Timer};
use std::sync::{Arc, Mutex};

pub struct FixedTimer(pub i64);
impl Timer for FixedTimer {
    fn start_time_millis(&self) -> i64 {
        self.0
    }
    fn current_time_millis(&self) -> ClockResult<i64> {
        Ok(self.0)
    }
    fn elapsed_time_millis(&self) -> ClockResult<i64> {
        Ok(0)
    }
    fn measure(&self, f: &mut dyn FnMut()) -> ClockResult<i64> {
        f();
        Ok(0)
    }
}
fn logging() -> SshLogging {
    SshLogging::new(
        Arc::new(Mutex::new(MarathonLogging::default())),
        Arc::new(|_, _, _, _| {}),
    )
}
fn parser(target: &str) -> TestRunProgressParser {
    TestRunProgressParser::new(Arc::new(FixedTimer(1537187696000)), target, logging())
}
fn reason(line: &str, reason: DeviceFailureReason) {
    let Ok(Some(events)) = DeviceFailureParser.process(line) else {
        panic!("missing failure: {line}")
    };
    assert!(matches!(&events[0],TestEvent::TestRunFailed{reason:r,..} if *r==reason));
}
#[test]
fn batch_level_failures() {
    for (line, r) in [
        (
            "Test runner exited before starting test execution",
            DeviceFailureReason::Unknown,
        ),
        (
            "Failed to launch app with identifier: com.example",
            DeviceFailureReason::Unknown,
        ),
        (
            "Early unexpected exit, operation never finished bootstrapping",
            DeviceFailureReason::Unknown,
        ),
        (
            "Failed to install or launch the test runner",
            DeviceFailureReason::FailedRunner,
        ),
    ] {
        reason(line, r);
        assert!(r.is_batch_failure());
    }
}
#[test]
fn device_level_failures() {
    for (line, r) in [
        ("Software caused connection abort", DeviceFailureReason::ConnectionAbort),
        (
            "Unable to find a destination matching the provided destination specifier",
            DeviceFailureReason::InvalidSimulatorIdentifier,
        ),
        (
            "Connection peer refused channel request",
            DeviceFailureReason::UnreachableHost,
        ),
        (
            "CoreSimulatorService connection became invalid",
            DeviceFailureReason::ServicesUnavailable,
        ),
        ("Unable to locate device set", DeviceFailureReason::ServicesUnavailable),
    ] {
        reason(line, r);
        assert!(!r.is_batch_failure());
    }
}
#[test]
fn a_dead_system_app_is_a_device_level_failure() {
    for line in [
        "Terminating since there is no system app.",
        "XCTRunner[4242:4242] Exiting because the workspace server has disconnected.",
        "Test runner exited before starting test execution: Terminating since there is no system app.",
    ] {
        reason(line, DeviceFailureReason::SystemAppUnavailable);
    }
}
#[test]
fn regular_output() {
    assert!(matches!(
        DeviceFailureParser.process("Test Suite 'All tests' started"),
        Ok(None)
    ));
}
#[test]
fn test() -> Result<(), Box<dyn std::error::Error>> {
    let hooks = marathon_core::executor::hooks::RuntimeHooks::live();
    for (name, target) in [
        ("success_0", ""),
        ("patrol_0", "testTarget"),
        ("success_multiple_0", ""),
        ("success_multiple_0", "testTarget"),
        ("patrol_1", "testTarget"),
        ("timeout_0", ""),
        ("timeout_1", ""),
        ("failure_0", ""),
        ("failure_1", ""),
    ] {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/vendor-apple-base/fixtures/test_output");
        let input = std::fs::read_to_string(base.join(format!("{name}.log.input")))?;
        let expected = std::fs::read_to_string(base.join(format!("{name}.expected")))?;
        let mut parser = parser(target);
        let mut actual = vec![];
        for line in input.lines() {
            for event in parser.process(line)?.into_iter().flatten() {
                actual.push(
                    event
                        .to_kotlin_string(&*hooks)
                        .map_err(|e| format!("{e:?}"))?
                        .to_string_lossy(),
                );
            }
        }
        assert_eq!(actual.join("\n"), expected.trim_end(), "{name}/{target}");
    }
    Ok(())
}
#[test]
fn failures_preempt_progress_and_class_split_keeps_a5_a7() -> Result<(), ParseError> {
    let mut p = XctestEventProducer::new(parser("target"));
    let events = p
        .process("Test Case '-[a.b.C Test runner exited]' started.")?
        .unwrap_or_default();
    assert!(matches!(events[0], TestEvent::TestRunFailed { .. }));
    let mut p = parser("target");
    for (class, pkg, clazz) in [("a.b.C", "a.b", "b.C"), ("C", "target", "C")] {
        let events = p
            .process(&format!("Test Case '-[{class} m]' started."))?
            .unwrap_or_default();
        assert!(
            matches!(&events[0],TestEvent::TestStarted{id} if id.pkg==JavaString::from(pkg) && id.clazz==JavaString::from(clazz))
        );
    }
    Ok(())
}
#[test]
fn malformed_durations_escape_as_typed_errors() {
    for allowance in ["1 minute", "1 hour", "1 minute, 30 seconds", " 3 minutes"] {
        assert!(matches!(
            parser("").process(&format!(
                "Test Case '-[p.C m]' exceeded execution time allowance of {allowance}. The test may have hung"
            )),
            Err(ParseError::Duration { .. })
        ));
    }
    assert!(matches!(
        parser("").process("Test Case '-[p.C m]' failed (1.2.3 seconds)."),
        Err(ParseError::NumberFormat { .. })
    ));
}

#[test]
fn jvm_streams_replay_events_errors_and_debug_lines() -> Result<(), Box<dyn std::error::Error>> {
    use jcompat::logging::{Level, Vendor};
    let hooks = marathon_core::executor::hooks::RuntimeHooks::live();
    for line in include_str!("../../../fixtures/oracle/apple-logparser/probes.jsonl")
        .split('\n')
        .filter(|l| !l.is_empty())
    {
        let row: serde_json::Value = serde_json::from_str(line)?;
        if row["kind"] != "stream" {
            continue;
        }
        let mut registry = MarathonLogging::default();
        registry.configure(true, Vendor::Apple { compact_output: false });
        let logs = Arc::new(Mutex::new(vec![]));
        let output = logs.clone();
        let logging = SshLogging::new(
            Arc::new(Mutex::new(registry)),
            Arc::new(move |logger, level, message, _| {
                output.lock().unwrap().push(serde_json::json!({"logger":logger.name(),"level":match level {Level::Debug=>"DEBUG",Level::Trace=>"TRACE",_=>"OTHER"},"message":message}))
            }),
        );
        let progress = TestRunProgressParser::new(
            Arc::new(FixedTimer(1537187696000)),
            row["target"].as_str().unwrap(),
            logging,
        );
        let mut producer = XctestEventProducer::new(progress);
        for entry in row["rows"].as_array().unwrap() {
            let text = entry["line"].as_str().unwrap();
            let result = if row["producer"].as_bool().unwrap() {
                producer.process(text)
            } else {
                producer.progress.process(text)
            };
            match result {
                Ok(events) => {
                    assert!(entry["exception"].is_null(), "{}: {text:?}", row["case"]);
                    let events = events.map(|v| {
                        v.iter()
                            .map(|e| e.to_kotlin_string(&*hooks).unwrap().to_string_lossy())
                            .collect::<Vec<_>>()
                    });
                    assert_eq!(
                        serde_json::to_value(events)?,
                        entry["events"],
                        "{}: {text:?}",
                        row["case"]
                    );
                }
                Err(error) => {
                    let f = error.to_failure();
                    assert_eq!(
                        serde_json::json!([f.class.to_string_lossy(), f.message.map(|s| s.to_string_lossy())]),
                        entry["exception"],
                        "{}: {text:?}",
                        row["case"]
                    );
                }
            }
        }
        assert_eq!(
            serde_json::to_value(logs.lock().unwrap().clone())?,
            row["logs"],
            "{}",
            row["case"]
        );
    }
    Ok(())
}
