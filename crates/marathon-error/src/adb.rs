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
    pub const fn legacy_class(&self) -> &'static str {
        match self {
            Self::PullFailed => "com.malinskiy.adam.exception.PullFailedException",
            Self::PushFailed => "com.malinskiy.adam.exception.PushFailedException",
            Self::RequestRejected => "com.malinskiy.adam.exception.RequestRejectedException",
            Self::RequestValidation => "com.malinskiy.adam.exception.RequestValidationException",
            Self::UnsupportedForwardingSpec => "com.malinskiy.adam.exception.UnsupportedForwardingSpecException",
            Self::UnsupportedImageProtocol => "com.malinskiy.adam.exception.UnsupportedImageProtocolException",
            Self::UnsupportedSyncProtocol => "com.malinskiy.adam.exception.UnsupportedSyncProtocolException",
            Self::ConnectRefused => "io.netty.channel.AbstractChannel$AnnotatedConnectException",
            Self::ConnectTimeout => "io.netty.channel.ConnectTimeoutException",
            Self::ClosedChannel => "io.netty.channel.StacklessClosedChannelException",
            Self::TransportClosed => "io.vertx.core.impl.NoStackTraceThrowable",
        }
    }
}
