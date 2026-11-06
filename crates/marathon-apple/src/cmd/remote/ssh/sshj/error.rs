//! Port of the sshj 0.39.0 exception shapes that SshjCommandExecutor.kt, CommandExecutor.kt (`safeExecute`)
//! and device/ConnectionFailures.kt (4.3) observe: `net/schmizz/sshj/common/SSHException.java`, `DisconnectReason.java`,
//! `connection/channel/OpenFailException.java` and the JDK classes they wrap.
//!
//! `Failure` has no disconnect reason and no class hierarchy, so connect failures stay `SshFailure` for 4.3's
//! classifier. Execute failures cross the 4.1 boundary as `Failure`; `is_instance` keeps the hierarchy for them.

use crate::cmd::failure;
use marathon_core::error::{Failure, VendorError};

pub const IO_EXCEPTION: &str = "java.io.IOException";
pub const SSH_EXCEPTION: &str = "net.schmizz.sshj.common.SSHException";
pub const CONNECTION_EXCEPTION: &str = "net.schmizz.sshj.connection.ConnectionException";
pub const CIRCULAR_BUFFER_EXCEPTION: &str = "net.schmizz.sshj.common.CircularBuffer$CircularBufferException";
pub const OPEN_FAIL_EXCEPTION: &str = "net.schmizz.sshj.connection.channel.OpenFailException";
pub const TRANSPORT_EXCEPTION: &str = "net.schmizz.sshj.transport.TransportException";
pub const USER_AUTH_EXCEPTION: &str = "net.schmizz.sshj.userauth.UserAuthException";
pub const KEY_DECRYPTION_FAILED_EXCEPTION: &str = "com.hierynomus.sshj.common.KeyDecryptionFailedException";
pub const PEM_EXCEPTION: &str = "org.bouncycastle.openssl.PEMException";
pub const ENCRYPTION_EXCEPTION: &str = "org.bouncycastle.openssl.EncryptionException";
pub const PASSWORD_EXCEPTION: &str = "org.bouncycastle.openssl.PasswordException";
pub const PKCS_EXCEPTION: &str = "org.bouncycastle.pkcs.PKCSException";
pub const ILLEGAL_ARGUMENT_EXCEPTION: &str = "java.lang.IllegalArgumentException";
pub const TIMEOUT_EXCEPTION: &str = "java.util.concurrent.TimeoutException";
pub const ILLEGAL_STATE_EXCEPTION: &str = "java.lang.IllegalStateException";
pub const CONNECT_EXCEPTION: &str = "java.net.ConnectException";
pub const SOCKET_EXCEPTION: &str = "java.net.SocketException";
pub const SOCKET_TIMEOUT_EXCEPTION: &str = "java.net.SocketTimeoutException";
pub const UNKNOWN_HOST_EXCEPTION: &str = "java.net.UnknownHostException";
pub const FILE_NOT_FOUND_EXCEPTION: &str = "java.io.FileNotFoundException";
pub const PATTERN_SYNTAX_EXCEPTION: &str = "java.util.regex.PatternSyntaxException";
const NOT_CONNECTED: &str = "Not connected";

/// Direct superclasses of every class this module creates.
const SUPERCLASS: &[(&str, &str)] = &[
    (OPEN_FAIL_EXCEPTION, CONNECTION_EXCEPTION),
    (CONNECTION_EXCEPTION, SSH_EXCEPTION),
    (CIRCULAR_BUFFER_EXCEPTION, SSH_EXCEPTION),
    (TRANSPORT_EXCEPTION, SSH_EXCEPTION),
    (USER_AUTH_EXCEPTION, SSH_EXCEPTION),
    (SSH_EXCEPTION, IO_EXCEPTION),
    (KEY_DECRYPTION_FAILED_EXCEPTION, IO_EXCEPTION),
    (ENCRYPTION_EXCEPTION, PEM_EXCEPTION),
    (PASSWORD_EXCEPTION, PEM_EXCEPTION),
    (PEM_EXCEPTION, IO_EXCEPTION),
    (PKCS_EXCEPTION, "java.lang.Exception"),
    (PATTERN_SYNTAX_EXCEPTION, ILLEGAL_ARGUMENT_EXCEPTION),
    (ILLEGAL_ARGUMENT_EXCEPTION, "java.lang.RuntimeException"),
    (CONNECT_EXCEPTION, SOCKET_EXCEPTION),
    ("java.net.NoRouteToHostException", SOCKET_EXCEPTION),
    ("java.net.BindException", SOCKET_EXCEPTION),
    ("java.net.PortUnreachableException", SOCKET_EXCEPTION),
    (SOCKET_EXCEPTION, IO_EXCEPTION),
    (SOCKET_TIMEOUT_EXCEPTION, "java.io.InterruptedIOException"),
    ("java.io.InterruptedIOException", IO_EXCEPTION),
    (UNKNOWN_HOST_EXCEPTION, IO_EXCEPTION),
    (FILE_NOT_FOUND_EXCEPTION, IO_EXCEPTION),
    (IO_EXCEPTION, "java.lang.Exception"),
    (TIMEOUT_EXCEPTION, "java.lang.Exception"),
    (ILLEGAL_STATE_EXCEPTION, "java.lang.RuntimeException"),
    ("java.lang.RuntimeException", "java.lang.Exception"),
];

