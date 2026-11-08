//! Port of core/src/main/kotlin/com/malinskiy/marathon/exceptions/DeviceLostException.kt
//! and the other exceptions in that package; DeviceActor.kt supplies catch order.

use marathon_config::binding::ConfigurationError;

/// Preserve class, nullable message and causal chain for classification/output.
/// Formatting stack traces is a later WP, not Rust Debug or Display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub class: String,
    pub message: Option<String>,
    pub cause: Option<Box<Failure>>,
    pub stacktrace: Option<String>,
}
impl Failure {
    /// `Throwable.toString()`: the class, then `: message` when there is one.
    pub fn to_java_string(&self) -> String {
        let mut text = self.class.clone();
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
