//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/logparser/parser/DeviceFailureParser.kt
use super::{DeviceFailureReason, ParseError};
use crate::test::TestEvent;
use DeviceFailureReason::*;

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
#[derive(Default)]
pub struct DeviceFailureParser;
impl DeviceFailureParser {
    pub fn process(&self, line: &str) -> Result<Option<Vec<TestEvent>>, ParseError> {
        Ok(FAILURE_PATTERNS
            .iter()
            .find(|(p, _)| line.contains(p))
            .map(|(p, reason)| {
                vec![TestEvent::TestRunFailed {
                    message: (*p).into(),
                    reason: *reason,
                }]
            }))
    }
}
