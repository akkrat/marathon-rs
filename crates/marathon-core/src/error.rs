//! Port of core/src/main/kotlin/com/malinskiy/marathon/exceptions/DeviceLostException.kt
//! and the other exceptions in that package; DeviceActor.kt supplies catch order.

use marathon_config::binding::ConfigurationError;
pub use marathon_error::*;

/// Preserve class, nullable message and causal chain for classification/output.
/// Formatting stack traces is a later WP, not Rust Debug or Display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: Option<String>,
    pub cause: Option<Box<Failure>>,
    pub stacktrace: Option<String>,
}
impl Failure {
    /// Typed producers get their rendered class only from the legacy mapping.
    pub fn from_kind(kind: FailureKind, message: Option<String>) -> Self {
        Self {
            kind,
            message,
            cause: None,
            stacktrace: None,
        }
    }

    /// `Throwable.toString()`: the class, then `: message` when there is one.
    pub fn to_java_string(&self) -> String {
        let mut text = self.kind.legacy_class().to_owned();
        if let Some(message) = &self.message {
            text.push_str(": ");
            text.push_str(&message.to_owned());
        }
        text
    }
    /// `Throwable.stackTraceToString()` over the causal chain. JVM frames are not
    /// recorded (PLAN §3.10); `stacktrace` holds frame lines kept from the source.
    pub fn stack_trace(&self) -> String {
        let mut text = String::default();
        let mut current = Some(self);
        while let Some(failure) = current {
            if !std::ptr::eq(failure, self) {
                text.push_str("Caused by: ");
            }
            text.push_str(&failure.to_java_string());
            text.push('\n');
            if let Some(frames) = &failure.stacktrace {
                text.push_str(&frames.clone());
                if !frames.is_empty() && !frames.ends_with('\n') {
                    text.push('\n');
                }
            }
            current = failure.cause.as_deref();
        }
        text
    }
}
impl Cancellation {
    /// The CancellationException Kotlin throws for this cancellation.
    pub fn failure(&self) -> &Failure {
        match self {
            Self::Requested(f) | Self::Parent(f) | Self::Timeout { detail: f, .. } => f,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cancellation {
    Requested(Failure),
    /// Scope identity prevents catching a nested timeout as one's own timeout.
    Timeout {
        scope: u64,
        detail: Failure,
    },
    Parent(Failure),
}
/// Task and service failures never encode cancellation in an exception class name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationError {
    Cancelled(Cancellation),
    Failed(Failure),
}
impl From<Cancellation> for OperationError {
    fn from(reason: Cancellation) -> Self {
        Self::Cancelled(reason)
    }
}
impl From<Failure> for OperationError {
    fn from(failure: Failure) -> Self {
        Self::Failed(failure)
    }
}
pub type OperationResult<T> = Result<T, OperationError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeavePoolReason {
    NotBooted,
    NotAdmitted,
    /// U163: a file transfer to the device's worker made no progress for its whole limit.
    TransferStalled,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VendorError {
    Cancelled(Cancellation),
    LeavePool {
        reason: LeavePoolReason,
        detail: Failure,
    },
    DeviceLost(Failure),
    DeviceSetup(Failure),
    IncompatibleDevice(Failure),
    BatchExecution(Failure),
    TestParsing(Failure),
    NoDevices(Failure),
    Transfer(Failure),
    /// Recoverable during execute, retried during prepare. Do not flatten known
    /// cancellation/lost variants into this catch-all.
    Other(Failure),
}
impl VendorError {
    /// The thrown exception, whatever its classification.
    pub fn failure(&self) -> &Failure {
        match self {
            Self::Cancelled(c) => c.failure(),
            Self::LeavePool { detail, .. } => detail,
            Self::DeviceLost(f)
            | Self::DeviceSetup(f)
            | Self::IncompatibleDevice(f)
            | Self::BatchExecution(f)
            | Self::TestParsing(f)
            | Self::NoDevices(f)
            | Self::Transfer(f)
            | Self::Other(f) => f,
        }
    }
}
pub type VendorResult<T> = Result<T, VendorError>;

/// Internal classification, not CLI exit codes. Marathon.kt:92–101 owns the
/// ignoreFailures policy; config parsing may fail outside that catch entirely.
pub enum RunOutcome {
    Passed,
    TestsFailed,
    NoDevices(Failure),
    NoTests(Failure),
    Configuration(Box<ConfigurationError>),
    Crashed(Failure),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_failures_render_nullable_messages_frames_and_causes_unchanged() {
        let mut failure = Failure::from_kind(FailureKind::IllegalState, Some(String::new()));
        failure.stacktrace = Some("\tat source.frame(Source:1)".into());
        failure.cause = Some(Box::new(Failure::from_kind(FailureKind::Io, None)));
        assert_eq!(failure.kind.legacy_class(), FailureKind::IllegalState.legacy_class());
        assert_eq!(failure.to_java_string(), "java.lang.IllegalStateException: ");
        assert_eq!(
            failure.stack_trace(),
            "java.lang.IllegalStateException: \n\tat source.frame(Source:1)\nCaused by: java.io.IOException\n"
        );
        assert_eq!(failure, failure.clone());
    }

    #[test]
    fn external_class_and_stack_text_remain_opaque() {
        // This models a class and trace supplied by an external runner.
        let kind = FailureKind::External("runner.CustomFailure".into());
        let mut failure = Failure::from_kind(kind.clone(), Some("java.io.IOException: remote".into()));
        failure.stacktrace = Some("external.Frame\n".into());
        assert_eq!(failure.kind.legacy_class(), kind.legacy_class());
        assert_eq!(
            failure.stack_trace(),
            "runner.CustomFailure: java.io.IOException: remote\nexternal.Frame\n"
        );
    }
}
