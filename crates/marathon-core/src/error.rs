//! Port of core/src/main/kotlin/com/malinskiy/marathon/exceptions/DeviceLostException.kt
//! and the other exceptions in that package; DeviceActor.kt supplies catch order.

use marathon_config::binding::ConfigurationError;
pub use marathon_error::*;

/// Preserve kind, nullable message and causal chain for classification/output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: Option<String>,
    pub cause: Option<Box<Failure>>,
    pub stacktrace: Option<String>,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.kind.fmt(f)?;
        if let Some(message) = &self.message {
            write!(f, ": {message}")?;
        }
        Ok(())
    }
}

impl Failure {
    /// Preserve classification independently of its rendered description.
    pub fn from_kind(kind: FailureKind, message: Option<String>) -> Self {
        Self {
            kind,
            message,
            cause: None,
            stacktrace: None,
        }
    }

    /// Diagnostic projection only: stored failures and report text retain their bytes.
    pub fn for_log(&self) -> Self {
        let mut rendered = self.clone();
        rendered.cause = self.cause.as_deref().map(|cause| Box::new(cause.for_log()));
        if let Some(message) = self
            .message
            .as_deref()
            .and_then(|message| generated_message_for_log(&self.kind, message))
        {
            rendered.message = Some(message.into());
        }
        if !matches!(self.kind, FailureKind::External(_)) {
            rendered.stacktrace = self.stacktrace.as_deref().map(trace_for_log);
        }
        if matches!(self.kind, FailureKind::Ssh(_))
            && let (Some(cause), Some(diagnostic)) = (self.cause.as_deref(), rendered.cause.as_deref())
            && self.message == cause.message
        {
            rendered.message = diagnostic.message.clone();
        }
        if self.kind == FailureKind::DeviceLost
            && let (Some(cause), Some(diagnostic)) = (self.cause.as_deref(), rendered.cause.as_deref())
            && self.message.as_deref() == Some(cause.to_string().as_str())
        {
            rendered.message = Some(diagnostic.to_string());
        }
        if self.kind == FailureKind::DeviceLost
            && let (Some(cause), Some(diagnostic)) = (self.cause.as_deref(), rendered.cause.as_deref())
            && cause.kind == FailureKind::JobCancelled
            && cause.message.as_deref() == Some("LazyDeferredCoroutine was cancelled")
            && self.message.as_deref() == Some(format!("{cause}; job=LazyDeferredCoroutine{{Cancelled}}").as_str())
        {
            rendered.message = Some(diagnostic.to_string());
        }
        rendered
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
            text.push_str(&failure.to_string());
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
fn generated_message_for_log(kind: &FailureKind, message: &str) -> Option<&'static str> {
    match (kind, message) {
        (FailureKind::JobCancelled, "DeferredCoroutine was cancelled" | "LazyDeferredCoroutine was cancelled") => {
            Some("task was cancelled")
        }
        (FailureKind::Timeout | FailureKind::Ssh(_), "Timeout expired: 30000 MILLISECONDS") => {
            Some("no response after 30 s")
        }
        (FailureKind::Timeout | FailureKind::Ssh(_), "Timeout expired: 1 NANOSECONDS") => {
            Some("no response after 1 ns")
        }
        _ => None,
    }
}

// flatten_suppressed retains generated headers in the otherwise opaque frame text.
fn trace_for_log(trace: &str) -> String {
    trace
        .split_inclusive('\n')
        .map(|line| {
            let body = line.trim_start_matches('\t');
            let indentation = &line[..line.len() - body.len()];
            let (label, body) = ["Suppressed: ", "Caused by: "]
                .into_iter()
                .find_map(|label| body.strip_prefix(label).map(|body| (label, body)))
                .unwrap_or(("", body));
            let (body, newline) = body.strip_suffix('\n').map_or((body, ""), |body| (body, "\n"));
            match generated_header_for_log(body) {
                Some(header) => format!("{indentation}{label}{header}{newline}"),
                None => line.to_owned(),
            }
        })
        .collect()
}
fn generated_header_for_log(header: &str) -> Option<String> {
    let kinds = [
        FailureKind::DeviceLost,
        FailureKind::JobCancelled,
        FailureKind::Timeout,
        FailureKind::Ssh(SshKind::Connection),
        FailureKind::Ssh(SshKind::Transport),
        FailureKind::Ssh(SshKind::Authentication),
    ];
    let mut rest = header;
    let mut prefix = String::new();
    loop {
        let (kind, body) = kinds
            .iter()
            .find_map(|kind| rest.strip_prefix(format!("{kind}: ").as_str()).map(|body| (kind, body)))?;
        prefix.push_str(&format!("{kind}: "));
        if *kind == FailureKind::DeviceLost {
            rest = body;
            continue;
        }
        let body = if *kind == FailureKind::JobCancelled {
            body.strip_suffix("; job=LazyDeferredCoroutine{Cancelled}")
                .filter(|body| *body == "LazyDeferredCoroutine was cancelled")
                .unwrap_or(body)
        } else {
            body
        };
        return generated_message_for_log(kind, body).map(|message| format!("{prefix}{message}"));
    }
}

/// Only the actor's generated failure envelopes contain rendered traces; other reasons are opaque data.
pub fn batch_return_reason_for_log(reason: &str) -> String {
    for prefix in ["Test batch failed execution:\n", "Unknown vendor exception caught:\n"] {
        if let Some(trace) = reason.strip_prefix(prefix) {
            return format!("{prefix}{}", trace_for_log(trace));
        }
    }
    reason.to_owned()
}

impl Cancellation {
    /// The CancellationException Kotlin throws for this cancellation.
    pub fn failure(&self) -> &Failure {
        match self {
            Self::Requested(f) | Self::Timeout { detail: f, .. } => f,
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
        assert_eq!(failure.kind, FailureKind::IllegalState);
        assert_eq!(failure.to_string(), "invalid state: ");
        assert_eq!(
            failure.stack_trace(),
            "invalid state: \n\tat source.frame(Source:1)\nCaused by: I/O error\n"
        );
        assert_eq!(failure, failure.clone());
    }

    #[test]
    fn debug_uses_typed_fields_and_display_uses_the_description() {
        let mut failure = Failure::from_kind(FailureKind::IllegalState, Some("outer".into()));
        failure.cause = Some(Box::new(Failure::from_kind(FailureKind::Io, None)));
        assert_eq!(failure.to_string(), "invalid state: outer");
        assert_eq!(
            format!("{:?}", VendorError::Other(failure)),
            "Other(Failure { kind: IllegalState, message: Some(\"outer\"), cause: Some(Failure { kind: Io, message: None, cause: None, stacktrace: None }), stacktrace: None })"
        );
    }

    #[test]
    fn diagnostic_cancellation_is_plain_without_changing_stored_data() {
        let cause = Failure::from_kind(
            FailureKind::JobCancelled,
            Some("LazyDeferredCoroutine was cancelled".into()),
        );
        let mut loss = Failure::from_kind(
            FailureKind::DeviceLost,
            Some(format!("{cause}; job=LazyDeferredCoroutine{{Cancelled}}")),
        );
        loss.cause = Some(Box::new(cause.clone()));
        let original = loss.clone();
        assert_eq!(
            loss.for_log().stack_trace(),
            "device lost: job cancelled: task was cancelled\nCaused by: job cancelled: task was cancelled\n"
        );
        assert_eq!(loss, original);
        assert!(loss.stack_trace().contains("LazyDeferredCoroutine"));
        let mut ios_loss = loss.clone();
        ios_loss.message = Some(cause.to_string());
        assert_eq!(
            ios_loss.for_log().message.as_deref(),
            Some("job cancelled: task was cancelled")
        );
        assert_eq!(
            ios_loss.message.as_deref(),
            Some("job cancelled: LazyDeferredCoroutine was cancelled")
        );
        let deferred = Failure::from_kind(
            FailureKind::JobCancelled,
            Some("DeferredCoroutine was cancelled".into()),
        );
        assert_eq!(deferred.for_log().to_string(), "job cancelled: task was cancelled");
        // Class-looking user/tool text is opaque, even inside a cancellation.
        for kind in [FailureKind::Io, FailureKind::External("runner.CustomFailure".into())] {
            let external = Failure::from_kind(kind, cause.message.clone());
            assert_eq!(external.for_log(), external);
        }
        let external = Failure::from_kind(
            FailureKind::JobCancelled,
            Some("external LazyDeferredCoroutine was cancelled".into()),
        );
        assert_eq!(external.for_log(), external);
        let mut wrapper = loss.clone();
        wrapper.message = Some("external LazyDeferredCoroutine".into());
        assert_eq!(wrapper.for_log().message, wrapper.message);
    }

    #[test]
    fn diagnostic_projection_handles_suppressed_trace_fields_without_mutating_data() {
        let trace = "\tat external.Frame(Timeout expired: 30000 MILLISECONDS)\n\tSuppressed: job cancelled: LazyDeferredCoroutine was cancelled\n\tCaused by: timed out: Timeout expired: 30000 MILLISECONDS\n\t\tSuppressed: SSH connection failure: Timeout expired: 1 NANOSECONDS\nexternal LazyDeferredCoroutine was cancelled\n";
        let mut failure = Failure::from_kind(FailureKind::Io, Some("outer".into()));
        failure.stacktrace = Some(trace.into());
        let original = failure.clone();
        assert_eq!(
            failure.for_log().stacktrace.as_deref(),
            Some(
                "\tat external.Frame(Timeout expired: 30000 MILLISECONDS)\n\tSuppressed: job cancelled: task was cancelled\n\tCaused by: timed out: no response after 30 s\n\t\tSuppressed: SSH connection failure: no response after 1 ns\nexternal LazyDeferredCoroutine was cancelled\n"
            )
        );
        assert_eq!(failure, original);
        failure.kind = FailureKind::External("runner.CustomFailure".into());
        assert_eq!(failure.for_log(), failure);
        assert_eq!(batch_return_reason_for_log(trace), trace);
    }

    #[test]
    fn external_class_and_stack_text_remain_opaque() {
        // This models a class and trace supplied by an external runner.
        let kind = FailureKind::External("runner.CustomFailure".into());
        let mut failure = Failure::from_kind(kind.clone(), Some("java.io.IOException: remote".into()));
        failure.stacktrace = Some("external.Frame\n".into());
        assert_eq!(failure.kind, kind);
        assert_eq!(
            failure.stack_trace(),
            "runner.CustomFailure: java.io.IOException: remote\nexternal.Frame\n"
        );
    }
}
