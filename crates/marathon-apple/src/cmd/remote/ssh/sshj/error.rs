//! Port of the sshj 0.39.0 exception shapes that SshjCommandExecutor.kt, CommandExecutor.kt (`safeExecute`)
//! and device/ConnectionFailures.kt (4.3) observe: `net/schmizz/sshj/common/SSHException.java`, `DisconnectReason.java`,
//! `connection/channel/OpenFailException.java` and the JDK classes they wrap.
//!
//! Disconnect reasons and suppressed failures stay in `SshFailure`; shared kinds retain catch
//! membership when execute failures cross into core `Failure`.

use crate::cmd::failure;
use marathon_core::error::{Failure, VendorError};
use marathon_error::{FailureKind, SshKind};

pub const IO_EXCEPTION: FailureKind = FailureKind::Io;
pub const SSH_EXCEPTION: FailureKind = FailureKind::Ssh(SshKind::General);
pub const CONNECTION_EXCEPTION: FailureKind = FailureKind::Ssh(SshKind::Connection);
pub const CIRCULAR_BUFFER_EXCEPTION: FailureKind = FailureKind::Ssh(SshKind::CircularBuffer);
pub const OPEN_FAIL_EXCEPTION: FailureKind = FailureKind::Ssh(SshKind::OpenChannel);
pub const TRANSPORT_EXCEPTION: FailureKind = FailureKind::Ssh(SshKind::Transport);
pub const USER_AUTH_EXCEPTION: FailureKind = FailureKind::Ssh(SshKind::Authentication);
pub const KEY_DECRYPTION_FAILED_EXCEPTION: FailureKind = FailureKind::Ssh(SshKind::KeyDecryptionFailed);
pub const PEM_EXCEPTION: FailureKind = FailureKind::Ssh(SshKind::Pem);
pub const ENCRYPTION_EXCEPTION: FailureKind = FailureKind::Ssh(SshKind::Encryption);
pub const PASSWORD_EXCEPTION: FailureKind = FailureKind::Ssh(SshKind::Password);
pub const PKCS_EXCEPTION: FailureKind = FailureKind::Ssh(SshKind::Pkcs);
pub const ILLEGAL_ARGUMENT_EXCEPTION: FailureKind = FailureKind::IllegalArgument;
pub const TIMEOUT_EXCEPTION: FailureKind = FailureKind::Timeout;
pub const ILLEGAL_STATE_EXCEPTION: FailureKind = FailureKind::IllegalState;
pub const CONNECT_EXCEPTION: FailureKind = FailureKind::Connect;
pub const SOCKET_EXCEPTION: FailureKind = FailureKind::Socket;
pub const SOCKET_TIMEOUT_EXCEPTION: FailureKind = FailureKind::SocketTimeout;
pub const UNKNOWN_HOST_EXCEPTION: FailureKind = FailureKind::UnknownHost;
pub const FILE_NOT_FOUND_EXCEPTION: FailureKind = FailureKind::FileNotFound;
pub const PATTERN_SYNTAX_EXCEPTION: FailureKind = FailureKind::PatternSyntax;
const NOT_CONNECTED: &str = "Not connected";

/// Shared typed catch membership, also used after projection into core failures.
pub fn is_instance(kind: &FailureKind, of: FailureKind) -> bool {
    kind.is_ssh_instance(&of)
}

