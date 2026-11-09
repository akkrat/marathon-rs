//! Port of adam/src/main/kotlin/com/malinskiy/adam/exception/RequestRejectedException.kt

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AdbKind {
    PullFailed,
    PushFailed,
    RequestRejected,
    RequestValidation,
    UnsupportedForwardingSpec,
    UnsupportedImageProtocol,
    UnsupportedSyncProtocol,
    ConnectRefused,
    ConnectTimeout,
    ClosedChannel,
    TransportClosed,
}

impl AdbKind {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::PullFailed => "ADB pull failure",
            Self::PushFailed => "ADB push failure",
            Self::RequestRejected => "ADB request rejected",
            Self::RequestValidation => "invalid ADB request",
            Self::UnsupportedForwardingSpec => "unsupported ADB forwarding specification",
            Self::UnsupportedImageProtocol => "unsupported ADB image protocol",
            Self::UnsupportedSyncProtocol => "unsupported ADB sync protocol",
            Self::ConnectRefused => "ADB connection refused",
            Self::ConnectTimeout => "ADB connection timed out",
            Self::ClosedChannel => "ADB channel closed",
            Self::TransportClosed => "ADB transport closed",
        }
    }

    /// Stable machine identifier, independent of user-facing wording.
    pub const fn identifier(&self) -> &'static str {
        match self {
            Self::PullFailed => "adb.pull-failed",
            Self::PushFailed => "adb.push-failed",
            Self::RequestRejected => "adb.request-rejected",
            Self::RequestValidation => "adb.request-validation",
            Self::UnsupportedForwardingSpec => "adb.unsupported-forwarding-spec",
            Self::UnsupportedImageProtocol => "adb.unsupported-image-protocol",
            Self::UnsupportedSyncProtocol => "adb.unsupported-sync-protocol",
            Self::ConnectRefused => "adb.connect-refused",
            Self::ConnectTimeout => "adb.connect-timeout",
            Self::ClosedChannel => "adb.closed-channel",
            Self::TransportClosed => "adb.transport-closed",
        }
    }
}

impl std::fmt::Display for AdbKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.description())
    }
}
