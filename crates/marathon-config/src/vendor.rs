//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/vendor/VendorConfiguration.kt,
//! config/vendor/android/*.kt, config/vendor/apple/*.kt, config/vendor/apple/ios/*.kt,
//! config/vendor/apple/macos/PermissionsConfiguration.kt,
//! config/environment/{EnvironmentConfiguration,EnvironmentReader,SystemEnvironmentReader}.kt and
//! vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/extensions/ConfigurationExtensions.kt.
//!
//! Types only, plus the Kotlin `validate()` definitions, the SDK lookup and the
//! stage-(a) post-processing pieces of `ConfigurationFactory`. Binding defaults
//! live in `crate::bind_vendor`; no `Default` impl carries a binding default.

use crate::{
    VendorConfiguration,
    binding::{BindingCause, BindingContext, ConfigurationError, ValidationStage},
};
use jcompat::{
    JavaRegex,
    jtime::Duration,
    regex_patterns::{DEFAULT_TEST_CLASS_PATTERN, default_test_class_matches},
};
use marathon_error::FailureKind;
use std::{
    fmt,
    hash::{Hash, Hasher},
    path::PathBuf,
};

pub const CONFIGURATION_EXCEPTION_CLASS: FailureKind = FailureKind::Config(marathon_error::ConfigKind::Rejected);
pub const FILE_NOT_FOUND_EXCEPTION_CLASS: FailureKind = FailureKind::FileNotFound;
pub const ZIP_EXCEPTION_CLASS: FailureKind = FailureKind::Zip;
pub const IO_EXCEPTION_CLASS: FailureKind = FailureKind::Io;
pub const NO_SUCH_FILE_EXCEPTION_CLASS: FailureKind = FailureKind::NoSuchFile;

pub use marathon_error::config::ValidationKind;

/// A Kotlin `validate()` failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    pub kind: ValidationKind,
    pub message: String,
}
impl ValidationError {
    pub fn configuration(message: impl Into<String>) -> Self {
        Self {
            kind: ValidationKind::Configuration,
            message: message.into(),
        }
    }
    pub fn file_not_found(message: impl Into<String>) -> Self {
        Self {
            kind: ValidationKind::FileNotFound,
            message: message.into(),
        }
    }
    /// A `java.util.zip.ZipException` from bundle extraction (WP 2.3b).
    pub fn zip(message: impl Into<String>) -> Self {
        Self {
            kind: ValidationKind::Zip,
            message: message.into(),
        }
    }
    /// A plain `java.io.IOException` from bundle extraction (WP 2.3b).
    pub fn io(message: impl Into<String>) -> Self {
        Self {
            kind: ValidationKind::Io,
            message: message.into(),
        }
    }
    /// A `java.nio.file.NoSuchFileException`: `ZipFile` opening a missing
    /// archive (WP 2.3b).
    pub fn no_such_file(message: impl Into<String>) -> Self {
        Self {
            kind: ValidationKind::NoSuchFile,
            message: message.into(),
        }
    }
    /// The plain description of the validation kind.
    pub fn description(&self) -> &'static str {
        self.kind.description()
    }
}
impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ValidationError {}

/// `File.absolutePath`: absolute paths verbatim, relative ones against the
/// process directory. Used only for `validate()` messages.
pub(crate) fn absolute_path_text(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().into_owned();
    if text.starts_with('/') {
        return text;
    }
    match std::env::current_dir() {
        Ok(dir) => {
            let cwd = dir.to_string_lossy();
            let abs = jcompat::file_manager::absolute_file_path(&path.to_string_lossy(), &cwd);
            abs.to_string_lossy().into_owned()
        }
        Err(_) => text,
    }
}
fn file_exists(path: &std::path::Path) -> bool {
    path.exists()
}
fn file_is_regular(path: &std::path::Path) -> bool {
    path.is_file()
}

// ---- Android leaves (config/vendor/android/*.kt) ----------------------------

/// Port of config/vendor/android/AdbEndpoint.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdbEndpoint {
    pub host: String,
    pub port: i32,
}

/// Port of config/vendor/android/FileSyncConfiguration.kt (`PathRoot`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathRoot {
    ExternalStorage,
    AppData,
    LocalTmp,
}
/// Port of config/vendor/android/FileSyncConfiguration.kt (`AggregationMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggregationMode {
    Device,
    Pool,
    DeviceAndPool,
    TestRun,
}

/// Port of config/vendor/android/AllureConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllureConfiguration {
    pub enabled: bool,
    pub relative_results_directory: String,
    pub path_root: PathRoot,
}

/// Port of config/vendor/android/AndroidTestBundleConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidTestBundleConfiguration {
    pub application: Option<PathBuf>,
    pub test_application: PathBuf,
    pub extra_applications: Option<Vec<PathBuf>>,
    pub split_apks: Option<Vec<PathBuf>>,
}

/// Port of config/vendor/android/FileSyncConfiguration.kt. Sets are ordered
/// vectors with duplicates collapsed in first-seen order (P7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSyncConfiguration {
    pub pull: Vec<FileSyncEntry>,
    pub push: Vec<FilePushEntry>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSyncEntry {
    pub relative_path: String,
    pub path_root: PathRoot,
    pub aggregation_mode: AggregationMode,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePushEntry {
    pub path: String,
    pub path_root: PathRoot,
}

/// Port of config/vendor/android/ProfilingConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfilingConfiguration {
    pub enabled: bool,
    pub pbtxt: Option<PathBuf>,
    pub url_template: String,
}

/// Port of config/vendor/android/ScreenRecordConfiguration.kt (`RecorderType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AndroidRecorderType {
    Video,
    Screenshot,
}
/// `java.util.concurrent.TimeUnit`, in JDK declaration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeUnit {
    Nanoseconds,
    Microseconds,
    Milliseconds,
    Seconds,
    Minutes,
    Hours,
    Days,
}
/// Port of config/vendor/android/VideoConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidVideoConfiguration {
    pub enabled: bool,
    pub width: i32,
    pub height: i32,
    pub bitrate_mbps: i32,
    pub time_limit: i64,
    pub time_limit_units: TimeUnit,
}
/// Port of config/vendor/android/ScreenshotConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidScreenshotConfiguration {
    pub enabled: bool,
    pub width: i32,
    pub height: i32,
    pub delay_ms: i32,
}
/// Port of config/vendor/android/ScreenRecordConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidScreenRecordConfiguration {
    pub preferable_recorder_type: Option<AndroidRecorderType>,
    pub video_configuration: AndroidVideoConfiguration,
    pub screenshot_configuration: AndroidScreenshotConfiguration,
}