/// `net.schmizz.sshj.common.DisconnectReason`; the ordinal is the wire code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DisconnectReason {
    #[default]
    Unknown,
    HostNotAllowedToConnect,
    ProtocolError,
    KeyExchangeFailed,
    Reserved,
    MacError,
    CompressionError,
    ServiceNotAvailable,
    ProtocolVersionNotSupported,
    HostKeyNotVerifiable,
    ConnectionLost,
    ByApplication,
    TooManyConnections,
    AuthCancelledByUser,
    NoMoreAuthMethodsAvailable,
    IllegalUserName,
}
impl DisconnectReason {
    const ALL: [Self; 16] = [
        Self::Unknown,
        Self::HostNotAllowedToConnect,
        Self::ProtocolError,
        Self::KeyExchangeFailed,
        Self::Reserved,
        Self::MacError,
        Self::CompressionError,
        Self::ServiceNotAvailable,
        Self::ProtocolVersionNotSupported,
        Self::HostKeyNotVerifiable,
        Self::ConnectionLost,
        Self::ByApplication,
        Self::TooManyConnections,
        Self::AuthCancelledByUser,
        Self::NoMoreAuthMethodsAvailable,
        Self::IllegalUserName,
    ];
    /// `fromInt`; its off-by-one (`code > len`) only matters for the exotic wire code 16.
    pub fn from_int(code: u32) -> Self {
        Self::ALL.get(code as usize).copied().unwrap_or_default()
    }
    pub fn to_int(self) -> u32 {
        self as u32
    }
    pub fn name(self) -> &'static str {
        [
            "UNKNOWN",
            "HOST_NOT_ALLOWED_TO_CONNECT",
            "PROTOCOL_ERROR",
            "KEY_EXCHANGE_FAILED",
            "RESERVED",
            "MAC_ERROR",
            "COMPRESSION_ERROR",
            "SERVICE_NOT_AVAILABLE",
            "PROTOCOL_VERSION_NOT_SUPPORTED",
            "HOST_KEY_NOT_VERIFIABLE",
            "CONNECTION_LOST",
            "BY_APPLICATION",
            "TOO_MANY_CONNECTIONS",
            "AUTH_CANCELLED_BY_USER",
            "NO_MORE_AUTH_METHODS_AVAILABLE",
            "ILLEGAL_USER_NAME",
        ][self as usize]
    }
}

