//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/logparser/parser/{DeviceFailureParserTest,TestRunProgressParserTest}.kt
mod support;
use jcompat::{logging::MarathonLogging, writers::JavaString};
use marathon_apple::{
    cmd::remote::ssh::sshj::config::SshLogging,
    logparser::{
        XctestEventProducer,
        parser::{
            DeviceFailureParser, DeviceFailureReason, ParseError, TestRunProgressParser,
            device_failure::FAILURE_PATTERNS,
        },
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
    // The progress parser claims this line too, and a true failure goes first. D39: since the name
    // is the test's own text, the failure has to be outside it.
    let line = "Test Case '-[a.b.C m]' exceeded execution time allowance of 2 minutes. The test may have hung; Test runner exited";
    let progress = parser("target").process(line)?.unwrap_or_default();
    assert!(matches!(progress[0], TestEvent::TestFailed { .. }));
    let mut p = XctestEventProducer::new(parser("target"));
    let events = p.process(line)?.unwrap_or_default();
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
/// Rendered from the format strings in Xcode's binaries, the recorded logs and the Kotlin tests:
/// see the table in docs/wp/x17-findings.md.
const D39_REAL_FORMS: &[(&str, &str)] = &[
    (
        "Terminating since there is no system app.",
        "Terminating since there is no system app",
    ),
    (
        "2026-11-06 12:08:08.118250+0300 Tests-Runner[27239:2608175] Terminating since there is no system app.",
        "Terminating since there is no system app",
    ),
    (
        "2026-11-06 12:08:08.118250+0300 Some App[27239:2608175] Terminating since there is no system app.",
        "Terminating since there is no system app",
    ),
    (
        "XCTRunner[4242:4242] Exiting because the workspace server has disconnected.",
        "Exiting because the workspace server has disconnected",
    ),
    (
        "2026-11-06 12:08:08.118 Some App[27239:a3f] Exiting because the workspace server has disconnected.",
        "Exiting because the workspace server has disconnected",
    ),
    (
        "xcodebuild: error: Unable to find a destination matching the provided destination specifier:",
        "Unable to find a destination matching the provided destination specifier",
    ),
    (
        "iOSSimulator: AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE: Failed to launch app with identifier: com.example and options: {} (error = E)",
        "Failed to launch app with identifier",
    ),
    (
        "Failed to launch app with identifier com.example (an unknown error has occurred)",
        "Failed to launch app with identifier",
    ),
    (
        "Connection peer refused channel request for \"x\"; channel canceled <DTXChannel: 0x1>",
        "Connection peer refused channel request",
    ),
    (
        "2026-11-06 12:08:08.118250+0300 xcodebuild[1:2] Connection peer refused channel request for \"x\"; channel canceled <DTXChannel: 0x1>",
        "Connection peer refused channel request",
    ),
    (
        "2026-11-06 12:08:08.118250+0300 Some App[1:2] Connection peer refused channel request for \"x\"; channel canceled <DTXChannel: 0x1>",
        "Connection peer refused channel request",
    ),
    (
        "CoreSimulatorService connection became invalid.  Simulator services will no longer be available.",
        "CoreSimulatorService connection became invalid",
    ),
    (
        "CoreSimulatorService connection interrupted.  Resubscribing to notifications.",
        "CoreSimulatorService connection interrupted",
    ),
    (
        "Loaded CoreSimulatorService is no longer valid for this process.  Simulator services will no longer be available.  Error=E",
        "Simulator services will no longer be available",
    ),
    ("Unable to locate device set: /d", "Unable to locate device set"),
    (
        "UI testing error: (XCUITestingErrorDomain) XCUITestingErrorFailedToTerminateApplicationProcess: Failed to terminate com.example",
        "Failed to terminate",
    ),
    (
        "Failed to terminate com.example after 1.0s; state is still `4`.",
        "Failed to terminate",
    ),
    (
        "\tFailed to install or launch the test runner",
        "Failed to install or launch the test runner",
    ),
    (
        "Error Domain=X Code=1 \"Failed to install or launch the test runner\" UserInfo={}",
        "Failed to install or launch the test runner",
    ),
    // A test's name is not a shelter: what follows it on the line can be xcodebuild's own text.
    ("Test Case '-[p.C m]' started.Test runner exited", "Test runner exited"),
    (
        "Test Suite 'All tests' started at 2026-11-06 12:08:08.118. Test runner exited",
        "Test runner exited",
    ),
];
fn literal_of(line: &str) -> Option<String> {
    match DeviceFailureParser.process(line) {
        Ok(Some(events)) => match &events[0] {
            TestEvent::TestRunFailed { message, .. } => Some(message.to_string_lossy()),
            other => panic!("{other:?}"),
        },
        _ => None,
    }
}
#[test]
fn every_real_failure_form_is_detected_d39() {
    for (line, literal) in D39_REAL_FORMS {
        assert_eq!(literal_of(line).as_deref(), Some(*literal), "{line}");
    }
    // Every literal, bare, indented, and in a log line of each process that reports failures.
    for (literal, _) in FAILURE_PATTERNS {
        for line in [
            literal.to_string(),
            format!("{literal}."),
            format!("\t{literal}."),
            format!("prefix: {literal}: suffix"),
            format!("2026-11-06 12:08:08.118250+0300 xcodebuild[1:2] [MT] {literal}"),
            format!("2026-11-06 12:08:08.118250+0300 simctl[1:2] {literal}"),
            format!("2026-11-06 12:08:08.118250+0300 Tests-Runner[1:2] {literal}"),
            format!("2026-11-06 12:08:08.118250+0300 XCTRunner[1:2] {literal}"),
            format!("Some App[1:2] {literal}"),
        ] {
            assert!(literal_of(&line).is_some(), "{line}");
        }
    }
}
#[test]
fn a_test_name_an_assertion_or_an_app_log_line_is_not_a_failure_d39() -> Result<(), ParseError> {
    for (literal, _) in FAILURE_PATTERNS {
        for line in [
            format!("Test Case '-[p.C {literal}]' started."),
            format!("Test Case '-[p.C {literal}]' passed (0.100 seconds)."),
            format!("Test Case '-[p.C {literal}]' failed (0.100 seconds)."),
            format!("Test Case '-[p.C {literal}]' skipped (0.100 seconds)."),
            format!(
                "Test Case '-[p.C {literal}]' exceeded execution time allowance of 1 minute. The test may have hung; check Xcode's test report for additional diagnostics."
            ),
            format!("Test Case '-[{literal} m]' started."),
            format!("Test Suite '{literal}' started at 2026-11-06 12:08:08.118."),
            format!("Test Suite '{literal}.xctest' passed at 2026-11-06 12:08:08.118."),
            format!("Test Suite '{literal}' failed at 2026-11-06 12:08:08.118."),
            format!("/a/T.swift:33: error: -[p.C m] : XCTAssertTrue failed - {literal}"),
            format!("<unknown>:0: error: -[p.C m] : failed: caught \"E\", \"{literal}\""),
            format!("    t =     4.20s Assertion Failure: T.swift:33: {literal}"),
            format!("    t =     4.20s Tap \"{literal}\" Button"),
            format!("◇ Test \"{literal}\" started."),
            format!("\u{200b}✔ Test \"{literal}\" passed after 0.001 seconds."),
            format!("✘ Test m() recorded an issue at T.swift:1:2: Expectation failed: \"{literal}\""),
            format!("↳ {literal}"),
            format!("\u{200b}\u{200b}➜ Test \"{literal}\" skipped."),
            format!("2026-11-06 12:08:08.118250+0300 Some App[27239:2608175] [general] {literal} process: E"),
            format!("2026-11-06 12:08:08.118250+0300 Some App[27239:2608175] {literal} process: E"),
            format!("2026-11-06 12:08:08.118250+0300 xctest[27239:2608175] note: {literal}"),
            format!("2026-11-06 12:08:08.118 App[1:a3f] {literal} because of a user action"),
        ] {
            assert_eq!(literal_of(&line), None, "{line}");
        }
    }
    // A name line goes on to the progress parser, whose events carry the name.
    let mut p = XctestEventProducer::new(parser("target"));
    let events = p
        .process("Test Case '-[a.b.C Test runner exited]' started.")?
        .unwrap_or_default();
    assert!(matches!(&events[0], TestEvent::TestStarted { id } if id.clazz == JavaString::from("b.C Test runner")));
    // An assertion's message reaches the failing test's trace.
    let mut p = XctestEventProducer::new(parser("target"));
    p.process("Test Case '-[p.C m]' started.")?;
    assert!(
        p.process("/a/T.swift:33: error: -[p.C m] : Test runner exited")?
            .is_none()
    );
    let events = p
        .process("Test Case '-[p.C m]' failed (1.000 seconds).")?
        .unwrap_or_default();
    assert!(matches!(&events[0], TestEvent::TestFailed { trace: Some(trace), .. }
        if trace.to_string_lossy().ends_with("Test runner exited")));
    Ok(())
}
/// The one line of the JVM recordings that D39 changes. The recording stays its witness.
const D39_WITNESS: &str = "Test Case '-[p.C Test runner exited]' started.";
#[test]
fn every_failure_line_in_the_recordings_is_still_detected_d39() -> Result<(), Box<dyn std::error::Error>> {
    fn strings<'a>(value: &'a serde_json::Value, found: &mut Vec<&'a str>) {
        match value {
            serde_json::Value::String(text) => found.push(text),
            serde_json::Value::Array(values) => values.iter().for_each(|v| strings(v, found)),
            serde_json::Value::Object(values) => values.values().for_each(|v| strings(v, found)),
            _ => {}
        }
    }
    let mut counts = Vec::new();
    for corpus in [
        include_str!("../../../fixtures/oracle/apple-logparser/probes.jsonl"),
        include_str!("../../../fixtures/oracle/simulator-device/events.jsonl"),
        include_str!("../../../fixtures/oracle/simulator-device/tests.jsonl"),
        include_str!("../../../fixtures/oracle/test-runner/probes.jsonl"),
    ] {
        let (mut detected, mut witnesses) = (0, 0);
        for row in corpus.lines().filter(|l| !l.is_empty()) {
            let row: serde_json::Value = serde_json::from_str(row)?;
            let mut found = Vec::new();
            strings(&row, &mut found);
            for text in found {
                let Some((first, _)) = FAILURE_PATTERNS.iter().find(|(p, _)| text.contains(p)) else {
                    continue;
                };
                if text == D39_WITNESS {
                    assert_eq!(literal_of(text), None);
                    witnesses += 1;
                } else {
                    assert_eq!(literal_of(text).as_deref(), Some(*first), "{text}");
                    detected += 1;
                }
            }
        }
        counts.push((detected, witnesses));
    }
    // Counted as strings, so a recorded line and the event it produced are two.
    assert_eq!(counts, [(307, 1), (44, 0), (2, 0), (3, 0)]);
    Ok(())
}
#[test]
fn a_recorded_app_log_line_is_not_a_failure_d39() {
    // The shape of real lines in an application's xcodebuild log, which name a framework's failure to
    // terminate an extension process.
    let line = "2026-11-06 12:08:08.118250+0300 Some App[27239:2608175] [general] Failed to terminate process: Error Domain=com.apple.extensionKit.errorDomain Code=18 \"(null)\"";
    assert_eq!(literal_of(line), None);
    // The same text printed with no log header could be xcodebuild's own, so it still reports.
    assert_eq!(
        literal_of("Failed to terminate process: Error Domain=com.apple.extensionKit.errorDomain").as_deref(),
        Some("Failed to terminate")
    );
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
                    // D39: the JVM aborts on this name, recorded as TestRunFailed; here the test starts.
                    let expected = if row["case"] == "failure-literals" && text == D39_WITNESS {
                        assert_eq!(
                            entry["events"],
                            serde_json::json!(["TestRunFailed(message=Test runner exited, reason=Unknown)"])
                        );
                        serde_json::json!([
                            "TestStarted(id=Test(pkg=p, clazz=C Test runner, method=exited, metaProperties=[]))"
                        ])
                    } else {
                        entry["events"].clone()
                    };
                    assert_eq!(serde_json::to_value(events)?, expected, "{}: {text:?}", row["case"]);
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