/// Port of config/vendor/android/SerialStrategy.kt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SerialStrategy {
    Automatic,
    MarathonProperty,
    BootProperty,
    Hostname,
    Ddms,
}

/// Port of config/vendor/android/TestAccessConfiguration.kt. `Debug` never
/// prints `consoleToken` (a secret); Kotlin's data-class `toString` does print
/// it, and WP 2.4's rendering keeps that asymmetry.
#[derive(Clone, PartialEq, Eq)]
pub struct TestAccessConfiguration {
    pub adb: bool,
    pub grpc: bool,
    pub console: bool,
    pub console_token: String,
}
impl fmt::Debug for TestAccessConfiguration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestAccessConfiguration")
            .field("adb", &self.adb)
            .field("grpc", &self.grpc)
            .field("console", &self.console)
            .field("console_token", &"<redacted>")
            .finish()
    }
}

/// Port of config/vendor/android/TestParserConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AndroidTestParserConfiguration {
    Local,
    Remote {
        instrumentation_args: Vec<(String, String)>,
    },
}

/// Port of config/vendor/android/ThreadingConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidThreadingConfiguration {
    pub boot_waiting_threads: i32,
    /// F5: accepted, stored and serialised but never used downstream.
    pub adb_io_threads: i32,
    pub install_threads: i32,
}

/// Port of config/vendor/android/TimeoutConfiguration.kt. `listFiles`,
/// `uninstall` and `portForward` default to the bound `shell`; the derivation
/// lives in [`android_timeout_configuration`] so binder and constructor agree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidTimeoutConfiguration {
    pub shell: Duration,
    pub list_files: Duration,
    pub push_file: Duration,
    pub push_folder: Duration,
    pub pull_file: Duration,
    pub uninstall: Duration,
    pub install: Duration,
    pub screenrecorder: Duration,
    pub screencapturer: Duration,
    pub socket_idle_timeout: Duration,
    pub port_forward: Duration,
    pub boot: Duration,
}
/// Inputs to [`android_timeout_configuration`]: `None` is "not supplied".
pub struct AndroidTimeoutParameters {
    pub shell: Option<Duration>,
    pub list_files: Option<Duration>,
    pub push_file: Option<Duration>,
    pub push_folder: Option<Duration>,
    pub pull_file: Option<Duration>,
    pub uninstall: Option<Duration>,
    pub install: Option<Duration>,
    pub screenrecorder: Option<Duration>,
    pub screencapturer: Option<Duration>,
    pub socket_idle_timeout: Option<Duration>,
    pub port_forward: Option<Duration>,
    pub boot: Option<Duration>,
}
impl AndroidTimeoutParameters {
    pub fn empty() -> Self {
        Self {
            shell: None,
            list_files: None,
            push_file: None,
            push_folder: None,
            pull_file: None,
            uninstall: None,
            install: None,
            screenrecorder: None,
            screencapturer: None,
            socket_idle_timeout: None,
            port_forward: None,
            boot: None,
        }
    }
}
/// The Kotlin primary constructor of android `TimeoutConfiguration`.
pub fn android_timeout_configuration(p: AndroidTimeoutParameters) -> AndroidTimeoutConfiguration {
    let shell = p.shell.unwrap_or(Duration::of_seconds(20));
    AndroidTimeoutConfiguration {
        shell,
        list_files: p.list_files.unwrap_or(shell),
        push_file: p.push_file.unwrap_or(Duration::of_seconds(60)),
        push_folder: p.push_folder.unwrap_or(Duration::of_seconds(60)),
        pull_file: p.pull_file.unwrap_or(Duration::of_seconds(30)),
        uninstall: p.uninstall.unwrap_or(shell),
        install: p.install.unwrap_or(Duration::of_seconds(60)),
        screenrecorder: p.screenrecorder.unwrap_or(Duration::of_seconds(200)),
        screencapturer: p
            .screencapturer
            .unwrap_or(jcompat::jtime::duration_of(0, 300_000_000).unwrap_or(Duration::of_seconds(0))),
        socket_idle_timeout: p.socket_idle_timeout.unwrap_or(Duration::of_seconds(30)),
        port_forward: p.port_forward.unwrap_or(shell),
        boot: p.boot.unwrap_or(Duration::of_seconds(30)),
    }
}

// ---- Apple shared leaves (config/vendor/apple/*.kt) --------------------------

/// Port of config/vendor/apple/DeviceProvider.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceProvider {
    Static { path: Option<PathBuf> },
    Dynamic { host: String, port: i32 },
}

/// Port of config/vendor/apple/RsyncConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RsyncConfiguration {
    pub remote_path: String,
}

/// Port of config/vendor/apple/SshConfiguration.kt (`SshAuthentication`).
/// `Debug` never prints the password; Kotlin's `toString` shows only the
/// username for password authentication (WP 2.4 renders that form).
#[derive(Clone, Eq)]
pub enum SshAuthentication {
    Password { username: String, password: String },
    PublicKey { username: String, key: PathBuf },
}
// Worker keys retain the key path's spelling, including dot components.
impl PartialEq for SshAuthentication {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Password {
                    username: a,
                    password: ap,
                },
                Self::Password {
                    username: b,
                    password: bp,
                },
            ) => a == b && ap == bp,
            (Self::PublicKey { username: a, key: ak }, Self::PublicKey { username: b, key: bk }) => {
                a == b && ak.as_os_str() == bk.as_os_str()
            }
            _ => false,
        }
    }
}
impl fmt::Debug for SshAuthentication {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password { username, .. } => f
                .debug_struct("Password")
                .field("username", username)
                .field("password", &"<redacted>")
                .finish(),
            Self::PublicKey { username, key } => f
                .debug_struct("PublicKey")
                .field("username", username)
                .field("key", key)
                .finish(),
        }
    }
}

