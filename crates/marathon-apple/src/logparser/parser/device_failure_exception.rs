//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/logparser/parser/DeviceFailureException.kt

use crate::cmd::{failure, remote::ssh::sshj::SshFailure};
use jcompat::JavaRegex;
use marathon_core::error::Failure;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceFailureReason {
    Unknown,
    FailedRunner,
    ConnectionAbort,
    InvalidSimulatorIdentifier,
    UnreachableHost,
    ServicesUnavailable,
    IncompatibleDevice,
    SystemAppUnavailable,
}
impl DeviceFailureReason {
    pub fn is_batch_failure(self) -> bool {
        matches!(self, Self::Unknown | Self::FailedRunner)
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::FailedRunner => "FailedRunner",
            Self::ConnectionAbort => "ConnectionAbort",
            Self::InvalidSimulatorIdentifier => "InvalidSimulatorIdentifier",
            Self::UnreachableHost => "UnreachableHost",
            Self::ServicesUnavailable => "ServicesUnavailable",
            Self::IncompatibleDevice => "IncompatibleDevice",
            Self::SystemAppUnavailable => "SystemAppUnavailable",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceFailureException {
    pub reason: DeviceFailureReason,
    pub message: String,
    pub cause: Option<SshFailure>,
}
impl DeviceFailureException {
    pub fn new(reason: DeviceFailureReason, message: Option<String>, cause: Option<SshFailure>) -> Self {
        Self {
            reason,
            message: message.unwrap_or_else(|| reason.name().into()),
            cause,
        }
    }
    pub fn from_regex(reason: DeviceFailureReason, regex: &JavaRegex) -> Self {
        Self::new(reason, Some(regex.to_string()), None)
    }
    pub fn to_failure(&self) -> Failure {
        let mut result = failure(
            "com.malinskiy.marathon.apple.logparser.parser.DeviceFailureException",
            &self.message,
        );
        result.cause = self.cause.as_ref().map(|cause| Box::new(cause.to_failure()));
        result
    }
}
impl std::fmt::Display for DeviceFailureException {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for DeviceFailureException {}
