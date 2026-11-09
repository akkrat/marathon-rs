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
    /// Only a class separately supplied by an external tool.
    External(String),
}

impl FailureKind {
    pub const fn legacy_class(&self) -> &str {
        match self {
            Self::Security => "java.lang.SecurityException",
            Self::ReplayClock => "ReplayError",
            Self::DeviceLost => "com.malinskiy.marathon.exceptions.DeviceLostException",
            Self::DeviceSetup => "com.malinskiy.marathon.exceptions.DeviceSetupException",
            Self::IncompatibleDevice => "com.malinskiy.marathon.exceptions.IncompatibleDeviceException",
            Self::NoDevices => "com.malinskiy.marathon.exceptions.NoDevicesException",
            Self::NoTests => "com.malinskiy.marathon.exceptions.NoTestCasesFoundException",
            Self::BatchExecution => "com.malinskiy.marathon.exceptions.TestBatchExecutionException",
            Self::TestParsing => "com.malinskiy.marathon.exceptions.TestParsingException",
            Self::Transfer => "com.malinskiy.marathon.exceptions.TransferException",
            Self::Eof => "java.io.EOFException",
            Self::FileNotFound => "java.io.FileNotFoundException",
            Self::Io => "java.io.IOException",
            Self::InterruptedIo => "java.io.InterruptedIOException",
            Self::UncheckedIo => "java.io.UncheckedIOException",
            Self::Arithmetic => "java.lang.ArithmeticException",
            Self::ArrayIndexOutOfBounds => "java.lang.ArrayIndexOutOfBoundsException",
            Self::ClassCast => "java.lang.ClassCastException",
            Self::Generic => "java.lang.Exception",
            Self::IllegalArgument => "java.lang.IllegalArgumentException",
            Self::IllegalState => "java.lang.IllegalStateException",
            Self::IndexOutOfBounds => "java.lang.IndexOutOfBoundsException",
            Self::NegativeArraySize => "java.lang.NegativeArraySizeException",
            Self::NullReference => "java.lang.NullPointerException",
            Self::NumberFormat => "java.lang.NumberFormatException",
            Self::Runtime => "java.lang.RuntimeException",
            Self::StringIndexOutOfBounds => "java.lang.StringIndexOutOfBoundsException",
            Self::UnsupportedOperation => "java.lang.UnsupportedOperationException",
            Self::Bind => "java.net.BindException",
            Self::Connect => "java.net.ConnectException",
            Self::NoRouteToHost => "java.net.NoRouteToHostException",
            Self::PortUnreachable => "java.net.PortUnreachableException",
            Self::Protocol => "java.net.ProtocolException",
            Self::Socket => "java.net.SocketException",
            Self::SocketTimeout => "java.net.SocketTimeoutException",
            Self::UnknownHost => "java.net.UnknownHostException",
            Self::AccessDenied => "java.nio.file.AccessDeniedException",
            Self::FileAlreadyExists => "java.nio.file.FileAlreadyExistsException",
            Self::FileSystem => "java.nio.file.FileSystemException",
            Self::InvalidPath => "java.nio.file.InvalidPathException",
            Self::NoSuchFile => "java.nio.file.NoSuchFileException",
            Self::GeneralSecurity => "java.security.GeneralSecurityException",
            Self::DateTime => "java.time.DateTimeException",
            Self::DateTimeParse => "java.time.format.DateTimeParseException",
            Self::NoSuchElement => "java.util.NoSuchElementException",
            Self::Cancelled => "java.util.concurrent.CancellationException",
            Self::Timeout => "java.util.concurrent.TimeoutException",
            Self::PatternSyntax => "java.util.regex.PatternSyntaxException",
            Self::Zip => "java.util.zip.ZipException",
            Self::UninitializedProperty => "kotlin.UninitializedPropertyAccessException",
            Self::CopyDestinationExists => "kotlin.io.FileAlreadyExistsException",
            Self::CopySourceMissing => "kotlin.io.NoSuchFileException",
            Self::JobCancelled => "kotlinx.coroutines.JobCancellationException",
            Self::TimeoutCancelled => "kotlinx.coroutines.TimeoutCancellationException",
            Self::ChannelReceiveClosed => "kotlinx.coroutines.channels.ClosedReceiveChannelException",
            Self::ChannelSendClosed => "kotlinx.coroutines.channels.ClosedSendChannelException",
            Self::Replay => "marathon.ReplayError",
            Self::DependencyCreation => "org.koin.core.error.InstanceCreationException",
            Self::MissingDefinition => "org.koin.core.error.NoBeanDefFoundException",
            Self::Config(kind) => kind.legacy_class(),
            Self::Ssh(kind) => kind.legacy_class(),
            Self::Analytics(kind) => kind.legacy_class(),
            Self::Dex(kind) => kind.legacy_class(),
            Self::Adb(kind) => kind.legacy_class(),
            Self::Android(kind) => kind.legacy_class(),
            Self::Apple(kind) => kind.legacy_class(),
            Self::Report(kind) => kind.legacy_class(),
            Self::Fatal(kind) => kind.legacy_class(),
            Self::Injected(kind) => kind.legacy_class(),
            Self::External(class) => class.as_str(),
        }
    }
}

impl std::fmt::Display for FailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.legacy_class())
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