/// Port of config/vendor/apple/SshConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshConfiguration {
    pub authentication: Option<SshAuthentication>,
    pub known_hosts_path: Option<PathBuf>,
    pub keep_alive_interval: Duration,
    pub keep_alive_max_count: i32,
    pub debug: bool,
    pub share_worker_connection: bool,
}
impl SshConfiguration {
    /// `SshConfiguration.validate()` (SshConfiguration.kt:27-42): key, then
    /// known-hosts, then the keep-alive count.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if let Some(SshAuthentication::PublicKey { key, .. }) = &self.authentication
            && !file_exists(key)
        {
            return Err(ValidationError::file_not_found(format!(
                "Private key not found at {}",
                absolute_path_text(key)
            )));
        }
        if let Some(path) = &self.known_hosts_path
            && !file_exists(path)
        {
            return Err(ValidationError::file_not_found(format!(
                "knownhosts file not found at {}",
                path.to_string_lossy().into_owned()
            )));
        }
        if self.keep_alive_max_count < 1 {
            return Err(ValidationError::configuration(format!(
                "ssh.keepAliveMaxCount should be at least 1, got {}",
                self.keep_alive_max_count
            )));
        }
        Ok(())
    }
}

/// Port of config/vendor/apple/TestType.kt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestType {
    Xcuitest,
    Xctest,
    LogicTest,
}
impl TestType {
    pub fn value(self) -> &'static str {
        match self {
            Self::Xcuitest => "xcuitest",
            Self::Xctest => "xctest",
            Self::LogicTest => "logic_test",
        }
    }
}

/// Bindable data layout of config/vendor/apple/AppleTestBundleConfiguration.kt
/// (WP 2.3a decision 2), plus the resolved paths WP 2.3b's `validate()`
/// records (the Kotlin `@JsonIgnore var`s `app`/`testApp` and `lateinit`
/// `xctest`; `xctest` is always set after a successful `validate()`).
/// Equality covers only the six bound fields, as Kotlin's data-class
/// equality does (WP 2.3a findings; the `tempDirFor` lambda has no Rust
/// counterpart, WP 2.3b S7).
#[derive(Clone, Debug)]
pub struct AppleTestBundleConfiguration {
    pub application: Option<PathBuf>,
    pub test_application: Option<PathBuf>,
    pub extra_applications: Option<Vec<PathBuf>>,
    pub derived_data_dir: Option<PathBuf>,
    pub test_type: Option<TestType>,
    pub xctestrun: Option<PathBuf>,
    pub app: Option<PathBuf>,
    pub test_app: Option<PathBuf>,
    pub xctest: Option<PathBuf>,
}
impl PartialEq for AppleTestBundleConfiguration {
    fn eq(&self, other: &Self) -> bool {
        self.application == other.application
            && self.test_application == other.test_application
            && self.extra_applications == other.extra_applications
            && self.derived_data_dir == other.derived_data_dir
            && self.test_type == other.test_type
            && self.xctestrun == other.xctestrun
    }
}
impl Eq for AppleTestBundleConfiguration {}
impl AppleTestBundleConfiguration {
    /// The five bound fields with no resolved paths yet, as binding produces.
    pub fn unresolved(
        application: Option<PathBuf>,
        test_application: Option<PathBuf>,
        extra_applications: Option<Vec<PathBuf>>,
        derived_data_dir: Option<PathBuf>,
        test_type: Option<TestType>,
    ) -> Self {
        Self {
            application,
            test_application,
            extra_applications,
            derived_data_dir,
            test_type,
            xctestrun: None,
            app: None,
            test_app: None,
            xctest: None,
        }
    }
}

/// Port of config/vendor/apple/ThreadingConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppleThreadingConfiguration {
    pub device_provider_threads: i32,
    pub device_threads: i32,
    pub provisioning_concurrency: i32,
}
impl AppleThreadingConfiguration {
    /// The Kotlin primary-constructor defaults; `LogicalConfigurationValidator`
    /// (WP 2.4, F6) compares a macOS value against a fresh default instance.
    pub fn default_instance() -> Self {
        Self {
            device_provider_threads: 8,
            device_threads: 2,
            provisioning_concurrency: 2,
        }
    }
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.provisioning_concurrency < 1 {
            return Err(ValidationError::configuration(format!(
                "threadingConfiguration.provisioningConcurrency should be at least 1, got {}",
                self.provisioning_concurrency
            )));
        }
        Ok(())
    }
}

/// Port of config/vendor/apple/TimeoutConfiguration.kt. The chained and
/// floored defaults live in [`apple_timeout_configuration`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppleTimeoutConfiguration {
    pub shell: Duration,
    pub shell_idle: Duration,
    pub reachability: Duration,
    pub screenshot: Duration,
    pub video: Duration,
    pub erase: Duration,
    pub shutdown: Duration,
    pub delete: Duration,
    pub create: Duration,
    pub boot: Duration,
    pub install: Duration,
    pub uninstall: Duration,
    pub import_media: Duration,
    pub test_destination: Duration,
    pub xcresulttool: Duration,
    pub boot_wait: Duration,
}
impl AppleTimeoutConfiguration {
    /// `TimeoutConfiguration.INFINITE` (7 days).
    pub fn infinite() -> Duration {
        Duration::of_seconds(7 * 24 * 3600)
    }
}
/// Inputs to [`apple_timeout_configuration`]: `None` is "not supplied".
pub struct AppleTimeoutParameters {
    pub shell: Option<Duration>,
    pub shell_idle: Option<Duration>,
    pub reachability: Option<Duration>,
    pub screenshot: Option<Duration>,
    pub video: Option<Duration>,
    pub erase: Option<Duration>,
    pub shutdown: Option<Duration>,
    pub delete: Option<Duration>,
    pub create: Option<Duration>,
    pub boot: Option<Duration>,
    pub install: Option<Duration>,
    pub uninstall: Option<Duration>,
    pub import_media: Option<Duration>,
    pub test_destination: Option<Duration>,
    pub xcresulttool: Option<Duration>,
    pub boot_wait: Option<Duration>,
}
impl AppleTimeoutParameters {
    pub fn empty() -> Self {
        Self {
            shell: None,
            shell_idle: None,
            reachability: None,
            screenshot: None,
            video: None,
            erase: None,
            shutdown: None,
            delete: None,
            create: None,
            boot: None,
            install: None,
            uninstall: None,
            import_media: None,
            test_destination: None,
            xcresulttool: None,
            boot_wait: None,
        }
    }
}
/// The Kotlin primary constructor of apple `TimeoutConfiguration`
/// (TimeoutConfiguration.kt:18-34).
pub fn apple_timeout_configuration(p: AppleTimeoutParameters) -> AppleTimeoutConfiguration {
    let shell = p.shell.unwrap_or(Duration::of_seconds(30));
    let shutdown = p.shutdown.unwrap_or(shell);
    let boot = p.boot.unwrap_or(shell);
    AppleTimeoutConfiguration {
        shell,
        shell_idle: p.shell_idle.unwrap_or(Duration::of_seconds(30)),
        reachability: p.reachability.unwrap_or(Duration::of_seconds(5)),
        screenshot: p.screenshot.unwrap_or(Duration::of_seconds(10)),
        video: p.video.unwrap_or(Duration::of_seconds(300)),
        erase: p.erase.unwrap_or(shell),
        shutdown,
        delete: p.delete.unwrap_or(shutdown),
        create: p.create.unwrap_or(Duration::of_seconds(30)),
        boot,
        install: p.install.unwrap_or(shell),
        uninstall: p.uninstall.unwrap_or(shell),
        import_media: p.import_media.unwrap_or(shell),
        test_destination: p.test_destination.unwrap_or(Duration::of_seconds(30)),
        xcresulttool: p.xcresulttool.unwrap_or_else(|| shell.max(Duration::of_seconds(120))),
        boot_wait: p.boot_wait.unwrap_or_else(|| boot.max(Duration::of_seconds(180))),
    }
}

