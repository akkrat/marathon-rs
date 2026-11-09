//! Port of core/src/main/kotlin/com/malinskiy/marathon/exceptions/DeviceLostException.kt
//! and the shared exception classifications used by the vendor and output boundaries.

pub mod config;
pub use config::ConfigKind;
pub mod ssh;
pub use ssh::SshKind;
pub mod analytics;
pub use analytics::AnalyticsKind;
pub mod dex;
pub use dex::DexKind;
pub mod adb;
pub use adb::AdbKind;
pub mod android;
pub use android::AndroidKind;
pub mod apple;
pub use apple::AppleKind;
pub mod report;
pub use report::ReportKind;
pub mod fatal;
pub use fatal::FatalKind;
pub mod injected;
pub use analytics::{InfluxV1Kind, RetryDisposition};
pub use injected::InjectedKind;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FailureKind {
    Security,
    ReplayClock,
    DeviceLost,
    DeviceSetup,
    IncompatibleDevice,
    NoDevices,
    NoTests,
    BatchExecution,
    TestParsing,
    Transfer,
    Eof,
    FileNotFound,
    Io,
    InterruptedIo,
    UncheckedIo,
    Arithmetic,
    ArrayIndexOutOfBounds,
    ClassCast,
    Generic,
    IllegalArgument,
    IllegalState,
    IndexOutOfBounds,
    NegativeArraySize,
    NullReference,
    NumberFormat,
    Runtime,
    StringIndexOutOfBounds,
    UnsupportedOperation,
    Bind,
    Connect,
    NoRouteToHost,
    PortUnreachable,
    Protocol,
    Socket,
    SocketTimeout,
    UnknownHost,
    AccessDenied,
    FileAlreadyExists,
    FileSystem,
    InvalidPath,
    NoSuchFile,
    GeneralSecurity,
    DateTime,
    DateTimeParse,
    NoSuchElement,
    Cancelled,
    Timeout,
    PatternSyntax,
    Zip,
    UninitializedProperty,
    CopyDestinationExists,
    CopySourceMissing,
    JobCancelled,
    TimeoutCancelled,
    ChannelReceiveClosed,
    ChannelSendClosed,
    Replay,
    DependencyCreation,
    MissingDefinition,
    Config(ConfigKind),
    Ssh(SshKind),
    Analytics(AnalyticsKind),
    Dex(DexKind),
    Adb(AdbKind),
    Android(AndroidKind),
    Apple(AppleKind),
    Report(ReportKind),
    Fatal(FatalKind),
    Injected(InjectedKind),
    /// Verbatim class text separately supplied by an external tool; rendering only.
    /// It participates in no selector: no equality-based selection, analytics I/O
    /// wrapping (`wraps_as_analytics_io`), fatal catch (`is_fatal`) or deduplication class.
    External(String),
}

