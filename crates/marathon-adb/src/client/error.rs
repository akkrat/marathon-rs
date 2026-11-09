//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/exception/RequestRejectedException.kt` and
//! `.../exception/RequestValidationException.kt`, plus the JVM exceptions of adam's vert.x transport that
//! marathon branches on (`AdamDeviceProvider.kt:75,85`, `AdamAndroidDevice.kt:480-481`).
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.

use marathon_error::{AdbKind, FailureKind};
use std::fmt;

/// What a request or stream fails with. Each variant stands for the Java class it is named after; see
/// [`AdbError::to_failure`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdbError {
    PullFailed(String),
    PushFailed(String),
    UnsupportedSyncProtocol(String),
    UnsupportedImageProtocol(String),
    UnsupportedForwardingSpec(String),
    Runtime(String),
    NumberFormat(String),
    StringIndexOutOfBounds(String),
    UnsupportedOperation(String),
    /// `java.net.ConnectException`: the connection was refused or the connect timeout expired.
    Connect(ConnectError),
    /// `com.malinskiy.adam.exception.RequestRejectedException`.
    RequestRejected(String),
    /// `com.malinskiy.adam.exception.RequestValidationException`.
    RequestValidation(String),
    /// `java.lang.IllegalStateException`: a write on a closed socket, or `Unknown deployment` when a
    /// request or stream that was open at [`AndroidDebugBridgeClient::close`](super::AndroidDebugBridgeClient::close)
    /// otherwise completes (adam's socket close fails once vert.x is gone).
    IllegalState(String),
    /// `io.vertx.core.impl.NoStackTraceThrowable: Vert.x closed`: a request made after `close()`. It is a
    /// `Throwable`, not an `Exception`, so Kotlin's `catch (e: Exception)` lets it through.
    VertxClosed,
    /// `io.netty.channel.StacklessClosedChannelException`, with no message: the idle timer closed the
    /// connection under a write the server did not take in (JVM probe `idle-blocked-write`).
    ClosedChannel,
    /// A `devices` or `track-devices` line without a tab (WP 6.1 P6). Kotlin fails on `split[1]` with
    /// `IndexOutOfBoundsException`; the message is that exception's (JVM probe `devices-line-without-tab`).
    MalformedResponse(String),
    /// Any other `java.io.IOException`, such as a failed write or an unreachable host.
    Io(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectError {
    pub timed_out: bool,
    pub message: String,
}

/// The `Failure`-shaped view of an [`AdbError`]: the kind, message and cause that the Kotlin
/// exception would carry. 6.5 turns it into the core's `Failure` without this crate depending on core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdbFailure {
    pub kind: FailureKind,
    pub message: Option<String>,
    pub cause: Option<Box<AdbFailure>>,
}

impl AdbError {
    /// Shared kinds preserve the catch distinctions without depending on core.
    pub fn failure_kind(&self) -> FailureKind {
        match self {
            AdbError::Connect(ConnectError { timed_out: false, .. }) => FailureKind::Adb(AdbKind::ConnectRefused),
            AdbError::Connect(ConnectError { timed_out: true, .. }) => FailureKind::Adb(AdbKind::ConnectTimeout),
            AdbError::RequestRejected(_) => FailureKind::Adb(AdbKind::RequestRejected),
            AdbError::RequestValidation(_) => FailureKind::Adb(AdbKind::RequestValidation),
            AdbError::IllegalState(_) => FailureKind::IllegalState,
            AdbError::VertxClosed => FailureKind::Adb(AdbKind::TransportClosed),
            AdbError::ClosedChannel => FailureKind::Adb(AdbKind::ClosedChannel),
            AdbError::MalformedResponse(_) => FailureKind::IndexOutOfBounds,
            AdbError::Io(_) => FailureKind::Io,
            AdbError::PullFailed(_) => FailureKind::Adb(AdbKind::PullFailed),
            AdbError::PushFailed(_) => FailureKind::Adb(AdbKind::PushFailed),
            AdbError::UnsupportedSyncProtocol(_) => FailureKind::Adb(AdbKind::UnsupportedSyncProtocol),
            AdbError::UnsupportedImageProtocol(_) => FailureKind::Adb(AdbKind::UnsupportedImageProtocol),
            AdbError::UnsupportedForwardingSpec(_) => FailureKind::Adb(AdbKind::UnsupportedForwardingSpec),
            AdbError::StringIndexOutOfBounds(_) => FailureKind::StringIndexOutOfBounds,
            AdbError::NumberFormat(_) => FailureKind::NumberFormat,
            AdbError::Runtime(_) => FailureKind::Runtime,
            AdbError::UnsupportedOperation(_) => FailureKind::UnsupportedOperation,
        }
    }