/// One element of `NmTestParserConfiguration.testClassRegexes`. The Kotlin
/// default pattern uses a negative lookahead the Rust `regex` crate cannot
/// compile (D6), so the default stays distinguishable from an explicitly
/// supplied pattern (WP 2.3a decision 7): only explicit values are compiled,
/// and an explicit copy of the default pattern is rejected at bind time.
#[derive(Clone, Debug)]
pub enum TestClassRegex {
    Default,
    Explicit(JavaRegex),
}
impl TestClassRegex {
    pub fn pattern(&self) -> &str {
        match self {
            Self::Default => DEFAULT_TEST_CLASS_PATTERN,
            Self::Explicit(regex) => regex.pattern(),
        }
    }
    pub fn matches(&self, input: &str) -> bool {
        match self {
            Self::Default => default_test_class_matches(input),
            Self::Explicit(regex) => regex.matches(input),
        }
    }
}
impl PartialEq for TestClassRegex {
    fn eq(&self, other: &Self) -> bool {
        self.pattern() == other.pattern()
    }
}
impl Eq for TestClassRegex {}
impl Hash for TestClassRegex {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.pattern().hash(state);
    }
}

/// Port of config/vendor/apple/TestParserConfiguration.kt. Equality and
/// hashing are pattern-based and mutually consistent (0.6 D1); Kotlin's
/// `equals` compares patterns while `hashCode` hashes the `Regex` objects.
#[derive(Clone, Debug)]
pub enum AppleTestParserConfiguration {
    Nm { test_class_regexes: Vec<TestClassRegex> },
    Xctest,
}
impl PartialEq for AppleTestParserConfiguration {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Nm { test_class_regexes: a }, Self::Nm { test_class_regexes: b }) => a == b,
            (Self::Xctest, Self::Xctest) => true,
            _ => false,
        }
    }
}
impl Eq for AppleTestParserConfiguration {}
impl Hash for AppleTestParserConfiguration {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Nm { test_class_regexes } => {
                0u8.hash(state);
                test_class_regexes.hash(state);
            }
            Self::Xctest => 1u8.hash(state),
        }
    }
}

// ---- iOS leaves (config/vendor/apple/ios/*.kt) -------------------------------

/// Port of config/vendor/apple/ios/Display.kt. `value` is the simctl spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    Internal,
    External,
}
impl Display {
    pub fn value(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::External => "external",
        }
    }
}
/// Port of config/vendor/apple/ios/Mask.kt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mask {
    Ignored,
    Black,
}
impl Mask {
    pub fn value(self) -> &'static str {
        match self {
            Self::Ignored => "ignored",
            Self::Black => "black",
        }
    }
}

/// Port of config/vendor/apple/ios/LifecycleConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecycleConfiguration {
    pub shutdown_unused: bool,
    pub on_prepare: Vec<LifecycleAction>,
    pub on_dispose: Vec<LifecycleAction>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleAction {
    Terminate,
    Shutdown,
    Erase,
}

/// Port of config/vendor/apple/ios/PermissionsConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IosPermissionsConfiguration {
    pub grant: Vec<IosPermission>,
    pub lifecycle: GrantLifecycle,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantLifecycle {
    BeforeTestRun,
    BeforeEachBatch,
}
/// Port of ios/PermissionsConfiguration.kt (`Permission`). `value` is mostly
/// the simctl name, but `kTCCService…` for the last two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IosPermission {
    All,
    Calendar,
    ContactsLimited,
    Contacts,
    Location,
    LocationAlways,
    PhotosAdd,
    Photos,
    MediaLibrary,
    Microphone,
    Motion,
    Reminders,
    Siri,
    UserTracking,
    Pasteboard,
}
impl IosPermission {
    pub fn value(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Calendar => "calendar",
            Self::ContactsLimited => "contacts-limited",
            Self::Contacts => "contacts",
            Self::Location => "location",
            Self::LocationAlways => "location-always",
            Self::PhotosAdd => "photos-add",
            Self::Photos => "photos",
            Self::MediaLibrary => "media-library",
            Self::Microphone => "microphone",
            Self::Motion => "motion",
            Self::Reminders => "reminders",
            Self::Siri => "siri",
            Self::UserTracking => "kTCCServiceUserTracking",
            Self::Pasteboard => "kTCCServicePasteboard",
        }
    }
}

/// Port of config/vendor/apple/ios/ScreenRecordConfiguration.kt (`RecorderType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IosRecorderType {
    Video,
    Screenshot,
}
/// Port of config/vendor/apple/ios/ScreenshotConfiguration.kt (`Type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenshotType {
    Png,
    Tiff,
    Bmp,
    Jpeg,
}
impl ScreenshotType {
    pub fn value(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Tiff => "tiff",
            Self::Bmp => "bmp",
            Self::Jpeg => "jpeg",
        }
    }
}
/// Port of config/vendor/apple/ios/ScreenshotConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IosScreenshotConfiguration {
    pub enabled: bool,
    pub screenshot_type: ScreenshotType,
    pub display: Display,
    pub mask: Mask,
    pub delay: Duration,
    pub width: i32,
    pub height: i32,
}