impl FailureKind {
    pub const fn description(&self) -> &str {
        match self {
            Self::Security => "security restriction",
            Self::ReplayClock => "clock replay failure",
            Self::DeviceLost => "device lost",
            Self::DeviceSetup => "device setup failure",
            Self::IncompatibleDevice => "incompatible device",
            Self::NoDevices => "no devices available",
            Self::NoTests => "no tests found",
            Self::BatchExecution => "test batch execution failure",
            Self::TestParsing => "test discovery failure",
            Self::Transfer => "file transfer failure",
            Self::Eof => "unexpected end of input",
            Self::FileNotFound => "file not found",
            Self::Io => "I/O error",
            Self::InterruptedIo => "interrupted I/O",
            Self::UncheckedIo => "unchecked I/O error",
            Self::Arithmetic => "arithmetic failure",
            Self::ArrayIndexOutOfBounds => "array index out of bounds",
            Self::ClassCast => "incompatible value type",
            Self::Generic => "failure",
            Self::IllegalArgument => "invalid argument",
            Self::IllegalState => "invalid state",
            Self::IndexOutOfBounds => "index out of bounds",
            Self::NegativeArraySize => "negative array size",
            Self::NullReference => "missing value",
            Self::NumberFormat => "invalid number",
            Self::Runtime => "runtime failure",
            Self::StringIndexOutOfBounds => "string index out of bounds",
            Self::UnsupportedOperation => "unsupported operation",
            Self::Bind => "socket bind failure",
            Self::Connect => "connection failure",
            Self::NoRouteToHost => "no route to host",
            Self::PortUnreachable => "unreachable port",
            Self::Protocol => "protocol failure",
            Self::Socket => "socket failure",
            Self::SocketTimeout => "socket timed out",
            Self::UnknownHost => "unknown host",
            Self::AccessDenied => "access denied",
            Self::FileAlreadyExists => "file already exists",
            Self::FileSystem => "filesystem failure",
            Self::InvalidPath => "invalid path",
            Self::NoSuchFile => "no such file or directory",
            Self::GeneralSecurity => "security operation failure",
            Self::DateTime => "invalid date or time",
            Self::DateTimeParse => "date or time parsing failure",
            Self::NoSuchElement => "missing element",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timed out",
            Self::PatternSyntax => "invalid regular expression",
            Self::Zip => "archive failure",
            Self::UninitializedProperty => "uninitialized property",
            Self::CopyDestinationExists => "copy destination already exists",
            Self::CopySourceMissing => "copy source missing",
            Self::JobCancelled => "job cancelled",
            Self::TimeoutCancelled => "operation cancelled after timeout",
            Self::ChannelReceiveClosed => "receive channel closed",
            Self::ChannelSendClosed => "send channel closed",
            Self::Replay => "replay failure",
            Self::DependencyCreation => "dependency creation failure",
            Self::MissingDefinition => "missing dependency definition",
            Self::Config(kind) => kind.description(),
            Self::Ssh(kind) => kind.description(),
            Self::Analytics(kind) => kind.description(),
            Self::Dex(kind) => kind.description(),
            Self::Adb(kind) => kind.description(),
            Self::Android(kind) => kind.description(),
            Self::Apple(kind) => kind.description(),
            Self::Report(kind) => kind.description(),
            Self::Fatal(kind) => kind.description(),
            Self::Injected(kind) => kind.description(),
            Self::External(class) => class.as_str(),
        }
    }

    /// Stable machine identifier, independent of user-facing wording.
    pub fn identifier(&self) -> std::borrow::Cow<'static, str> {
        match self {
            Self::Security => "security",
            Self::ReplayClock => "replay-clock",
            Self::DeviceLost => "device-lost",
            Self::DeviceSetup => "device-setup",
            Self::IncompatibleDevice => "incompatible-device",
            Self::NoDevices => "no-devices",
            Self::NoTests => "no-tests",
            Self::BatchExecution => "batch-execution",
            Self::TestParsing => "test-parsing",
            Self::Transfer => "transfer",
            Self::Eof => "eof",
            Self::FileNotFound => "file-not-found",
            Self::Io => "io",
            Self::InterruptedIo => "interrupted-io",
            Self::UncheckedIo => "unchecked-io",
            Self::Arithmetic => "arithmetic",
            Self::ArrayIndexOutOfBounds => "array-index-out-of-bounds",
            Self::ClassCast => "class-cast",
            Self::Generic => "generic",
            Self::IllegalArgument => "illegal-argument",
            Self::IllegalState => "illegal-state",
            Self::IndexOutOfBounds => "index-out-of-bounds",
            Self::NegativeArraySize => "negative-array-size",
            Self::NullReference => "null-reference",
            Self::NumberFormat => "number-format",
            Self::Runtime => "runtime",
            Self::StringIndexOutOfBounds => "string-index-out-of-bounds",
            Self::UnsupportedOperation => "unsupported-operation",
            Self::Bind => "bind",
            Self::Connect => "connect",
            Self::NoRouteToHost => "no-route-to-host",
            Self::PortUnreachable => "port-unreachable",
            Self::Protocol => "protocol",
            Self::Socket => "socket",
            Self::SocketTimeout => "socket-timeout",
            Self::UnknownHost => "unknown-host",
            Self::AccessDenied => "access-denied",
            Self::FileAlreadyExists => "file-already-exists",
            Self::FileSystem => "file-system",
            Self::InvalidPath => "invalid-path",
            Self::NoSuchFile => "no-such-file",
            Self::GeneralSecurity => "general-security",
            Self::DateTime => "date-time",
            Self::DateTimeParse => "date-time-parse",
            Self::NoSuchElement => "no-such-element",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::PatternSyntax => "pattern-syntax",
            Self::Zip => "zip",
            Self::UninitializedProperty => "uninitialized-property",
            Self::CopyDestinationExists => "copy-destination-exists",
            Self::CopySourceMissing => "copy-source-missing",
            Self::JobCancelled => "job-cancelled",
            Self::TimeoutCancelled => "timeout-cancelled",
            Self::ChannelReceiveClosed => "channel-receive-closed",
            Self::ChannelSendClosed => "channel-send-closed",
            Self::Replay => "replay",
            Self::DependencyCreation => "dependency-creation",
            Self::MissingDefinition => "missing-definition",
            Self::Config(kind) => kind.identifier(),
            Self::Ssh(kind) => kind.identifier(),
            Self::Analytics(kind) => kind.identifier(),
            Self::Dex(kind) => kind.identifier(),
            Self::Adb(kind) => kind.identifier(),
            Self::Android(kind) => kind.identifier(),
            Self::Apple(kind) => kind.identifier(),
            Self::Report(kind) => kind.identifier(),
            Self::Fatal(kind) => kind.identifier(),
            Self::Injected(kind) => kind.identifier(),
            Self::External(class) => return format!("external:{class}").into(),
        }
        .into()
    }
}

