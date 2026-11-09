//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/logparser/parser/{DeviceFailureParserTest,TestRunProgressParserTest}.kt
mod support;
use jcompat::logging::MarathonLogging;
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
                actual.push(event.to_kotlin_string().to_owned());
            }
        }
        // D45: Kotlin's expectation holds except that a hang now reports its line as the trace.
        let expected = match input.lines().find(|line| line.contains(D45_MARKER)) {
            Some(line) => {
                assert_eq!(expected.matches("trace=null").count(), 1, "{name}");
                expected.replace("trace=null", &format!("trace={line}"))
            }
            None => expected,
        };
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
        assert!(matches!(&events[0],TestEvent::TestStarted{id} if id.pkg==pkg && id.clazz==clazz));
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
        "2018-09-15 01:08:49.715033+0700 Tests-Runner[1000:2000] Terminating since there is no system app.",
        "Terminating since there is no system app",
    ),
    (
        "2018-09-15 01:08:49.715033+0700 Some App[1000:2000] Terminating since there is no system app.",
        "Terminating since there is no system app",
    ),
    (
        "XCTRunner[4242:4242] Exiting because the workspace server has disconnected.",
        "Exiting because the workspace server has disconnected",
    ),
    (
        "2018-09-15 01:08:51.148 Some App[1000:a3f] Exiting because the workspace server has disconnected.",
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
        "2018-09-15 01:08:49.715033+0700 xcodebuild[1:2] Connection peer refused channel request for \"x\"; channel canceled <DTXChannel: 0x1>",
        "Connection peer refused channel request",
    ),
    (
        "2018-09-15 01:08:49.715033+0700 Some App[1:2] Connection peer refused channel request for \"x\"; channel canceled <DTXChannel: 0x1>",
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
        "Test Suite 'All tests' started at 2018-09-15 01:08:51.148. Test runner exited",
        "Test runner exited",
    ),
];
fn literal_of(line: &str) -> Option<String> {
    match DeviceFailureParser.process(line) {
        Ok(Some(events)) => match &events[0] {
            TestEvent::TestRunFailed { message, .. } => Some(message.to_owned()),
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
            format!("2018-09-15 01:08:49.715033+0700 xcodebuild[1:2] [MT] {literal}"),
            format!("2018-09-15 01:08:49.715033+0700 simctl[1:2] {literal}"),
            format!("2018-09-15 01:08:49.715033+0700 Tests-Runner[1:2] {literal}"),
            format!("2018-09-15 01:08:49.715033+0700 XCTRunner[1:2] {literal}"),
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
            format!("Test Suite '{literal}' started at 2018-09-15 01:08:51.148."),
            format!("Test Suite '{literal}.xctest' passed at 2018-09-15 01:08:51.148."),
            format!("Test Suite '{literal}' failed at 2018-09-15 01:08:51.148."),
            format!("/a/T.swift:33: error: -[p.C m] : XCTAssertTrue failed - {literal}"),
            format!("<unknown>:0: error: -[p.C m] : failed: caught \"E\", \"{literal}\""),
            format!("    t =     4.20s Assertion Failure: T.swift:33: XCTAssertTrue failed - {literal}"),
            format!("    t =     4.20s Assertion Failure: failed - {literal}"),
            format!("    t =     4.20s Tap \"{literal}\" Button"),
            format!("    t =     4.20s {literal}"),
            format!("/a/T.swift:33: error: -[p.C m] : failed - [a b] : {literal}"),
            format!("/a/T.swift:33: error: -[p.C m] : failed - x: error: -[p.D n] : {literal}"),
            format!(
                "/a/T.swift:33: error: -[Module.C test()] : XCTAssertEqual failed: (\"{literal}\") is not equal to (\"x\")"
            ),
            format!("/a/T.swift:33: error: +[p.C m] : failed - {literal}"),
            format!("<unknown>:0: error: -[p.C m] : failed: caught error: \"{literal}\""),
            format!("⚠\u{fe0e} Test \"{literal}\" recorded a warning."),
            format!("✔\u{fe0e} Test \"{literal}\" passed."),
            format!("◇ Test \"{literal}\" started."),
            format!("\u{200b}✔ Test \"{literal}\" passed after 0.001 seconds."),
            format!("✘ Test m() recorded an issue at T.swift:1:2: Expectation failed: \"{literal}\""),
            format!("↳ {literal}"),
            format!("\u{200b}\u{200b}➜ Test \"{literal}\" skipped."),
            format!("2018-09-15 01:08:49.715033+0700 Some App[1000:2000] [general] {literal} process: E"),
            format!("2018-09-15 01:08:49.715033+0700 Some App[1000:2000] {literal} process: E"),
            format!("2018-09-15 01:08:49.715033+0700 xctest[1000:2000] note: {literal}"),
            format!("2018-09-15 01:08:51.148 App[1:a3f] {literal} because of a user action"),
        ] {
            assert_eq!(literal_of(&line), None, "{line}");
        }
    }
    // A name line goes on to the progress parser, whose events carry the name.
    let mut p = XctestEventProducer::new(parser("target"));
    let events = p
        .process("Test Case '-[a.b.C Test runner exited]' started.")?
        .unwrap_or_default();
    assert!(matches!(&events[0], TestEvent::TestStarted { id } if id.clazz == "b.C Test runner"));
    // An assertion's message reaches the failing test's trace.
    let mut p = XctestEventProducer::new(parser("target"));
    p.process("Test Case '-[p.C m]' started.")?;
    assert!(
        p.process("/a/T.swift:33: error: -[p.C m] : XCTAssertTrue failed - Test runner exited")?
            .is_none()
    );
    let events = p
        .process("Test Case '-[p.C m]' failed (1.000 seconds).")?
        .unwrap_or_default();
    assert!(matches!(&events[0], TestEvent::TestFailed { trace: Some(trace), .. }
        if trace.to_owned().ends_with("Test runner exited")));
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
/// A failure the UI test framework records itself appears raw in the message, unlike a test's own
/// text, and has to keep recycling a sick simulator. Reasoned from Xcode's formats: no recording
/// holds a raw one.
#[test]
fn a_raw_framework_failure_in_an_assertion_is_a_failure_d39() {
    for (literal, _) in FAILURE_PATTERNS {
        for line in [
            format!("/a/T.swift:33: error: -[p.C m] : {literal}"),
            format!("/a/T.swift:33: error: -[p.C m] : {literal} com.example.app:1234"),
            format!("/a/T.swift:33: error: -[Module.C test()] : {literal}"),
            format!("/a b/T.swift:33: error: +[p.C m] :   {literal}"),
            format!("<unknown>:0: error: -[p.C m] : {literal}"),
            format!("    t =     4.20s Assertion Failure: T.swift:33: {literal}"),
            format!("    t =     4.20s     Assertion Failure: <unknown>:0: {literal}"),
            format!("    t =     4.20s Assertion Failure: {literal}"),
            format!("    t =     4.20s Assertion Failure: {literal} com.example.app:1: x"),
        ] {
            assert!(literal_of(&line).is_some(), "{line}");
        }
    }
    // Which literal reports follows the list, as it does on any other line.
    assert_eq!(
        literal_of("/a/T.swift:33: error: -[p.C m] : Test runner exited: Terminating since there is no system app.")
            .as_deref(),
        Some("Terminating since there is no system app")
    );
    // A test name or path holding a literal is no reason to report a test's own text.
    for line in [
        "/a/Failed to terminate/T.swift:33: error: -[p.C m] : XCTAssertTrue failed - x",
        "/a/T.swift:33: error: -[p.C Failed to terminate] : XCTAssertTrue failed - x",
    ] {
        assert_eq!(literal_of(line), None, "{line}");
    }
}
#[test]
fn a_recorded_app_log_line_is_not_a_failure_d39() {
    // The shape of real lines in an application's xcodebuild log, which name a framework's failure to
    // terminate an extension process.
    let line = "2018-09-15 01:08:49.715033+0700 Some App[1000:2000] [general] Failed to terminate process: Error Domain=com.apple.extensionKit.errorDomain Code=18 \"(null)\"";
    assert_eq!(literal_of(line), None);
    // The same text printed with no log header could be xcodebuild's own, so it still reports.
    assert_eq!(
        literal_of("Failed to terminate process: Error Domain=com.apple.extensionKit.errorDomain").as_deref(),
        Some("Failed to terminate")
    );
}
const D45_MARKER: &str = "exceeded execution time allowance of";
/// D45: the JVM rows where parseAppleDuration threw (A47), with the milliseconds and the seconds of the log
/// line that native builds report instead. The recordings stay as they are, as the witness.
const D45_RECORDED_THROWS: &[(&str, i64, i64)] = &[
    ("real/xcode27-timeout", 60_000, 60),
    ("duration/1 minute", 60_000, 60),
    ("duration/1 hour", 3_600_000, 3600),
    ("duration/1 minute, 30 seconds", 90_000, 90),
    ("duration/ 3 minutes", 180_000, 180),
];
fn hung(allowance: &str) -> String {
    format!("Test Case '-[p.C m]' exceeded execution time allowance of {allowance}. The test may have hung")
}
fn elapsed_of(allowance: &str) -> i64 {
    match parser("").process(&hung(allowance)) {
        Ok(Some(events)) => match &events[..] {
            [
                TestEvent::TestFailed {
                    start_time, end_time, ..
                },
            ] => end_time - start_time,
            other => panic!("{allowance:?}: {other:?}"),
        },
        other => panic!("{allowance:?}: {other:?}"),
    }
}
#[test]
fn no_allowance_stops_a_hung_test_from_failing_d45() {
    for (allowance, millis) in [
        // What NSDateComponentsFormatter's full style prints: see docs/wp/x28-findings.md.
        ("1 second", 1_000),
        ("30 seconds", 30_000),
        ("1 minute", 60_000),
        ("2 minutes", 120_000),
        ("1 minute, 1 second", 61_000),
        ("1 minute, 30 seconds", 90_000),
        ("1 hour", 3_600_000),
        ("2 hours", 7_200_000),
        ("1 hour, 1 minute", 3_660_000),
        ("1 hour, 1 minute, 1 second", 3_661_000),
        ("1 day", 86_400_000),
        ("1 day, 1 hour", 90_000_000),
        ("1 week", 604_800_000),
        // Fractions, including those Duration.parse only takes on seconds.
        ("1.5 seconds", 1_500),
        ("1.5 minutes", 90_000),
        ("0.5 hours", 1_800_000),
        // Whitespace, which Kotlin's untrimmed receiver kept.
        (" 3 minutes", 180_000),
        ("1\u{a0}minute", 60_000),
        // The abbreviated style.
        ("1 min", 60_000),
        ("1 hr, 30 min", 5_400_000),
        ("1 minute and 30 seconds", 90_000),
        // Plurals in an order that Kotlin's rewrite turns into an unreadable ISO text.
        ("30 seconds, 2 minutes", 150_000),
    ] {
        assert_eq!(elapsed_of(allowance), millis, "{allowance:?}");
    }
}
#[test]
fn an_allowance_that_is_not_understood_still_fails_the_test_d45() {
    for allowance in [
        "soon",
        "1 Minute und 30 Sekunden",
        "60",
        "minutes",
        "1 fortnight",
        "NaN seconds x",
        "inf minutes, 1",
        "99999999999999999999 weeks",
        "9223372036854775807 hours",
    ] {
        assert_eq!(elapsed_of(allowance), 0, "{allowance:?}");
    }
}
#[test]
fn every_form_kotlin_could_read_is_read_the_same_d45() {
    for (allowance, millis) in [
        ("2 minutes", 120_000),
        ("1.5 seconds", 1_500),
        ("2 hours", 7_200_000),
        // Odd, but Duration.parse took them.
        ("1H", 3_600_000),
        ("-5 seconds", -5_000),
    ] {
        assert_eq!(elapsed_of(allowance), millis, "{allowance:?}");
    }
}
#[test]
fn a_hung_test_reports_its_line_unless_it_has_a_failing_line_already_d45() -> Result<(), ParseError> {
    let trace_of = |lines: &[String]| -> Result<Option<String>, ParseError> {
        let mut p = parser("");
        let mut trace = None;
        for line in lines {
            for event in p.process(line)?.into_iter().flatten() {
                if let TestEvent::TestFailed { trace: t, .. } = event {
                    trace = Some(t.map(|t| t.to_owned()));
                }
            }
        }
        Ok(trace.flatten())
    };
    let line = hung("1 minute");
    assert_eq!(trace_of(std::slice::from_ref(&line))?, Some(line.clone()));
    assert_eq!(
        trace_of(&[
            "Test Case '-[p.C m]' started.".to_owned(),
            "/a:1: error: -[p.C m] : first".to_owned(),
            line.clone(),
        ])?,
        Some("/a:1:first".to_owned())
    );
    // A later test isn't handed the last hang's line.
    assert_eq!(
        trace_of(&[
            line,
            "Test Case '-[p.C n]' started.".to_owned(),
            "Test Case '-[p.C n]' failed (1.000 seconds).".to_owned(),
        ])?,
        None
    );
    Ok(())
}
/// D45 on a real stream: an iOS 18.6 simulator under Xcode 27.2, each hang restarting the runner. Before, the first
/// hang's `1 minute` threw and ended the batch here. See fixtures/PROVENANCE.md.
#[test]
fn a_real_stream_with_hung_tests_reports_every_test_and_no_batch_failure_d45() -> Result<(), ParseError> {
    let mut p = XctestEventProducer::new(parser("TimeoutTests"));
    let mut seen = vec![];
    for line in include_str!("../../../fixtures/apple-logparser/xcode27-ios-hang.log.input").lines() {
        for event in p.process(line)?.into_iter().flatten() {
            seen.push(match event {
                TestEvent::TestFailed {
                    id,
                    end_time,
                    start_time,
                    trace,
                } => format!(
                    "failed {}.{}.{} {} {}",
                    id.pkg.to_owned(),
                    id.clazz.to_owned(),
                    id.method.to_owned(),
                    end_time - start_time,
                    trace.is_some_and(|t| t == line)
                ),
                TestEvent::TestPassed { id, .. } => format!(
                    "passed {}.{}.{}",
                    id.pkg.to_owned(),
                    id.clazz.to_owned(),
                    id.method.to_owned()
                ),
                TestEvent::TestStarted { .. } => continue,
                other => panic!("{line:?}: {other:?}"),
            });
        }
    }
    assert_eq!(
        seen,
        [
            "failed TimeoutTests.TimeoutTests.testA_hang60 60000 true",
            "passed TimeoutTests.TimeoutTests.testB_passes",
            // A 90 s allowance: XCTest rounds it up to the minute it prints.
            "failed TimeoutTests.TimeoutTests.testC_hang90 120000 true",
            // A nested Swift class logs a mangled name with no dot, so A5's split doesn't apply.
            "passed TimeoutTests._TtCC12TimeoutTests12TimeoutTests6Nested.testNested",
        ]
    );
    Ok(())
}
/// D45 is only about the allowance's text: a malformed result line still escapes as Kotlin's does.
#[test]
fn a_malformed_test_duration_escapes_as_a_typed_error() {
    let error = parser("")
        .process("Test Case '-[p.C m]' failed (1.2.3 seconds).")
        .unwrap_err();
    assert!(matches!(&error, ParseError::NumberFormat { .. }));
    assert_eq!(error.to_string(), "invalid number: multiple points");
}
#[test]
fn the_d45_allowance_hides_nothing_else_in_the_recordings() -> Result<(), Box<dyn std::error::Error>> {
    let mut throws = vec![];
    for line in include_str!("../../../fixtures/oracle/apple-logparser/probes.jsonl")
        .split('\n')
        .filter(|l| !l.is_empty())
    {
        let row: serde_json::Value = serde_json::from_str(line)?;
        if row["kind"] != "stream" {
            continue;
        }
        for entry in row["rows"].as_array().unwrap() {
            if entry["exception"][0].as_str().is_some_and(|class| {
                marathon_test_support::recorded_errors::kind(class) == marathon_error::FailureKind::DateTimeParse
            }) {
                assert!(entry["line"].as_str().unwrap().contains(D45_MARKER));
                throws.push(row["case"].as_str().unwrap().to_owned());
            }
        }
    }
    // Every throw is the witness of A47, each listed once. A row that stopped throwing or a new one fails here.
    let listed = D45_RECORDED_THROWS.iter().map(|(case, ..)| (*case).to_owned());
    assert_eq!(throws, listed.collect::<Vec<_>>());
    Ok(())
}

#[test]
fn jvm_streams_replay_events_errors_and_debug_lines() -> Result<(), Box<dyn std::error::Error>> {
    use jcompat::logging::{Level, Vendor};
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
            let d45 = D45_RECORDED_THROWS.iter().find(|(case, ..)| row["case"] == *case);
            let result = if row["producer"].as_bool().unwrap() {
                producer.process(text)
            } else {
                producer.progress.process(text)
            };
            match result {
                Ok(events) => {
                    let thrown = text.contains(D45_MARKER) && d45.is_some();
                    assert_eq!(entry["exception"].is_null(), !thrown, "{}: {text:?}", row["case"]);
                    let events = events.map(|v| v.iter().map(|e| e.to_kotlin_string().to_owned()).collect::<Vec<_>>());
                    // D39: the JVM aborts on this name, recorded as TestRunFailed; here the test starts.
                    let expected = if row["case"] == "failure-literals" && text == D39_WITNESS {
                        assert_eq!(
                            entry["events"],
                            serde_json::json!(["TestRunFailed(message=Test runner exited, reason=Unknown)"])
                        );
                        serde_json::json!([
                            "TestStarted(id=Test(pkg=p, clazz=C Test runner, method=exited, metaProperties=[]))"
                        ])
                    } else if thrown {
                        // D45: what the JVM threw on is now the failure it reports for the plural forms.
                        let (_, millis, _) = d45.unwrap();
                        let name = text.split('\'').nth(1).unwrap();
                        let name = name.trim_start_matches("-[").trim_end_matches(']');
                        let (class, method) = name.split_once(' ').unwrap();
                        let (pkg, clazz) = class.split_once('.').unwrap();
                        serde_json::json!([format!(
                            "TestFailed(id=Test(pkg={pkg}, clazz={clazz}, method={method}, metaProperties=[]), startTime={}, endTime=1537187696000, trace={text})",
                            1537187696000_i64 - millis
                        )])
                    } else if text.contains(D45_MARKER) {
                        // D45: the JVM's hung test had no trace unless a failing line came first.
                        let recorded = entry["events"][0].as_str().unwrap();
                        serde_json::json!([recorded.replace("trace=null", &format!("trace={text}"))])
                    } else {
                        entry["events"].clone()
                    };
                    assert_eq!(serde_json::to_value(events)?, expected, "{}: {text:?}", row["case"]);
                }
                Err(error) => {
                    let f = error.to_failure();
                    assert_eq!(
                        serde_json::json!([f.kind.identifier(), f.message.map(|s| s.to_owned())]),
                        serde_json::json!([
                            marathon_test_support::recorded_errors::kind(entry["exception"][0].as_str().unwrap())
                                .identifier(),
                            entry["exception"][1]
                        ]),
                        "{}: {text:?}",
                        row["case"]
                    );
                }
            }
        }
        let mut recorded_logs = row["logs"].as_array().unwrap().clone();
        if let Some((_, _, seconds)) = D45_RECORDED_THROWS.iter().find(|(case, ..)| row["case"] == *case) {
            // D45: the JVM threw before this line.
            let name = if row["case"] == "real/xcode27-timeout" {
                "TimeoutTests.TimeoutTests.testHang"
            } else {
                "p.C.m"
            };
            recorded_logs.push(serde_json::json!({"logger":"TestRunProgressParser","level":"DEBUG","message":format!("Test {name} finished with result <timeout> after {seconds} seconds")}));
        }
        assert_eq!(
            serde_json::to_value(logs.lock().unwrap().clone())?,
            serde_json::Value::Array(recorded_logs),
            "{}",
            row["case"]
        );
    }
    Ok(())
}