/// Port of config/vendor/apple/ios/VideoConfiguration.kt (`Codec`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}
impl Codec {
    pub fn value(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
        }
    }
}
/// Port of ios/VideoConfiguration.kt (`Size`); `value` is a width in px.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscodingSize {
    Hd480,
    Hd720,
    Hd1080,
}
impl TranscodingSize {
    pub fn value(self) -> i32 {
        match self {
            Self::Hd480 => 852,
            Self::Hd720 => 1280,
            Self::Hd1080 => 1920,
        }
    }
}
/// Port of ios/VideoConfiguration.kt (`TranscodingConfiguration`). The YAML
/// key `ffmpegPath` binds the Kotlin `binary` property.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscodingConfiguration {
    pub enabled: bool,
    pub binary: String,
    pub size: TranscodingSize,
}
/// Port of config/vendor/apple/ios/VideoConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IosVideoConfiguration {
    pub enabled: bool,
    pub codec: Codec,
    pub display: Display,
    pub mask: Mask,
    pub transcoding: TranscodingConfiguration,
}
/// Port of config/vendor/apple/ios/ScreenRecordConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IosScreenRecordConfiguration {
    pub preferable_recorder_type: Option<IosRecorderType>,
    pub video_configuration: IosVideoConfiguration,
    pub screenshot_configuration: IosScreenshotConfiguration,
}

/// Port of config/vendor/apple/ios/SigningConfiguration.kt. A `String`, not a
/// `File`: no Marathonfile-relative resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigningConfiguration {
    pub test_runner_provisioning_profile: Option<String>,
}

/// Port of config/vendor/apple/ios/WorkerConnectionConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerConnectionConfiguration {
    pub attempts: i32,
    pub backoff: Duration,
    pub retry_interval: Duration,
    pub late_join: bool,
    pub late_join_cutoff: Duration,
}
impl WorkerConnectionConfiguration {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.attempts < 1 {
            return Err(ValidationError::configuration(format!(
                "workerConnection.attempts should be at least 1, got {}",
                self.attempts
            )));
        }
        if self.backoff.is_negative() {
            return Err(ValidationError::configuration(format!(
                "workerConnection.backoff should not be negative, got {}",
                self.backoff
            )));
        }
        if self.late_join_cutoff.is_negative() {
            return Err(ValidationError::configuration(format!(
                "workerConnection.lateJoinCutoff should not be negative, got {}",
                self.late_join_cutoff
            )));
        }
        if self.retry_interval.is_negative() || self.retry_interval.is_zero() {
            return Err(ValidationError::configuration(format!(
                "workerConnection.retryInterval should be positive, got {}",
                self.retry_interval
            )));
        }
        Ok(())
    }
    /// `pauseAfter(attempt)` (WorkerConnectionConfiguration.kt:46-59).
    /// Approved simplification: Kotlin's doubling throws on `Duration`
    /// overflow; here an overflowing pause saturates to the capped
    /// `retryInterval`, since it is already at or past any cap and the
    /// signature has no error channel.
    pub fn pause_after(&self, attempt: i32) -> Option<Duration> {
        if attempt < self.attempts {
            let mut pause = self.backoff;
            // Kotlin `Int` subtraction wraps.
            let mut doublings = attempt.wrapping_sub(1);
            while doublings > 0 && !pause.is_zero() && pause < self.retry_interval {
                match pause.checked_mul(2) {
                    Some(doubled) => pause = doubled,
                    None => return Some(self.retry_interval),
                }
                doublings -= 1;
            }
            Some(pause.min(self.retry_interval))
        } else if self.late_join {
            Some(self.retry_interval)
        } else {
            None
        }
    }
}

/// Port of config/vendor/apple/ios/XcresultConfiguration.kt (`Lifetime`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifetime {
    KeepAlways,
    DeleteOnSuccess,
    KeepNever,
}
impl Lifetime {
    pub fn value(self) -> &'static str {
        match self {
            Self::KeepAlways => "keepAlways",
            Self::DeleteOnSuccess => "deleteOnSuccess",
            Self::KeepNever => "keepNever",
        }
    }
}
/// Port of ios/XcresultConfiguration.kt (`PullingPolicy`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullingPolicy {
    Never,
    Always,
    OnFailure,
}
/// Port of ios/XcresultConfiguration.kt (`ScreenCaptureFormat`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenCaptureFormat {
    Screenshots,
    ScreenRecording,
}
impl ScreenCaptureFormat {
    pub fn xcode_value(self) -> &'static str {
        match self {
            Self::Screenshots => "screenshots",
            Self::ScreenRecording => "screenRecording",
        }
    }
}
/// Port of ios/XcresultConfiguration.kt (`AttachmentsConfiguration`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentsConfiguration {
    pub system_attachment_lifetime: Lifetime,
    pub user_attachment_lifetime: Lifetime,
}
/// Port of ios/XcresultConfiguration.kt (`XcresultQueueConfiguration`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XcresultQueueConfiguration {
    pub capacity: i32,
    pub concurrency: i32,
    pub worker_capacity: Option<i32>,
}
impl XcresultQueueConfiguration {
    pub const DEFAULT_CAPACITY: i32 = 8;
    pub const DEFAULT_CONCURRENCY: i32 = 4;
    /// The Kotlin primary-constructor defaults; WP 2.4's validator compares a
    /// macOS value against a fresh default instance (F6).
    pub fn default_instance() -> Self {
        Self {
            capacity: Self::DEFAULT_CAPACITY,
            concurrency: Self::DEFAULT_CONCURRENCY,
            worker_capacity: None,
        }
    }
    pub fn default_worker_capacity(capacity: i32, concurrency: i32) -> i32 {
        // Approved simplification: `concurrency == 0` throws in Kotlin and
        // is rejected by `validate()` first; here it returns 1 (note for
        // WP 4.9, whose ArtifactProcessor passes caller values).
        if concurrency == 0 {
            return 1;
        }
        // Kotlin `Int` division wraps (`MIN / -1 == MIN`, so the result is 1).
        capacity.wrapping_div(concurrency).max(1)
    }
    pub fn effective_worker_capacity(&self) -> i32 {
        self.worker_capacity
            .unwrap_or_else(|| Self::default_worker_capacity(self.capacity, self.concurrency))
    }
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.capacity < 1 {
            return Err(ValidationError::configuration(format!(
                "xcresult.queue.capacity should be at least 1, got {}",
                self.capacity
            )));
        }
        if self.concurrency < 1 || self.concurrency > self.capacity {
            return Err(ValidationError::configuration(format!(
                "xcresult.queue.concurrency should be between 1 and xcresult.queue.capacity ({}), got {}",
                self.capacity, self.concurrency
            )));
        }
        if let Some(worker_capacity) = self.worker_capacity
            && (worker_capacity < 1 || worker_capacity > self.capacity)
        {
            return Err(ValidationError::configuration(format!(
                "xcresult.queue.workerCapacity should be between 1 and xcresult.queue.capacity ({}), got {}",
                self.capacity, worker_capacity
            )));
        }
        Ok(())
    }
}
/// Port of config/vendor/apple/ios/XcresultConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XcresultConfiguration {
    pub pull: Option<bool>,
    pub pulling_policy: PullingPolicy,
    pub remote_clean: bool,
    pub attachments: AttachmentsConfiguration,
    pub preferred_screen_capture_format: Option<ScreenCaptureFormat>,
    pub queue: XcresultQueueConfiguration,
}