impl std::fmt::Display for FailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.description())
    }
}

impl FailureKind {
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Fatal(_))
    }

    /// Retry metadata never participated in the old class/message deduplication key.
    pub fn dedup_key(&self) -> Self {
        match self {
            Self::Analytics(AnalyticsKind::InfluxV1 { problem, .. }) => Self::Analytics(AnalyticsKind::InfluxV1 {
                problem: *problem,
                retry: RetryDisposition::Retry,
            }),
            other => other.clone(),
        }
    }
}

impl FailureKind {
    pub fn wraps_as_analytics_io(&self) -> bool {
        matches!(
            self,
            Self::Io
                | Self::Eof
                | Self::FileNotFound
                | Self::InterruptedIo
                | Self::UncheckedIo
                | Self::Connect
                | Self::Socket
                | Self::SocketTimeout
                | Self::UnknownHost
                | Self::Bind
                | Self::NoRouteToHost
                | Self::PortUnreachable
                | Self::Protocol
                | Self::Fatal(FatalKind::Io)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fatal_catches_keep_the_known_hierarchy_and_do_not_guess_from_suffixes() {
        for kind in [
            FatalKind::WindowSystem,
            FatalKind::Io,
            FatalKind::Assertion,
            FatalKind::BootstrapMethod,
            FatalKind::ClassCircularity,
            FatalKind::ClassFormat,
            FatalKind::General,
            FatalKind::Initialization,
            FatalKind::IllegalAccess,
            FatalKind::IncompatibleClassChange,
            FatalKind::Instantiation,
            FatalKind::Internal,
            FatalKind::Linkage,
            FatalKind::NoClassDefFound,
            FatalKind::NoSuchField,
            FatalKind::NoSuchMethod,
            FatalKind::OutOfMemory,
            FatalKind::StackOverflow,
            FatalKind::ThreadDeath,
            FatalKind::Unknown,
            FatalKind::UnsatisfiedLink,
            FatalKind::UnsupportedClassVersion,
            FatalKind::Verify,
            FatalKind::VirtualMachine,
            FatalKind::AnnotationFormat,
            FatalKind::CoderMalfunction,
            FatalKind::ServiceConfiguration,
            FatalKind::Zip,
            FatalKind::ParserFactory,
            FatalKind::StreamFactory,
            FatalKind::TransformerFactory,
            FatalKind::NotImplemented,
            FatalKind::ReflectionUnsupported,
        ] {
            assert!(FailureKind::Fatal(kind).is_fatal());
        }
        for kind in [
            FailureKind::Io,
            FailureKind::Generic,
            FailureKind::Runtime,
            FailureKind::Injected(InjectedKind::ProbeAssertionError),
            FailureKind::Injected(InjectedKind::ProbeExceptionNamedError),
        ] {
            assert!(!kind.is_fatal());
        }
    }
}