    pub fn description(&self) -> String {
        self.failure_kind().description().to_owned()
    }
    /// `getMessage()`.
    pub fn message(&self) -> Option<&str> {
        match self {
            AdbError::Connect(c) => Some(&c.message),
            AdbError::VertxClosed => Some("Vert.x closed"),
            AdbError::ClosedChannel => None,
            AdbError::RequestRejected(m)
            | AdbError::RequestValidation(m)
            | AdbError::IllegalState(m)
            | AdbError::MalformedResponse(m)
            | AdbError::Io(m)
            | AdbError::PullFailed(m)
            | AdbError::PushFailed(m)
            | AdbError::UnsupportedSyncProtocol(m)
            | AdbError::UnsupportedImageProtocol(m)
            | AdbError::UnsupportedForwardingSpec(m)
            | AdbError::StringIndexOutOfBounds(m)
            | AdbError::NumberFormat(m)
            | AdbError::Runtime(m)
            | AdbError::UnsupportedOperation(m) => Some(m),
        }
    }

    /// `catch (e: ConnectException)`: both netty connect failures are subclasses.
    pub fn is_connect_exception(&self) -> bool {
        matches!(self, AdbError::Connect(_))
    }

    /// `catch (e: Exception)`: everything but [`AdbError::VertxClosed`].
    pub fn is_exception(&self) -> bool {
        !matches!(self, AdbError::VertxClosed)
    }

    /// A refusal keeps the `ConnectException` netty annotates (`AbstractChannel.java:1172-1174`
    /// `initCause`); its message is the JDK's, the same on macOS and Linux (JVM probe
    /// `connect-refused-cause`). The other errors have no cause.
    pub fn to_failure(&self) -> AdbFailure {
        let cause = match self {
            AdbError::Connect(ConnectError { timed_out: false, .. }) => Some(Box::new(AdbFailure {
                kind: FailureKind::Connect,
                message: Some("Connection refused".into()),
                cause: None,
            })),
            _ => None,
        };
        AdbFailure {
            kind: self.failure_kind(),
            message: self.message().map(str::to_string),
            cause,
        }
    }
}

impl fmt::Display for AdbError {
    /// Plain description and optional message; absent and empty messages stay distinct.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.message() {
            Some(message) => write!(f, "{}: {}", self.description(), message),
            None => f.write_str(&self.description()),
        }
    }
}

impl std::error::Error for AdbError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_and_messages() {
        let e = AdbError::RequestRejected(String::new());
        assert_eq!(e.to_string(), "ADB request rejected: ");
        assert!(!e.is_connect_exception());
        let c = AdbError::Connect(ConnectError {
            timed_out: true,
            message: "x".into(),
        });
        assert!(c.is_connect_exception());
        assert_eq!(
            c.to_failure(),
            AdbFailure {
                kind: FailureKind::Adb(AdbKind::ConnectTimeout),
                message: Some("x".into()),
                cause: None
            }
        );
        assert_eq!(AdbError::ClosedChannel.to_string(), "ADB channel closed");
        assert_eq!(AdbError::ClosedChannel.to_failure().message, None);
    }
}

#[cfg(test)]
mod kind_tests {
    use super::*;
    #[test]
    fn typed_adb_failures_keep_refusal_causes_and_throwable_distinctions() {
        let refused = AdbError::Connect(ConnectError {
            timed_out: false,
            message: "refused".into(),
        });
        let detail = refused.to_failure();
        assert_eq!(detail.kind, FailureKind::Adb(AdbKind::ConnectRefused));
        assert_eq!(detail.cause.as_deref().map(|c| &c.kind), Some(&FailureKind::Connect));
        assert_eq!(
            detail.cause.as_deref().and_then(|c| c.message.as_deref()),
            Some("Connection refused")
        );
        let timeout = AdbError::Connect(ConnectError {
            timed_out: true,
            message: "timeout".into(),
        });
        assert_eq!(timeout.to_failure().kind, FailureKind::Adb(AdbKind::ConnectTimeout));
        assert!(timeout.to_failure().cause.is_none());
        assert!(refused.is_connect_exception() && timeout.is_connect_exception());
        assert!(!AdbError::VertxClosed.is_exception());
        assert_eq!(
            AdbError::VertxClosed.to_failure().kind,
            FailureKind::Adb(AdbKind::TransportClosed)
        );
        assert!(AdbError::ClosedChannel.is_exception());
        assert_eq!(
            AdbError::ClosedChannel.to_failure().kind,
            FailureKind::Adb(AdbKind::ClosedChannel)
        );
        assert_eq!(AdbError::ClosedChannel.to_failure().message, None);
    }
}