/// Port of config/vendor/apple/ios/XctestrunEnvConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XctestrunEnvConfiguration {
    pub app_envs: Vec<(String, String)>,
    pub test_envs: Vec<(String, String)>,
}

// ---- macOS leaves (config/vendor/apple/macos/PermissionsConfiguration.kt) -----

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacosPermissionsConfiguration {
    pub grant: Vec<(String, Vec<MacosPermission>)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacosPermission {
    Accessibility,
    DeveloperTool,
}
impl MacosPermission {
    pub fn value(self) -> &'static str {
        match self {
            Self::Accessibility => "kTCCServiceAccessibility",
            Self::DeveloperTool => "kTCCServiceDeveloperTool",
        }
    }
}

// ---- Top-level vendor types (VendorConfiguration.kt) --------------------------

/// Port of `VendorConfiguration.AndroidConfiguration` (VendorConfiguration.kt:57-82).
/// Field names keep the Kotlin property names; YAML renames live in the binder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidConfiguration {
    pub android_sdk: Option<PathBuf>,
    pub application_output: Option<PathBuf>,
    pub test_application_output: Option<PathBuf>,
    pub split_apks: Option<Vec<PathBuf>>,
    pub extra_applications_output: Option<Vec<PathBuf>>,
    pub outputs: Option<Vec<AndroidTestBundleConfiguration>>,
    pub auto_grant_permission: bool,
    pub instrumentation_args: Vec<(String, String)>,
    pub application_pm_clear: bool,
    pub test_application_pm_clear: bool,
    /// F5: accepted but never used downstream.
    pub adb_init_timeout_millis: i32,
    pub install_options: String,
    pub serial_strategy: SerialStrategy,
    pub screen_record_configuration: AndroidScreenRecordConfiguration,
    pub wait_for_devices_timeout_millis: i64,
    pub allure_configuration: AllureConfiguration,
    pub timeout_configuration: AndroidTimeoutConfiguration,
    pub file_sync_configuration: FileSyncConfiguration,
    pub threading_configuration: AndroidThreadingConfiguration,
    pub test_parser_configuration: AndroidTestParserConfiguration,
    pub test_access_configuration: TestAccessConfiguration,
    pub adb_servers: Vec<AdbEndpoint>,
    pub disable_window_animation: bool,
    pub profiling_configuration: ProfilingConfiguration,
    pub mock_location: bool,
}
impl AndroidConfiguration {
    pub fn safe_android_sdk(&self) -> Result<PathBuf, ValidationError> {
        self.android_sdk
            .clone()
            .ok_or_else(|| ValidationError::configuration("No android SDK path specified"))
    }
    /// `AndroidConfiguration.validate()` (VendorConfiguration.kt:86-104).
    pub fn validate(&self) -> Result<(), ValidationError> {
        fn check(file: Option<&std::path::Path>) -> Result<(), ValidationError> {
            if let Some(file) = file {
                if !file_exists(file) {
                    return Err(ValidationError::configuration(format!(
                        "{} does not exist",
                        absolute_path_text(file)
                    )));
                }
                if !file_is_regular(file) {
                    return Err(ValidationError::configuration(format!(
                        "{} must be a regular file",
                        absolute_path_text(file)
                    )));
                }
            }
            Ok(())
        }
        check(self.application_output.as_deref())?;
        check(self.test_application_output.as_deref())?;
        if let Some(files) = &self.split_apks {
            for file in files {
                check(Some(file))?;
            }
        }
        if let Some(files) = &self.extra_applications_output {
            for file in files {
                check(Some(file))?;
            }
        }
        if let Some(outputs) = &self.outputs {
            for output in outputs {
                check(output.application.as_deref())?;
                check(Some(&output.test_application))?;
                if let Some(files) = &output.split_apks {
                    for file in files {
                        check(Some(file))?;
                    }
                }
                if let Some(files) = &output.extra_applications {
                    for file in files {
                        check(Some(file))?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Port of `VendorConfiguration.IOSConfiguration` (VendorConfiguration.kt:166-195).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IosConfiguration {
    pub bundle: Option<AppleTestBundleConfiguration>,
    pub device_provider: DeviceProvider,
    pub devices_file: Option<PathBuf>,
    pub ssh: SshConfiguration,
    pub worker_connection: WorkerConnectionConfiguration,
    pub xcresult: XcresultConfiguration,
    pub screen_record_configuration: IosScreenRecordConfiguration,
    pub xctestrun_env: XctestrunEnvConfiguration,
    pub lifecycle_configuration: LifecycleConfiguration,
    pub permissions: IosPermissionsConfiguration,
    pub media_files: Option<Vec<PathBuf>>,
    pub timeout_configuration: AppleTimeoutConfiguration,
    pub threading_configuration: AppleThreadingConfiguration,
    pub hide_runner_output: bool,
    pub compact_output: bool,
    pub rsync: RsyncConfiguration,
    pub xcodebuild_test_args: Vec<(String, String)>,
    pub data_container_clear: bool,
    pub test_parser_configuration: AppleTestParserConfiguration,
    pub device_log: bool,
    pub signing: SigningConfiguration,
    pub skip_copying_derived_data: bool,
    pub max_batches_without_progress: i32,
}
impl IosConfiguration {
    /// `IOSConfiguration.validate()` (VendorConfiguration.kt:197-208),
    /// including the trailing `bundle.validate()` (added by WP 2.3b; it
    /// re-runs extraction, as Kotlin's stage-(b) call does after stage
    /// (a)'s). The bundle is cloned for that call because this signature
    /// is `&self` (the frozen stage-(b) hooks take `&VendorConfiguration`);
    /// the clone's resolved paths equal the ones stage (a) recorded.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.max_batches_without_progress < 0 {
            return Err(ValidationError::configuration(format!(
                "maxBatchesWithoutProgress should not be negative, got {}",
                self.max_batches_without_progress
            )));
        }
        self.ssh.validate()?;
        self.worker_connection.validate()?;
        self.xcresult.queue.validate()?;
        self.threading_configuration.validate()?;
        let Some(bundle) = &self.bundle else {
            return Err(ValidationError::configuration(
                "bundles must contain at least one valid entry",
            ));
        };
        bundle.clone().validate()
    }
}

/// Port of `VendorConfiguration.MacosConfiguration` (VendorConfiguration.kt:210-226).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacosConfiguration {
    pub bundle: Option<AppleTestBundleConfiguration>,
    pub device_provider: DeviceProvider,
    pub devices_file: Option<PathBuf>,
    pub ssh: SshConfiguration,
    pub xcresult: XcresultConfiguration,
    pub xctestrun_env: XctestrunEnvConfiguration,
    pub timeout_configuration: AppleTimeoutConfiguration,
    pub threading_configuration: AppleThreadingConfiguration,
    pub hide_runner_output: bool,
    pub compact_output: bool,
    pub rsync: RsyncConfiguration,
    pub xcodebuild_test_args: Vec<(String, String)>,
    pub test_parser_configuration: AppleTestParserConfiguration,
    pub permissions: MacosPermissionsConfiguration,
}
impl MacosConfiguration {
    /// `MacosConfiguration.validate()` (VendorConfiguration.kt:228-233),
    /// including the trailing `bundle.validate()` on a clone (see
    /// `IosConfiguration::validate`). Never called at stage (b) in Kotlin
    /// (F6); WP 2.4 owns that staging.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.ssh.validate()?;
        let Some(bundle) = &self.bundle else {
            return Err(ValidationError::configuration(
                "bundles must contain at least one valid entry",
            ));
        };
        bundle.clone().validate()
    }
}

// ---- SDK lookup (config/environment/*.kt) -------------------------------------

/// Port of config/environment/EnvironmentConfiguration.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentConfiguration {
    pub android_sdk: Option<PathBuf>,
}

/// Port of `SystemEnvironmentReader.read()` over an ordered environment:
/// `ANDROID_HOME`, else `ANDROID_SDK_ROOT`, else none. A repeated name
/// resolves to its FIRST occurrence, as JVM `System.getenv` does. P2
/// (approved): an empty first occurrence counts as unset (and does not
/// fall back to a later occurrence), where Kotlin accepts `File("")`.
/// The found string becomes a path verbatim: no existence check, no
/// canonicalisation, relative stays relative.
pub fn read_environment(environment: &[(String, String)]) -> EnvironmentConfiguration {
    fn lookup<'a>(environment: &'a [(String, String)], name: &str) -> Option<&'a str> {
        let name: String = name.into();
        environment
            .iter()
            .find(|(key, _)| key == &name)
            .map(|(_, value)| value)
            .filter(|value| !value.is_empty())
            .map(|x| x.as_str())
    }
    let sdk = lookup(environment, "ANDROID_HOME").or_else(|| lookup(environment, "ANDROID_SDK_ROOT"));
    EnvironmentConfiguration {
        android_sdk: sdk.map(PathBuf::from),
    }
}

