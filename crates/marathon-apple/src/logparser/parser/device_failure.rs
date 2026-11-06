//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/logparser/parser/DeviceFailureParser.kt
use super::{DeviceFailureReason, ParseError};
use crate::test::TestEvent;
use DeviceFailureReason::*;
use jcompat::{
    JavaRegex,
    regex_patterns::{FAILING_TEST, TEST_CASE_FINISHED, TEST_CASE_STARTED, TIMEOUT_TEST},
};
use std::sync::LazyLock;

pub const FAILURE_PATTERNS: &[(&str, DeviceFailureReason)] = &[
    ("Failed to install or launch the test runner", FailedRunner),
    ("Software caused connection abort", ConnectionAbort),
    (
        "Unable to find a destination matching the provided destination specifier",
        InvalidSimulatorIdentifier,
    ),
    ("Terminating since there is no system app", SystemAppUnavailable),
    (
        "Exiting because the workspace server has disconnected",
        SystemAppUnavailable,
    ),
    ("Not authorized for performing UI testing PropertyActions", Unknown),
    ("Timed out waiting for automation session", Unknown),
    ("Failed to terminate", Unknown),
    ("Failed to launch app with identifier", Unknown),
    ("Test runner exited before starting test execution", Unknown),
    ("Early unexpected exit, operation never finished bootstrapping", Unknown),
    ("Connection peer refused channel request", UnreachableHost),
    ("CoreSimulatorService connection became invalid", ServicesUnavailable),
    ("CoreSimulatorService connection interrupted", ServicesUnavailable),
    ("Simulator services will no longer be available", ServicesUnavailable),
    ("Unable to locate device set", ServicesUnavailable),
    ("Test runner exited", Unknown),
];
// Fixed forms for D39, spelled like the other fixed patterns: Java's dot excludes five line terminators.
static ACTIVITY_LINE: LazyLock<JavaRegex> = LazyLock::new(|| {
    JavaRegex::compile(
        r"[ \t]+t = +[0-9]+\.[0-9]+s [^\n\r\x{85}\x{2028}\x{2029}]*",
        "XCTEST_ACTIVITY_LINE",
    )
    .expect("tested fixed marathon pattern")
});
static LOG_HEADER: LazyLock<JavaRegex> = LazyLock::new(|| {
    JavaRegex::compile(
        r"^[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?(?:[+\-][0-9]{2}:?[0-9]{2})? ([^\[\n\r\x{85}\x{2028}\x{2029}]+)\[[0-9]+:[0-9a-fA-Fx]+\] ",
        "XCTEST_LOG_HEADER",
    )
    .expect("tested fixed marathon pattern")
});

/// The part of a stdout line that can report a device failure (D39, A48).
///
/// Missing a real failure leaves the batch waiting for its timeout, so this removes only forms a
/// test or an application writes, and every other line is searched whole, as in Kotlin. Forms are
/// in docs/wp/x17-findings.md.
fn failure_text(line: &str) -> Option<&str> {
    // A test's name, which is the test's own text. Whatever follows it can still report a failure.
    for pattern in [&*TEST_CASE_STARTED, &*TEST_CASE_FINISHED, &*TIMEOUT_TEST] {
        if pattern.matches(line) {
            let name = pattern.find(line).and_then(|captures| captures.get(2))?;
            return Some(&line[name.end()..]);
        }
    }
    if let Some(name) = line.strip_prefix("Test Suite '")
        && let Some(end) = name.rfind("' ")
        && ["started at ", "passed at ", "failed at "]
            .iter()
            .any(|state| name[end + 2..].starts_with(state))
    {
        return Some(&name[end..]);
    }
    // An assertion's message and a UI test's activity belong to a test in progress: its runner is
    // alive and reports that test's result, so no batch waits for a timeout here.
    if FAILING_TEST.matches(line) || ACTIVITY_LINE.matches(line) || is_swift_testing_event(line) {
        return None;
    }
    if let Some(header) = LOG_HEADER.find(line) {
        let process = header.get(1).map_or("", |process| process.as_str());
        let message = &line[header.get(0).map_or(0, |all| all.end())..];
        // Only xcodebuild, simctl and the test runner print into xcodebuild's stdout besides the
        // application under test. XCTest also runs inside a hosted test's application, whose name
        // says nothing, so there the framework's own sentences still count.
        let tool = process == "xcodebuild" || process == "simctl" || process.ends_with("Runner");
        return (tool || is_framework_sentence(message)).then_some(line);
    }
    Some(line)
}

/// `◇ Test "name" started.`, recorded with leading zero-width spaces too. Swift Testing's display
/// names and issue comments are free text. Its macOS symbols are in the private use plane.
fn is_swift_testing_event(line: &str) -> bool {
    let mut chars = line.trim_start_matches('\u{200b}').chars();
    let symbol = chars.next();
    chars.next() == Some(' ')
        && symbol.is_some_and(|c| matches!(c, '◇' | '✔' | '✘' | '➜' | '↳' | '⚠') || c >= '\u{100000}')
}

/// What XCTest's own frameworks log inside an application they run in. An application writing
/// these exact sentences cannot be told apart, and stays a false alarm.
fn is_framework_sentence(message: &str) -> bool {
    let message = match message.strip_prefix('[').and_then(|m| m.split_once("] ")) {
        Some((_, rest)) => rest,
        None => message,
    };
    let sentence = message.strip_suffix('.').unwrap_or(message);
    sentence == "Terminating since there is no system app"
        || sentence == "Exiting because the workspace server has disconnected"
        || message.starts_with("Connection peer refused channel request for \"")
}

fn find_failure(text: &str) -> Option<&'static (&'static str, DeviceFailureReason)> {
    FAILURE_PATTERNS.iter().find(|(p, _)| text.contains(p))
}

#[derive(Default)]
pub struct DeviceFailureParser;
impl DeviceFailureParser {
    pub fn process(&self, line: &str) -> Result<Option<Vec<TestEvent>>, ParseError> {
        // Most lines hold no literal, and failure_text is only worth its regexes for those that do.
        find_failure(line)
            .and_then(|_| failure_text(line))
            .and_then(find_failure)
            .map_or(Ok(None), |(p, reason)| {
                Ok(Some(vec![TestEvent::TestRunFailed {
                    message: (*p).into(),
                    reason: *reason,
                }]))
            })
    }
}