/// A Java throwable as sshj builds it: class, disconnect reason (SSHException family only), the message passed
/// to the constructor, cause and suppressed failures (`closeOnFailure`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshFailure {
    pub kind: FailureKind,
    pub reason: DisconnectReason,
    message: Option<String>,
    pub cause: Option<Box<SshFailure>>,
    pub suppressed: Vec<SshFailure>,
    no_common_algorithm: bool,
}
impl SshFailure {
    pub fn new(class: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind: class,
            reason: DisconnectReason::Unknown,
            message: Some(message.into()),
            cause: None,
            suppressed: Vec::new(),
            no_common_algorithm: false,
        }
    }
    pub fn with_reason(mut self, reason: DisconnectReason) -> Self {
        self.reason = reason;
        self
    }
    pub fn caused_by(mut self, cause: SshFailure) -> Self {
        self.cause = Some(Box::new(cause));
        self
    }
    /// `new <SSHException subclass>(cause)`: no message of its own, UNKNOWN reason.
    pub fn wrapping(class: FailureKind, cause: SshFailure) -> Self {
        Self {
            kind: class,
            reason: DisconnectReason::Unknown,
            message: None,
            cause: Some(Box::new(cause)),
            suppressed: Vec::new(),
            no_common_algorithm: false,
        }
    }
    pub fn transport(reason: DisconnectReason, message: impl Into<String>) -> Self {
        Self::new(TRANSPORT_EXCEPTION, message).with_reason(reason)
    }
    /// A negotiation that found no common algorithm. D35: sshj's class, UNKNOWN reason and text stay as they are,
    /// because they reach logs, and UNKNOWN also covers real network faults. Only the classifier reads the marker.
    pub fn no_common_algorithm(message: impl Into<String>) -> Self {
        Self {
            no_common_algorithm: true,
            ..Self::transport(DisconnectReason::Unknown, message)
        }
    }
    pub fn is_no_common_algorithm(&self) -> bool {
        self.no_common_algorithm
    }
    /// D36: a command needs a transport that died, `cause` being why. Kotlin's `IllegalStateException` escaped
    /// `safeExecute` (A17); this is a `ConnectionException`, which keeps the text and adds the reason.
    pub fn connection_gone(cause: &SshFailure) -> Self {
        Self::connection(DisconnectReason::Unknown, NOT_CONNECTED).caused_by(cause.clone())
    }
    pub fn connection(reason: DisconnectReason, message: impl Into<String>) -> Self {
        Self::new(CONNECTION_EXCEPTION, message).with_reason(reason)
    }
    /// `ConnectionException.chainer.chain(t)`.
    pub fn chain_connection(cause: &SshFailure) -> Self {
        if is_instance(&cause.kind, CONNECTION_EXCEPTION) {
            cause.clone()
        } else {
            Self::wrapping(CONNECTION_EXCEPTION, cause.clone())
        }
    }
    /// `UserAuthException.chainer.chain(t)`.
    pub fn chain_user_auth(cause: &SshFailure) -> Self {
        if is_instance(&cause.kind, USER_AUTH_EXCEPTION) {
            cause.clone()
        } else {
            Self::wrapping(USER_AUTH_EXCEPTION, cause.clone())
        }
    }
    /// sshj's `Event.await(timeout)` expiry, chained by the event's chainer.
    pub fn timeout_expired(class: FailureKind, timeout: &str) -> Self {
        Self::wrapping(
            class,
            Self::new(TIMEOUT_EXCEPTION, format!("Timeout expired: {timeout}")),
        )
    }
    /// Use of a client after its own `close()`: the caller's mistake, not a connection fault.
    pub fn not_connected() -> Self {
        Self::new(ILLEGAL_STATE_EXCEPTION, NOT_CONNECTED)
    }
    pub fn is_ssh(&self) -> bool {
        is_instance(&self.kind, SSH_EXCEPTION)
    }
    pub fn is_io(&self) -> bool {
        is_instance(&self.kind, IO_EXCEPTION)
    }
    pub fn is_instance(&self, of: FailureKind) -> bool {
        is_instance(&self.kind, of)
    }
    /// `Throwable.getMessage()`; SSHException falls back to its cause's message.
    pub fn message(&self) -> Option<String> {
        if self.message.is_some() || !self.is_ssh() || self.kind == OPEN_FAIL_EXCEPTION {
            return self.message.to_owned();
        }
        self.cause.as_ref().and_then(|cause| cause.message())
    }
    /// Promise log rendering retains SSH reason codes and the channel-opening message.
    fn render(&self) -> String {
        let message = self.message();
        if self.kind == OPEN_FAIL_EXCEPTION {
            return format!(
                "Opening `session` channel failed: {}",
                message.unwrap_or_else(|| "null".into())
            );
        }
        if self.is_ssh() {
            let code = if self.reason == DisconnectReason::Unknown {
                String::new()
            } else {
                format!("[{}] ", self.reason.name())
            };
            let message = message.unwrap_or_default();
            let separator = if code.is_empty() && message.is_empty() {
                ""
            } else {
                ": "
            };
            return format!("{}{separator}{code}{message}", self.kind);
        }
        match message {
            Some(message) => format!("{}: {message}", self.kind),
            None => self.kind.description().to_owned(),
        }
    }
    /// Keep the typed kind, effective message and cause chain at the vendor boundary.
    pub fn to_failure(&self) -> Failure {
        let mut projected = Failure {
            message: self.message(),
            ..failure(self.kind.clone(), "")
        };
        projected.cause = self.cause.as_ref().map(|cause| Box::new(cause.to_failure()));
        projected
    }
    /// Keep SSH reason codes in diagnostics while projecting only generated failure wording.
    pub fn for_log(&self) -> Self {
        let mut diagnostic = self.clone();
        diagnostic.message = self.to_failure().for_log().message;
        diagnostic.cause = self.cause.as_deref().map(|cause| Box::new(cause.for_log()));
        diagnostic
    }
    pub fn into_vendor(self) -> VendorError {
        VendorError::Other(self.to_failure())
    }
}
impl std::fmt::Display for SshFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}
impl std::error::Error for SshFailure {}

/// A JDK socket/file failure: the OS message without Rust's " (os error N)" suffix.
pub fn io_failure(class: FailureKind, error: &std::io::Error) -> SshFailure {
    let text = error.to_string();
    let message = match text.rfind(" (os error ") {
        Some(index) if text.ends_with(')') => text[..index].to_owned(),
        _ => text,
    };
    SshFailure::new(class, message)
}