/// `class instanceof of`, over the classes this executor produces.
pub fn is_instance(class: &str, of: &str) -> bool {
    let mut current = class;
    loop {
        if current == of {
            return true;
        }
        match SUPERCLASS.iter().find(|(sub, _)| *sub == current) {
            Some((_, parent)) => current = parent,
            None => return false,
        }
    }
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
    pub class: &'static str,
    pub reason: DisconnectReason,
    message: Option<String>,
    pub cause: Option<Box<SshFailure>>,
    pub suppressed: Vec<SshFailure>,
    no_common_algorithm: bool,
}
impl SshFailure {
    pub fn new(class: &'static str, message: impl Into<String>) -> Self {
        Self {
            class,
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
    pub fn wrapping(class: &'static str, cause: SshFailure) -> Self {
        Self {
            class,
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
        if is_instance(cause.class, CONNECTION_EXCEPTION) {
            cause.clone()
        } else {
            Self::wrapping(CONNECTION_EXCEPTION, cause.clone())
        }
    }
    /// `UserAuthException.chainer.chain(t)`.
    pub fn chain_user_auth(cause: &SshFailure) -> Self {
        if is_instance(cause.class, USER_AUTH_EXCEPTION) {
            cause.clone()
        } else {
            Self::wrapping(USER_AUTH_EXCEPTION, cause.clone())
        }
    }
    /// sshj's `Event.await(timeout)` expiry, chained by the event's chainer.
    pub fn timeout_expired(class: &'static str, timeout: &str) -> Self {
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
        is_instance(self.class, SSH_EXCEPTION)
    }
    pub fn is_io(&self) -> bool {
        is_instance(self.class, IO_EXCEPTION)
    }
    pub fn is_instance(&self, of: &str) -> bool {
        is_instance(self.class, of)
    }
    /// `Throwable.getMessage()`; SSHException falls back to its cause's message.
    pub fn message(&self) -> Option<String> {
        if self.message.is_some() || !self.is_ssh() || self.class == OPEN_FAIL_EXCEPTION {
            return self.message.clone();
        }
        self.cause.as_ref().and_then(|cause| cause.message())
    }
    /// `Throwable.toString()`, as sshj's `Promise` "woke to" log lines print it.
    pub fn java_to_string(&self) -> String {
        let message = self.message();
        if self.class == OPEN_FAIL_EXCEPTION {
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
            return format!("{}{separator}{code}{message}", self.class);
        }
        match message {
            Some(message) => format!("{}: {message}", self.class),
            None => self.class.to_owned(),
        }
    }
    /// The 4.1/0.6 boundary keeps class, `getMessage()` and the cause chain; logback prints a throwable the same way.
    pub fn to_failure(&self) -> Failure {
        let mut projected = Failure {
            message: self.message().map(Into::into),
            ..failure(self.class, "")
        };
        projected.cause = self.cause.as_ref().map(|cause| Box::new(cause.to_failure()));
        projected
    }
    pub fn into_vendor(self) -> VendorError {
        VendorError::Other(self.to_failure())
    }
}
impl std::fmt::Display for SshFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.java_to_string())
    }
}
impl std::error::Error for SshFailure {}

/// A JDK socket/file failure: the OS message without Rust's " (os error N)" suffix.
pub fn io_failure(class: &'static str, error: &std::io::Error) -> SshFailure {
    let text = error.to_string();
    let message = match text.rfind(" (os error ") {
        Some(index) if text.ends_with(')') => text[..index].to_owned(),
        _ => text,
    };
    SshFailure::new(class, message)
}
