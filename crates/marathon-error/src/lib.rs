//! Port of core/src/main/kotlin/com/malinskiy/marathon/exceptions/DeviceLostException.kt
//! and the exception kinds used by core's output and executor boundaries.

/// Shared without depending on core: ADB, DEX and config also produce failures.
/// Domain subkinds will be added here as their producers are migrated (WP 8.6a).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FailureKind {
    Io,
    FileNotFound,
    NoSuchFile,
    AccessDenied,
    FileSystem,
    FileAlreadyExists,
    InvalidPath,
    IllegalArgument,
    IllegalState,
    Runtime,
    Arithmetic,
    ClassCast,
    NumberFormat,
    NullReference,
    Cancelled,
    Timeout,
    TimeoutCancelled,
    DeviceLost,
    DeviceSetup,
    IncompatibleDevice,
    BatchExecution,
    TestParsing,
    NoDevices,
    NoTests,
    Transfer,
    Replay,
    ReplayClock,
    /// Only a class supplied by an external tool, never an unknown internal kind.
    External(String),
}

impl FailureKind {
    /// WP 8.6a preserves the exact rendered name; WP 8.6b replaces this mapping.
    pub fn legacy_class(&self) -> &str {
        match self {
            Self::Io => "java.io.IOException",
            Self::FileNotFound => "java.io.FileNotFoundException",
            Self::NoSuchFile => "java.nio.file.NoSuchFileException",
            Self::AccessDenied => "java.nio.file.AccessDeniedException",
            Self::FileSystem => "java.nio.file.FileSystemException",
            Self::FileAlreadyExists => "java.nio.file.FileAlreadyExistsException",
            Self::InvalidPath => "java.nio.file.InvalidPathException",
            Self::IllegalArgument => "java.lang.IllegalArgumentException",
            Self::IllegalState => "java.lang.IllegalStateException",
            Self::Runtime => "java.lang.RuntimeException",
            Self::Arithmetic => "java.lang.ArithmeticException",
            Self::ClassCast => "java.lang.ClassCastException",
            Self::NumberFormat => "java.lang.NumberFormatException",
            Self::NullReference => "java.lang.NullPointerException",
            Self::Cancelled => "java.util.concurrent.CancellationException",
            Self::Timeout => "java.util.concurrent.TimeoutException",
            Self::TimeoutCancelled => "kotlinx.coroutines.TimeoutCancellationException",
            Self::DeviceLost => "com.malinskiy.marathon.exceptions.DeviceLostException",
            Self::DeviceSetup => "com.malinskiy.marathon.exceptions.DeviceSetupException",
            Self::IncompatibleDevice => "com.malinskiy.marathon.exceptions.IncompatibleDeviceException",
            Self::BatchExecution => "com.malinskiy.marathon.exceptions.TestBatchExecutionException",
            Self::TestParsing => "com.malinskiy.marathon.exceptions.TestParsingException",
            Self::NoDevices => "com.malinskiy.marathon.exceptions.NoDevicesException",
            Self::NoTests => "com.malinskiy.marathon.exceptions.NoTestCasesFoundException",
            Self::Transfer => "com.malinskiy.marathon.exceptions.TransferException",
            Self::Replay => "marathon.ReplayError",
            Self::ReplayClock => "ReplayError",
            Self::External(class) => class,
        }
    }
}