// ---- Stage-(a) post-processing (ConfigurationFactory.kt:58-188,211) ----------
// Free functions for WP 2.4's `VendorConfigurationHooks` impl (decision 4).
// Within a vendor branch the factory resolves the bundle (and calls
// `bundle.validate()`, WP 2.3b) first, then media files, devices, SSH and
// xcresult, and only then copies everything into the configuration.

fn parse_stage_error(class: FailureKind, message: impl Into<String>) -> Box<ConfigurationError> {
    let message = message.into();
    Box::new(ConfigurationError {
        stage: ValidationStage::Parse,
        kind: class,
        message: Some(message.to_owned()),
        causes: Vec::new(),
        path: Vec::new(),
        span: None,
        cause: BindingCause::Custom(message),
    })
}

/// The Android branch: fill a missing `androidSdk` from the environment
/// (`ConfigurationFactory.kt:59-70`). A bound value wins and the environment
/// is not consulted.
pub fn fill_android_sdk(
    config: &mut AndroidConfiguration,
    context: &BindingContext,
) -> Result<(), Box<ConfigurationError>> {
    if config.android_sdk.is_none() {
        let sdk = read_environment(&context.environment)
            .android_sdk
            .ok_or_else(|| parse_stage_error(CONFIGURATION_EXCEPTION_CLASS, "No android SDK path specified"))?;
        config.android_sdk = Some(sdk);
    }
    Ok(())
}

/// `File(parent).resolve(child)`: an absolute child is itself, otherwise the
/// child appended to the parent. No canonicalisation. Delegates to
/// `jcompat`'s slash normalization, retaining dot components.
fn resolve(parent: &std::path::Path, child: &std::path::Path) -> PathBuf {
    jcompat::file_manager::absolute_file_path(&child.to_string_lossy(), &parent.to_string_lossy())
}

/// `File.resolveAgainst` (ConfigurationFactory.kt:211):
/// `dir.resolve(this).canonicalFile`.
fn resolve_against(dir: &std::path::Path, path: &std::path::Path) -> Result<PathBuf, Box<ConfigurationError>> {
    let resolved = resolve(dir, path);
    let resolved_buf: PathBuf = resolved;
    let canonical =
        crate::paths::canonical_file(&resolved_buf).map_err(|e| parse_stage_error(FailureKind::Io, e.to_string()))?;
    let text = canonical
        .to_str()
        .ok_or_else(|| parse_stage_error(FailureKind::Io, "canonical path must be UTF-8"))?;
    Ok(PathBuf::from(text))
}

/// Step 1 of the factory's Apple branches: re-resolve the bundle paths
/// against `marathonfileDir` with plain `resolve` (no canonicalisation).
/// Public so WP 2.3b/2.4 can run `bundle.validate()` on the result before
/// any `canonicalFile` step (devices/SSH) runs.
pub fn resolve_bundle(bundle: &AppleTestBundleConfiguration, dir: &std::path::Path) -> AppleTestBundleConfiguration {
    // The factory builds a fresh bundle and calls `validate()` on it here;
    // WP 2.3b owns that call and its extraction side effects.
    // A fresh bundle in Kotlin, so any previously resolved paths are dropped.
    let mut resolved = AppleTestBundleConfiguration::unresolved(
        bundle.application.as_ref().map(|p| resolve(dir, p)),
        bundle.test_application.as_ref().map(|p| resolve(dir, p)),
        bundle
            .extra_applications
            .as_ref()
            .map(|files| files.iter().map(|p| resolve(dir, p)).collect()),
        bundle.derived_data_dir.as_ref().map(|p| resolve(dir, p)),
        bundle.test_type,
    );
    resolved.xctestrun = bundle.xctestrun.as_ref().map(|p| resolve(dir, p));
    resolved
}

/// The SSH step of the factory's Apple branches. `knownHostsPath` is
/// resolved BEFORE the public-key `key`, as in
/// `ConfigurationFactory.kt:94,97-100` (so a `canonicalFile` failure on
/// the known-hosts path precedes one on the key).
pub fn resolve_ssh(ssh: &SshConfiguration, dir: &std::path::Path) -> Result<SshConfiguration, Box<ConfigurationError>> {
    let known_hosts_path = match &ssh.known_hosts_path {
        Some(path) => Some(resolve_against(dir, path)?),
        None => None,
    };
    let authentication = match &ssh.authentication {
        Some(SshAuthentication::PublicKey { username, key }) => Some(SshAuthentication::PublicKey {
            username: username.clone(),
            key: resolve_against(dir, key)?,
        }),
        other => other.clone(),
    };
    Ok(SshConfiguration {
        authentication,
        known_hosts_path,
        ..ssh.clone()
    })
}

/// The devices step: `devicesFile?.resolveAgainst(dir)` else
/// `dir.resolve("Marathondevices")` (not canonicalised).
pub fn resolve_devices(
    devices: Option<&std::path::Path>,
    dir: &std::path::Path,
) -> Result<PathBuf, Box<ConfigurationError>> {
    match devices {
        Some(path) => resolve_against(dir, path),
        None => Ok(resolve(dir, &PathBuf::from("Marathondevices"))),
    }
}

/// `xcresult.pull` → `pullingPolicy` (ConfigurationFactory.kt:109-121,
/// 167-179). The bound `pull` itself stays in the configuration (P6 rejected).
pub fn apply_xcresult_pull(xcresult: &mut XcresultConfiguration) {
    match xcresult.pull {
        Some(true) => xcresult.pulling_policy = PullingPolicy::Always,
        Some(false) => xcresult.pulling_policy = PullingPolicy::Never,
        None => {}
    }
}

/// The iOS branch's path resolutions and xcresult rewrite
/// (`ConfigurationFactory.kt:71-131`), minus `bundle.validate()` (WP 2.3b).
pub fn post_process_ios(
    config: &mut IosConfiguration,
    context: &BindingContext,
) -> Result<(), Box<ConfigurationError>> {
    post_process_ios_with_bundle_validation(config, context, |_| Ok(()))
}

/// [`post_process_ios`] with WP 2.3b's `bundle.validate()` supplied by the
/// caller: it runs on the resolved bundle, before the media, devices and
/// SSH steps (the `canonicalFile` steps), exactly where the factory calls
/// it. A validation failure therefore precedes any `canonicalFile`
/// failure from those steps.
pub fn post_process_ios_with_bundle_validation(
    config: &mut IosConfiguration,
    context: &BindingContext,
    mut validate_bundle: impl FnMut(&mut AppleTestBundleConfiguration) -> Result<(), Box<ConfigurationError>>,
) -> Result<(), Box<ConfigurationError>> {
    let dir = &context.marathonfile_dir;
    if let Some(bundle) = &config.bundle {
        let mut resolved = resolve_bundle(bundle, dir);
        validate_bundle(&mut resolved)?;
        config.bundle = Some(resolved);
    }
    if let Some(media) = &config.media_files {
        config.media_files = Some(media.iter().map(|p| resolve(dir, p)).collect());
    }
    config.devices_file = Some(resolve_devices(config.devices_file.as_deref(), dir)?);
    config.ssh = resolve_ssh(&config.ssh, dir)?;
    apply_xcresult_pull(&mut config.xcresult);
    Ok(())
}

/// The macOS branch (`ConfigurationFactory.kt:132-188`), minus
/// `bundle.validate()` (WP 2.3b).
pub fn post_process_macos(
    config: &mut MacosConfiguration,
    context: &BindingContext,
) -> Result<(), Box<ConfigurationError>> {
    post_process_macos_with_bundle_validation(config, context, |_| Ok(()))
}

/// [`post_process_macos`] with the bundle validation callback; see
/// [`post_process_ios_with_bundle_validation`].
pub fn post_process_macos_with_bundle_validation(
    config: &mut MacosConfiguration,
    context: &BindingContext,
    mut validate_bundle: impl FnMut(&mut AppleTestBundleConfiguration) -> Result<(), Box<ConfigurationError>>,
) -> Result<(), Box<ConfigurationError>> {
    let dir = &context.marathonfile_dir;
    if let Some(bundle) = &config.bundle {
        let mut resolved = resolve_bundle(bundle, dir);
        validate_bundle(&mut resolved)?;
        config.bundle = Some(resolved);
    }
    config.devices_file = Some(resolve_devices(config.devices_file.as_deref(), dir)?);
    config.ssh = resolve_ssh(&config.ssh, dir)?;
    apply_xcresult_pull(&mut config.xcresult);
    Ok(())
}

// ---- Apple accessors (ConfigurationExtensions.kt) ------------------------------
// Inherent methods in marathon-config (WP 2.3a decision 5).

impl VendorConfiguration {
    pub fn bundle_configuration(&self) -> Option<&AppleTestBundleConfiguration> {
        match self {
            Self::Ios(config) => config.bundle.as_ref(),
            Self::Macos(config) => config.bundle.as_ref(),
            _ => None,
        }
    }
    pub fn xctestrun_env(&self) -> Option<&XctestrunEnvConfiguration> {
        match self {
            Self::Ios(config) => Some(&config.xctestrun_env),
            Self::Macos(config) => Some(&config.xctestrun_env),
            _ => None,
        }
    }
    pub fn xcresult_configuration(&self) -> Option<&XcresultConfiguration> {
        match self {
            Self::Ios(config) => Some(&config.xcresult),
            Self::Macos(config) => Some(&config.xcresult),
            _ => None,
        }
    }
    pub fn skip_copying_derived_data(&self) -> bool {
        match self {
            Self::Ios(config) => config.skip_copying_derived_data,
            _ => false,
        }
    }
}
