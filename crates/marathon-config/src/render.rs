//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/Configuration.kt (`toMap()`, with the
//! `toString()` of every configuration class it reaches),
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/yaml/RegexSerializer.kt and
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt (`serialize`).
//!
//! Two projections of the same model. `to_map` renders Kotlin `toString()`s
//! directly into result text (data classes in constructor order
//! with Kotlin property names, the two secret-masking overrides, plain
//! `object`s as simple class names (D58)); WP 2.4 P3 pins it with the JVM witnesses in
//! `fixtures/oracle/config-validation-corpus.jsonl`. `serialize` projects into
//! serde views in `yaml_views` in declaration order, with absent properties omitted.
//! The YAML reader and Kotlin text rendering have independent contracts.

use crate::{
    AnalyticsConfiguration, BatchingStrategyConfiguration, CiConfiguration, CompositionOperation, Configuration,
    Defaults, ExecutionMode, ExecutionStrategyConfiguration, FilteringConfiguration, FlakinessStrategyConfiguration,
    InfluxDb2RetentionPolicyConfiguration, OutputConfiguration, PoolingStrategyConfiguration, RegexValuesFileFilter,
    RetentionPolicyConfiguration, RetryStrategyConfiguration, ScreenRecordingPolicy, ShardingStrategyConfiguration,
    SortingStrategyConfiguration, TestFilterConfiguration, TestPackageFilter, VendorConfiguration, vendor::*,
};
use jcompat::{
    JavaRegex,
    file_manager::absolute_file_path,
    jnum::double_to_string,
    jtime::{Duration, Instant},
};
use std::path::PathBuf;

// ---- Enum names ---------------------------------------------------------------

/// The Kotlin constant name (`toString()`) and the Jackson spelling
/// (`@JsonProperty` where present, else the constant name).
pub(crate) trait Names: Copy {
    fn kotlin(self) -> &'static str;
    fn json(self) -> &'static str;
}
macro_rules! names {
    ($t:ty { $($v:ident => $kotlin:literal $(/ $json:literal)?),* $(,)? }) => {
        impl Names for $t {
            fn kotlin(self) -> &'static str {
                match self { $(Self::$v => $kotlin,)* }
            }
            fn json(self) -> &'static str {
                match self { $(Self::$v => names!(@json $kotlin $(, $json)?),)* }
            }
        }
    };
    (@json $kotlin:literal) => { $kotlin };
    (@json $kotlin:literal, $json:literal) => { $json };
}
names!(ScreenRecordingPolicy { OnFailure => "ON_FAILURE", OnAny => "ON_ANY" });
names!(ExecutionMode { AnySuccess => "ANY_SUCCESS", AllSuccess => "ALL_SUCCESS" });
names!(CompositionOperation { Union => "UNION", Intersection => "INTERSECTION", Subtract => "SUBTRACT" });
names!(PathRoot { ExternalStorage => "EXTERNAL_STORAGE", AppData => "APP_DATA", LocalTmp => "LOCAL_TMP" });
names!(AggregationMode {
    Device => "DEVICE",
    Pool => "POOL",
    DeviceAndPool => "DEVICE_AND_POOL",
    TestRun => "TEST_RUN",
});
names!(TimeUnit {
    Nanoseconds => "NANOSECONDS",
    Microseconds => "MICROSECONDS",
    Milliseconds => "MILLISECONDS",
    Seconds => "SECONDS",
    Minutes => "MINUTES",
    Hours => "HOURS",
    Days => "DAYS",
});
names!(AndroidRecorderType { Video => "VIDEO", Screenshot => "SCREENSHOT" });
names!(IosRecorderType { Video => "VIDEO", Screenshot => "SCREENSHOT" });
names!(SerialStrategy {
    Automatic => "AUTOMATIC",
    MarathonProperty => "MARATHON_PROPERTY",
    BootProperty => "BOOT_PROPERTY",
    Hostname => "HOSTNAME",
    Ddms => "DDMS",
});
names!(TestType { Xcuitest => "XCUITEST" / "xcuitest", Xctest => "XCTEST" / "xctest", LogicTest => "LOGIC_TEST" / "logic_test" });
names!(LifecycleAction { Terminate => "TERMINATE", Shutdown => "SHUTDOWN", Erase => "ERASE" });
names!(GrantLifecycle { BeforeTestRun => "BEFORE_TEST_RUN", BeforeEachBatch => "BEFORE_EACH_BATCH" });
names!(IosPermission {
    All => "All" / "all",
    Calendar => "Calendar" / "calendar",
    ContactsLimited => "ContactsLimited" / "contacts-limited",
    Contacts => "Contacts" / "contacts",
    Location => "Location" / "location",
    LocationAlways => "LocationAlways" / "location-always",
    PhotosAdd => "PhotosAdd" / "photos-add",
    Photos => "Photos" / "photos",
    MediaLibrary => "MediaLibrary" / "media-library",
    Microphone => "Microphone" / "microphone",
    Motion => "Motion" / "motion",
    Reminders => "Reminders" / "reminders",
    Siri => "Siri" / "siri",
    UserTracking => "UserTracking" / "user-tracking",
    Pasteboard => "Pasteboard" / "pasteboard",
});
names!(MacosPermission { Accessibility => "Accessibility" / "accessibility", DeveloperTool => "DeveloperTool" / "developer-tool" });
names!(ScreenshotType { Png => "PNG" / "png", Tiff => "TIFF" / "tiff", Bmp => "BMP" / "bmp", Jpeg => "JPEG" / "jpeg" });
names!(Display { Internal => "INTERNAL" / "internal", External => "EXTERNAL" / "external" });
names!(Mask { Ignored => "IGNORED" / "ignored", Black => "BLACK" / "black" });
names!(Codec { H264 => "H264" / "h264", Hevc => "HEVC" / "hevc" });
names!(TranscodingSize { Hd480 => "hd480", Hd720 => "hd720", Hd1080 => "hd1080" });
names!(Lifetime { KeepAlways => "KEEP_ALWAYS", DeleteOnSuccess => "DELETE_ON_SUCCESS", KeepNever => "KEEP_NEVER" });
names!(PullingPolicy { Never => "NEVER", Always => "ALWAYS", OnFailure => "ON_FAILURE" });
names!(ScreenCaptureFormat { Screenshots => "SCREENSHOTS", ScreenRecording => "SCREEN_RECORDING" });

// ---- toMap() ------------------------------------------------------------------

impl Configuration {
    /// `Configuration.toMap()` (Configuration.kt:55-80): 23 entries in
    /// `mapOf` order. `analyticsTracking`, `bugsnagReporting` and
    /// `uncompletedTestRetryQuota` are not in it. `outputDir` is
    /// `File.absolutePath`, resolved against `process_dir` (the JVM's
    /// `user.dir`) when it is relative; every other value is the field's
    /// Kotlin `toString()`.
    pub fn to_map(&self, process_dir: &std::path::Path) -> Vec<(String, String)> {
        let entries = [
            ("name", text(&self.name)),
            (
                "outputDir",
                absolute_file_path(&self.output_dir.to_string_lossy(), &process_dir.to_string_lossy())
                    .to_string_lossy()
                    .into_owned(),
            ),
            ("outputConfiguration", self.output_configuration.result_text()),
            ("analyticsConfiguration", self.analytics_configuration.result_text()),
            ("pooling", self.pooling_strategy.result_text()),
            ("sharding", self.sharding_strategy.result_text()),
            ("sorting", self.sorting_strategy.result_text()),
            ("batching", self.batching_strategy.result_text()),
            ("flakiness", self.flakiness_strategy.result_text()),
            ("retry", self.retry_strategy.result_text()),
            ("filtering", self.filtering_configuration.result_text()),
            ("ignoreFailures", boolean(self.ignore_failures)),
            ("isCodeCoverageEnabled", boolean(self.is_code_coverage_enabled)),
            ("executionStrategy", self.execution_strategy.result_text()),
            ("includeSerialRegexes", list(&self.include_serial_regexes, regex)),
            ("excludeSerialRegexes", list(&self.exclude_serial_regexes, regex)),
            ("testBatchTimeoutMillis", number(self.test_batch_timeout_millis)),
            ("testOutputTimeoutMillis", number(self.test_output_timeout_millis)),
            ("debug", boolean(self.debug)),
            ("screenRecordingPolicy", enumeration(self.screen_recording_policy)),
            ("vendorConfiguration", self.vendor_configuration.result_text()),
            (
                "deviceInitializationTimeoutMillis",
                number(self.device_initialization_timeout_millis),
            ),
            ("ciConfiguration", self.ci_configuration.result_text()),
        ];
        entries.into_iter().map(|(key, value)| (key.into(), value)).collect()
    }
}

/// The pinned text of a configuration value stored in Allure environment data.
trait ResultText {
    fn result_text(&self) -> String;
}

fn data(name: &str, fields: Vec<(&str, String)>) -> String {
    let fields = fields
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}({fields})")
}
fn text(s: &str) -> String {
    s.to_owned()
}
fn quoted(s: &str) -> String {
    format!("'{s}'")
}
fn path(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}
fn regex(r: &JavaRegex) -> String {
    r.pattern().into()
}
fn boolean(b: bool) -> String {
    b.to_string()
}
fn number(n: impl ToString) -> String {
    n.to_string()
}
fn double(d: f64) -> String {
    double_to_string(d)
}
fn duration(d: &Duration) -> String {
    number(d)
}
fn instant(i: &Instant) -> String {
    number(i)
}
fn enumeration(e: impl Names) -> String {
    e.kotlin().into()
}
fn opt<T>(v: Option<&T>, f: impl FnOnce(&T) -> String) -> String {
    v.map_or_else(|| "null".into(), f)
}
fn list<T>(v: &[T], f: impl FnMut(&T) -> String) -> String {
    format!("[{}]", v.iter().map(f).collect::<Vec<_>>().join(", "))
}
fn map(entries: Vec<(String, String)>) -> String {
    format!(
        "{{{}}}",
        entries
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
fn string_map(v: &[(String, String)]) -> String {
    map(v.to_vec())
}

impl ResultText for OutputConfiguration {
    fn result_text(&self) -> String {
        data(
            "OutputConfiguration",
            vec![
                ("maxPath", number(self.max_path)),
                ("maxFilename", number(self.max_filename)),
            ],
        )
    }
}
impl ResultText for Defaults {
    fn result_text(&self) -> String {
        data(
            "Defaults",
            vec![
                ("successRate", double(self.success_rate)),
                ("duration", duration(&self.duration)),
                (
                    "observedDurationMinTests",
                    opt(self.observed_duration_min_tests.as_ref(), |n| number(n)),
                ),
            ],
        )
    }
}
impl ResultText for RetentionPolicyConfiguration {
    fn result_text(&self) -> String {
        data(
            "RetentionPolicyConfiguration",
            vec![
                ("name", text(&self.name)),
                ("duration", text(&self.duration)),
                ("shardDuration", text(&self.shard_duration)),
                ("replicationFactor", number(self.replication_factor)),
                ("isDefault", boolean(self.is_default)),
            ],
        )
    }
}
impl ResultText for InfluxDb2RetentionPolicyConfiguration {
    fn result_text(&self) -> String {
        data(
            "RetentionPolicyConfiguration",
            vec![
                ("everySeconds", number(self.every_seconds)),
                ("shardGroupDurationSeconds", number(self.shard_group_duration_seconds)),
            ],
        )
    }
}
impl ResultText for AnalyticsConfiguration {
    fn result_text(&self) -> String {
        let masked = || "'*****'".to_owned();
        match self {
            Self::Disabled => text("DisabledAnalytics"),
            // The two `toString()` overrides hide credentials and omit
            // `defaults`/`readOnly` (AnalyticsConfiguration.kt:44-46,67-69).
            Self::InfluxDb(c) => data(
                "InfluxDbConfiguration",
                vec![
                    ("url", masked()),
                    ("user", masked()),
                    ("password", masked()),
                    ("dbName", quoted(&c.db_name)),
                    (
                        "retentionPolicyConfiguration",
                        c.retention_policy_configuration.result_text(),
                    ),
                ],
            ),
            Self::InfluxDb2(c) => data(
                "InfluxDb2Configuration",
                vec![
                    ("url", masked()),
                    ("token", masked()),
                    ("organization", quoted(&c.organization)),
                    ("bucket", quoted(&c.bucket)),
                    (
                        "retentionPolicyConfiguration",
                        c.retention_policy_configuration.result_text(),
                    ),
                ],
            ),
            Self::Graphite(c) => data(
                "GraphiteConfiguration",
                vec![
                    ("host", text(&c.host)),
                    ("port", opt(c.port.as_ref(), |n| number(n))),
                    ("prefix", opt(c.prefix.as_ref(), |s| text(s))),
                    ("defaults", c.defaults.result_text()),
                    ("readOnly", boolean(c.read_only)),
                ],
            ),
        }
    }
}

impl ResultText for PoolingStrategyConfiguration {
    fn result_text(&self) -> String {
        let simple = match self {
            Self::Omni => "OmniPoolingStrategyConfiguration",
            Self::Abi => "AbiPoolingStrategyConfiguration",
            Self::Manufacturer => "ManufacturerPoolingStrategyConfiguration",
            Self::Model => "ModelPoolingStrategyConfiguration",
            Self::OperatingSystemVersion => "OperatingSystemVersionPoolingStrategyConfiguration",
            Self::Combo(items) => {
                return data(
                    "ComboPoolingStrategyConfiguration",
                    vec![("list", list(items, Self::result_text))],
                );
            }
        };
        text(simple)
    }
}
impl ResultText for ShardingStrategyConfiguration {
    fn result_text(&self) -> String {
        match self {
            Self::Count { count } => data("CountShardingStrategyConfiguration", vec![("count", number(count))]),
            Self::Parallel => text("ParallelShardingStrategyConfiguration"),
        }
    }
}
impl ResultText for SortingStrategyConfiguration {
    fn result_text(&self) -> String {
        match self {
            Self::ExecutionTime { percentile, time_limit } => data(
                "ExecutionTimeSortingStrategyConfiguration",
                vec![("percentile", double(*percentile)), ("timeLimit", instant(time_limit))],
            ),
            Self::NoSorting => text("NoSortingStrategyConfiguration"),
            Self::RandomOrder => text("RandomOrderStrategyConfiguration"),
            Self::SuccessRate { time_limit, ascending } => data(
                "SuccessRateSortingStrategyConfiguration",
                vec![("timeLimit", instant(time_limit)), ("ascending", boolean(*ascending))],
            ),
        }
    }
}
impl ResultText for BatchingStrategyConfiguration {
    fn result_text(&self) -> String {
        match self {
            Self::FixedSize {
                size,
                duration_millis,
                percentile,
                time_limit,
                last_mile_length,
            } => data(
                "FixedSizeBatchingStrategyConfiguration",
                vec![
                    ("size", number(size)),
                    ("durationMillis", opt(duration_millis.as_ref(), |n| number(n))),
                    ("percentile", opt(percentile.as_ref(), |p| double(*p))),
                    ("timeLimit", opt(time_limit.as_ref(), instant)),
                    ("lastMileLength", number(last_mile_length)),
                ],
            ),
            Self::ClassName => text("ClassNameBatchingStrategyConfiguration"),
            Self::Isolate => text("IsolateBatchingStrategyConfiguration"),
        }
    }
}
impl ResultText for FlakinessStrategyConfiguration {
    fn result_text(&self) -> String {
        match self {
            Self::Ignore => text("IgnoreFlakinessStrategyConfiguration"),
            Self::Probability {
                min_success_rate,
                max_count,
                time_limit,
            } => data(
                "ProbabilityBasedFlakinessStrategyConfiguration",
                vec![
                    ("minSuccessRate", double(*min_success_rate)),
                    ("maxCount", number(max_count)),
                    ("timeLimit", instant(time_limit)),
                ],
            ),
        }
    }
}
impl ResultText for RetryStrategyConfiguration {
    fn result_text(&self) -> String {
        match self {
            Self::FixedQuota {
                total_allowed_retry_quota,
                retry_per_test_quota,
            } => data(
                "FixedQuotaRetryStrategyConfiguration",
                vec![
                    ("totalAllowedRetryQuota", number(total_allowed_retry_quota)),
                    ("retryPerTestQuota", number(retry_per_test_quota)),
                ],
            ),
            Self::NoRetry => text("NoRetryStrategyConfiguration"),
        }
    }
}
impl ResultText for ExecutionStrategyConfiguration {
    fn result_text(&self) -> String {
        data(
            "ExecutionStrategyConfiguration",
            vec![("mode", enumeration(self.mode)), ("fast", boolean(self.fast))],
        )
    }
}
impl ResultText for FilteringConfiguration {
    fn result_text(&self) -> String {
        data(
            "FilteringConfiguration",
            vec![
                ("allowlist", list(&self.allowlist, ResultText::result_text)),
                ("blocklist", list(&self.blocklist, ResultText::result_text)),
            ],
        )
    }
}
fn values_filter(name: &str, f: &RegexValuesFileFilter) -> String {
    // Null `values` elements were dropped at bind (D9), so they cannot
    // show here, where Kotlin prints `null`.
    data(
        name,
        vec![
            ("regex", opt(f.regex.as_ref(), regex)),
            ("values", opt(f.values.as_ref(), |v| list(v, |s| text(s)))),
            ("file", opt(f.file.as_ref(), |p| path(p))),
            ("enabled", boolean(f.enabled)),
        ],
    )
}
impl ResultText for TestFilterConfiguration {
    fn result_text(&self) -> String {
        match self {
            Self::SimpleClassname(f) => values_filter("SimpleClassnameFilterConfiguration", f),
            Self::SimpleTestname(f) => values_filter("SimpleTestnameFilterConfiguration", f),
            Self::FullyQualifiedClassname(f) => values_filter("FullyQualifiedClassnameFilterConfiguration", f),
            Self::FullyQualifiedTestname(f) => values_filter("FullyQualifiedTestnameFilterConfiguration", f),
            Self::TestMethod(f) => values_filter("TestMethodFilterConfiguration", f),
            Self::Annotation(f) => values_filter("AnnotationFilterConfiguration", f),
            Self::TestPackage(TestPackageFilter {
                regex: r,
                values,
                file,
                subpackages,
                enabled,
            }) => data(
                "TestPackageFilterConfiguration",
                vec![
                    ("regex", opt(r.as_ref(), regex)),
                    ("values", opt(values.as_ref(), |v| list(v, |s| text(s)))),
                    ("file", opt(file.as_ref(), |p| path(p))),
                    ("subpackages", boolean(*subpackages)),
                    ("enabled", boolean(*enabled)),
                ],
            ),
            Self::AnnotationData {
                name_regex,
                value_regex,
                enabled,
            } => data(
                "AnnotationDataFilterConfiguration",
                vec![
                    ("nameRegex", regex(name_regex)),
                    ("valueRegex", regex(value_regex)),
                    ("enabled", boolean(*enabled)),
                ],
            ),
            Self::Fragmentation { index, count, enabled } => data(
                "FragmentationFilterConfiguration",
                vec![
                    ("index", number(index)),
                    ("count", number(count)),
                    ("enabled", boolean(*enabled)),
                ],
            ),
            Self::Composition { filters, op, enabled } => data(
                "CompositionFilterConfiguration",
                vec![
                    ("filters", list(filters, Self::result_text)),
                    ("op", enumeration(*op)),
                    ("enabled", boolean(*enabled)),
                ],
            ),
            Self::Allure { enabled } => data("AllureFilterConfiguration", vec![("enabled", boolean(*enabled))]),
        }
    }
}
impl ResultText for CiConfiguration {
    fn result_text(&self) -> String {
        text(match self {
            Self::Auto => "Auto",
            Self::None => "None",
            Self::Teamcity => "Teamcity",
        })
    }
}

impl ResultText for VendorConfiguration {
    fn result_text(&self) -> String {
        match self {
            Self::Android(c) => c.result_text(),
            Self::Ios(c) => c.result_text(),
            Self::Macos(c) => c.result_text(),
            Self::Stub => text("StubVendorConfiguration"),
            Self::Empty => text("EmptyVendorConfiguration"),
        }
    }
}
fn paths(v: Option<&Vec<PathBuf>>) -> String {
    opt(v, |v| list(v, |p| path(p)))
}
impl ResultText for AndroidConfiguration {
    fn result_text(&self) -> String {
        data(
            "AndroidConfiguration",
            vec![
                ("androidSdk", opt(self.android_sdk.as_ref(), |p| path(p))),
                ("applicationOutput", opt(self.application_output.as_ref(), |p| path(p))),
                (
                    "testApplicationOutput",
                    opt(self.test_application_output.as_ref(), |p| path(p)),
                ),
                ("splitApks", paths(self.split_apks.as_ref())),
                (
                    "extraApplicationsOutput",
                    paths(self.extra_applications_output.as_ref()),
                ),
                (
                    "outputs",
                    opt(self.outputs.as_ref(), |v| list(v, ResultText::result_text)),
                ),
                ("autoGrantPermission", boolean(self.auto_grant_permission)),
                ("instrumentationArgs", string_map(&self.instrumentation_args)),
                ("applicationPmClear", boolean(self.application_pm_clear)),
                ("testApplicationPmClear", boolean(self.test_application_pm_clear)),
                ("adbInitTimeoutMillis", number(self.adb_init_timeout_millis)),
                ("installOptions", text(&self.install_options)),
                ("serialStrategy", enumeration(self.serial_strategy)),
                (
                    "screenRecordConfiguration",
                    self.screen_record_configuration.result_text(),
                ),
                (
                    "waitForDevicesTimeoutMillis",
                    number(self.wait_for_devices_timeout_millis),
                ),
                ("allureConfiguration", self.allure_configuration.result_text()),
                ("timeoutConfiguration", self.timeout_configuration.result_text()),
                ("fileSyncConfiguration", self.file_sync_configuration.result_text()),
                ("threadingConfiguration", self.threading_configuration.result_text()),
                ("testParserConfiguration", self.test_parser_configuration.result_text()),
                ("testAccessConfiguration", self.test_access_configuration.result_text()),
                ("adbServers", list(&self.adb_servers, ResultText::result_text)),
                ("disableWindowAnimation", boolean(self.disable_window_animation)),
                ("profilingConfiguration", self.profiling_configuration.result_text()),
                ("mockLocation", boolean(self.mock_location)),
            ],
        )
    }
}
impl ResultText for AndroidTestBundleConfiguration {
    fn result_text(&self) -> String {
        data(
            "AndroidTestBundleConfiguration",
            vec![
                ("application", opt(self.application.as_ref(), |p| path(p))),
                ("testApplication", path(&self.test_application)),
                ("extraApplications", paths(self.extra_applications.as_ref())),
                ("splitApks", paths(self.split_apks.as_ref())),
            ],
        )
    }
}
impl ResultText for AndroidScreenRecordConfiguration {
    fn result_text(&self) -> String {
        let video = &self.video_configuration;
        let screenshot = &self.screenshot_configuration;
        data(
            "ScreenRecordConfiguration",
            vec![
                (
                    "preferableRecorderType",
                    opt(self.preferable_recorder_type.as_ref(), |t| enumeration(*t)),
                ),
                (
                    "videoConfiguration",
                    data(
                        "VideoConfiguration",
                        vec![
                            ("enabled", boolean(video.enabled)),
                            ("width", number(video.width)),
                            ("height", number(video.height)),
                            ("bitrateMbps", number(video.bitrate_mbps)),
                            ("timeLimit", number(video.time_limit)),
                            ("timeLimitUnits", enumeration(video.time_limit_units)),
                        ],
                    ),
                ),
                (
                    "screenshotConfiguration",
                    data(
                        "ScreenshotConfiguration",
                        vec![
                            ("enabled", boolean(screenshot.enabled)),
                            ("width", number(screenshot.width)),
                            ("height", number(screenshot.height)),
                            ("delayMs", number(screenshot.delay_ms)),
                        ],
                    ),
                ),
            ],
        )
    }
}
impl ResultText for AllureConfiguration {
    fn result_text(&self) -> String {
        data(
            "AllureConfiguration",
            vec![
                ("enabled", boolean(self.enabled)),
                ("relativeResultsDirectory", text(&self.relative_results_directory)),
                ("pathRoot", enumeration(self.path_root)),
            ],
        )
    }
}
impl ResultText for AndroidTimeoutConfiguration {
    fn result_text(&self) -> String {
        data(
            "TimeoutConfiguration",
            vec![
                ("shell", duration(&self.shell)),
                ("listFiles", duration(&self.list_files)),
                ("pushFile", duration(&self.push_file)),
                ("pushFolder", duration(&self.push_folder)),
                ("pullFile", duration(&self.pull_file)),
                ("uninstall", duration(&self.uninstall)),
                ("install", duration(&self.install)),
                ("screenrecorder", duration(&self.screenrecorder)),
                ("screencapturer", duration(&self.screencapturer)),
                ("socketIdleTimeout", duration(&self.socket_idle_timeout)),
                ("portForward", duration(&self.port_forward)),
                ("boot", duration(&self.boot)),
            ],
        )
    }
}
impl ResultText for FileSyncConfiguration {
    fn result_text(&self) -> String {
        let pull = |e: &FileSyncEntry| {
            data(
                "FileSyncEntry",
                vec![
                    ("relativePath", text(&e.relative_path)),
                    ("pathRoot", enumeration(e.path_root)),
                    ("aggregationMode", enumeration(e.aggregation_mode)),
                ],
            )
        };
        let push = |e: &FilePushEntry| {
            data(
                "FilePushEntry",
                vec![("path", text(&e.path)), ("pathRoot", enumeration(e.path_root))],
            )
        };
        data(
            "FileSyncConfiguration",
            vec![("pull", list(&self.pull, pull)), ("push", list(&self.push, push))],
        )
    }
}
impl ResultText for AndroidThreadingConfiguration {
    fn result_text(&self) -> String {
        data(
            "ThreadingConfiguration",
            vec![
                ("bootWaitingThreads", number(self.boot_waiting_threads)),
                ("adbIoThreads", number(self.adb_io_threads)),
                ("installThreads", number(self.install_threads)),
            ],
        )
    }
}
impl ResultText for AndroidTestParserConfiguration {
    fn result_text(&self) -> String {
        match self {
            Self::Local => text("LocalTestParserConfiguration"),
            Self::Remote { instrumentation_args } => data(
                "RemoteTestParserConfiguration",
                vec![("instrumentationArgs", string_map(instrumentation_args))],
            ),
        }
    }
}
impl ResultText for TestAccessConfiguration {
    fn result_text(&self) -> String {
        data(
            "TestAccessConfiguration",
            vec![
                ("adb", boolean(self.adb)),
                ("grpc", boolean(self.grpc)),
                ("console", boolean(self.console)),
                ("consoleToken", text(&self.console_token)),
            ],
        )
    }
}
impl ResultText for AdbEndpoint {
    fn result_text(&self) -> String {
        data(
            "AdbEndpoint",
            vec![("host", text(&self.host)), ("port", number(self.port))],
        )
    }
}
impl ResultText for ProfilingConfiguration {
    fn result_text(&self) -> String {
        data(
            "ProfilingConfiguration",
            vec![
                ("enabled", boolean(self.enabled)),
                ("pbtxt", opt(self.pbtxt.as_ref(), |p| path(p))),
                ("urlTemplate", text(&self.url_template)),
            ],
        )
    }
}

impl ResultText for IosConfiguration {
    fn result_text(&self) -> String {
        data(
            "IOSConfiguration",
            vec![
                ("bundle", opt(self.bundle.as_ref(), ResultText::result_text)),
                ("deviceProvider", self.device_provider.result_text()),
                ("devicesFile", opt(self.devices_file.as_ref(), |p| path(p))),
                ("ssh", self.ssh.result_text()),
                ("workerConnection", self.worker_connection.result_text()),
                ("xcresult", self.xcresult.result_text()),
                (
                    "screenRecordConfiguration",
                    self.screen_record_configuration.result_text(),
                ),
                ("xctestrunEnv", self.xctestrun_env.result_text()),
                ("lifecycleConfiguration", self.lifecycle_configuration.result_text()),
                ("permissions", self.permissions.result_text()),
                ("mediaFiles", paths(self.media_files.as_ref())),
                ("timeoutConfiguration", self.timeout_configuration.result_text()),
                ("threadingConfiguration", self.threading_configuration.result_text()),
                ("hideRunnerOutput", boolean(self.hide_runner_output)),
                ("compactOutput", boolean(self.compact_output)),
                ("rsync", self.rsync.result_text()),
                ("xcodebuildTestArgs", string_map(&self.xcodebuild_test_args)),
                ("dataContainerClear", boolean(self.data_container_clear)),
                ("testParserConfiguration", self.test_parser_configuration.result_text()),
                ("deviceLog", boolean(self.device_log)),
                (
                    "signing",
                    data(
                        "SigningConfiguration",
                        vec![(
                            "testRunnerProvisioningProfile",
                            opt(self.signing.test_runner_provisioning_profile.as_ref(), |s| text(s)),
                        )],
                    ),
                ),
                ("skipCopyingDerivedData", boolean(self.skip_copying_derived_data)),
                ("maxBatchesWithoutProgress", number(self.max_batches_without_progress)),
            ],
        )
    }
}
impl ResultText for MacosConfiguration {
    fn result_text(&self) -> String {
        data(
            "MacosConfiguration",
            vec![
                ("bundle", opt(self.bundle.as_ref(), ResultText::result_text)),
                ("deviceProvider", self.device_provider.result_text()),
                ("devicesFile", opt(self.devices_file.as_ref(), |p| path(p))),
                ("ssh", self.ssh.result_text()),
                ("xcresult", self.xcresult.result_text()),
                ("xctestrunEnv", self.xctestrun_env.result_text()),
                ("timeoutConfiguration", self.timeout_configuration.result_text()),
                ("threadingConfiguration", self.threading_configuration.result_text()),
                ("hideRunnerOutput", boolean(self.hide_runner_output)),
                ("compactOutput", boolean(self.compact_output)),
                ("rsync", self.rsync.result_text()),
                ("xcodebuildTestArgs", string_map(&self.xcodebuild_test_args)),
                ("testParserConfiguration", self.test_parser_configuration.result_text()),
                (
                    "permissions",
                    data(
                        "PermissionsConfiguration",
                        vec![(
                            "grant",
                            map(self
                                .permissions
                                .grant
                                .iter()
                                .map(|(k, v)| (k.as_str().into(), list(v, |p| enumeration(*p))))
                                .collect()),
                        )],
                    ),
                ),
            ],
        )
    }
}
impl ResultText for AppleTestBundleConfiguration {
    fn result_text(&self) -> String {
        // `app`/`testApp`/`xctest` are body properties, outside the data
        // class `toString()`; the constructor's `tempDirFor` lambda is in it.
        data(
            "AppleTestBundleConfiguration",
            vec![
                ("application", opt(self.application.as_ref(), |p| path(p))),
                ("testApplication", opt(self.test_application.as_ref(), |p| path(p))),
                ("extraApplications", paths(self.extra_applications.as_ref())),
                ("derivedDataDir", opt(self.derived_data_dir.as_ref(), |p| path(p))),
                ("testType", opt(self.test_type.as_ref(), |t| enumeration(*t))),
                ("xctestrun", opt(self.xctestrun.as_ref(), |p| path(p))),
                ("tempDirFor", "(java.io.File) -> java.io.File".into()),
            ],
        )
    }
}
impl ResultText for DeviceProvider {
    fn result_text(&self) -> String {
        match self {
            Self::Static { path: p } => data("Static", vec![("path", opt(p.as_ref(), |p| path(p)))]),
            Self::Dynamic { host, port } => data("Dynamic", vec![("host", text(host)), ("port", number(port))]),
        }
    }
}
impl ResultText for SshConfiguration {
    fn result_text(&self) -> String {
        let authentication = self.authentication.as_ref().map_or_else(
            || "null".into(),
            |a| match a {
                // `PasswordAuthentication.toString()` hides the password (SshConfiguration.kt:58-60).
                SshAuthentication::Password { username, .. } => {
                    data("PasswordAuthentication", vec![("username", quoted(username))])
                }
                SshAuthentication::PublicKey { username, key } => data(
                    "PublicKeyAuthentication",
                    vec![("username", text(username)), ("key", path(key))],
                ),
            },
        );
        data(
            "SshConfiguration",
            vec![
                ("authentication", authentication),
                ("knownHostsPath", opt(self.known_hosts_path.as_ref(), |p| path(p))),
                ("keepAliveInterval", duration(&self.keep_alive_interval)),
                ("keepAliveMaxCount", number(self.keep_alive_max_count)),
                ("debug", boolean(self.debug)),
                ("shareWorkerConnection", boolean(self.share_worker_connection)),
            ],
        )
    }
}
impl ResultText for WorkerConnectionConfiguration {
    fn result_text(&self) -> String {
        data(
            "WorkerConnectionConfiguration",
            vec![
                ("attempts", number(self.attempts)),
                ("backoff", duration(&self.backoff)),
                ("retryInterval", duration(&self.retry_interval)),
                ("lateJoin", boolean(self.late_join)),
                ("lateJoinCutoff", duration(&self.late_join_cutoff)),
            ],
        )
    }
}
impl ResultText for XcresultConfiguration {
    fn result_text(&self) -> String {
        data(
            "XcresultConfiguration",
            vec![
                ("pull", opt(self.pull.as_ref(), |b| boolean(*b))),
                ("pullingPolicy", enumeration(self.pulling_policy)),
                ("remoteClean", boolean(self.remote_clean)),
                (
                    "attachments",
                    data(
                        "AttachmentsConfiguration",
                        vec![
                            (
                                "systemAttachmentLifetime",
                                enumeration(self.attachments.system_attachment_lifetime),
                            ),
                            (
                                "userAttachmentLifetime",
                                enumeration(self.attachments.user_attachment_lifetime),
                            ),
                        ],
                    ),
                ),
                (
                    "preferredScreenCaptureFormat",
                    opt(self.preferred_screen_capture_format.as_ref(), |f| enumeration(*f)),
                ),
                (
                    "queue",
                    data(
                        "XcresultQueueConfiguration",
                        vec![
                            ("capacity", number(self.queue.capacity)),
                            ("concurrency", number(self.queue.concurrency)),
                            (
                                "workerCapacity",
                                opt(self.queue.worker_capacity.as_ref(), |n| number(n)),
                            ),
                        ],
                    ),
                ),
            ],
        )
    }
}
impl ResultText for IosScreenRecordConfiguration {
    fn result_text(&self) -> String {
        let video = &self.video_configuration;
        let screenshot = &self.screenshot_configuration;
        data(
            "ScreenRecordConfiguration",
            vec![
                (
                    "preferableRecorderType",
                    opt(self.preferable_recorder_type.as_ref(), |t| enumeration(*t)),
                ),
                (
                    "videoConfiguration",
                    data(
                        "VideoConfiguration",
                        vec![
                            ("enabled", boolean(video.enabled)),
                            ("codec", enumeration(video.codec)),
                            ("display", enumeration(video.display)),
                            ("mask", enumeration(video.mask)),
                            (
                                "transcoding",
                                data(
                                    "TranscodingConfiguration",
                                    vec![
                                        ("enabled", boolean(video.transcoding.enabled)),
                                        ("binary", text(&video.transcoding.binary)),
                                        ("size", enumeration(video.transcoding.size)),
                                    ],
                                ),
                            ),
                        ],
                    ),
                ),
                (
                    "screenshotConfiguration",
                    data(
                        "ScreenshotConfiguration",
                        vec![
                            ("enabled", boolean(screenshot.enabled)),
                            ("type", enumeration(screenshot.screenshot_type)),
                            ("display", enumeration(screenshot.display)),
                            ("mask", enumeration(screenshot.mask)),
                            ("delay", duration(&screenshot.delay)),
                            ("width", number(screenshot.width)),
                            ("height", number(screenshot.height)),
                        ],
                    ),
                ),
            ],
        )
    }
}
impl ResultText for XctestrunEnvConfiguration {
    fn result_text(&self) -> String {
        data(
            "XctestrunEnvConfiguration",
            vec![
                ("appEnvs", string_map(&self.app_envs)),
                ("testEnvs", string_map(&self.test_envs)),
            ],
        )
    }
}
impl ResultText for LifecycleConfiguration {
    fn result_text(&self) -> String {
        // Kotlin sets are HashSets with no defined order; ours keep
        // first-seen order (WP 2.3a P7).
        data(
            "LifecycleConfiguration",
            vec![
                ("shutdownUnused", boolean(self.shutdown_unused)),
                ("onPrepare", list(&self.on_prepare, |a| enumeration(*a))),
                ("onDispose", list(&self.on_dispose, |a| enumeration(*a))),
            ],
        )
    }
}
impl ResultText for IosPermissionsConfiguration {
    fn result_text(&self) -> String {
        data(
            "PermissionsConfiguration",
            vec![
                ("grant", list(&self.grant, |p| enumeration(*p))),
                ("lifecycle", enumeration(self.lifecycle)),
            ],
        )
    }
}
impl ResultText for AppleTimeoutConfiguration {
    fn result_text(&self) -> String {
        data(
            "TimeoutConfiguration",
            vec![
                ("shell", duration(&self.shell)),
                ("shellIdle", duration(&self.shell_idle)),
                ("reachability", duration(&self.reachability)),
                ("screenshot", duration(&self.screenshot)),
                ("video", duration(&self.video)),
                ("erase", duration(&self.erase)),
                ("shutdown", duration(&self.shutdown)),
                ("delete", duration(&self.delete)),
                ("create", duration(&self.create)),
                ("boot", duration(&self.boot)),
                ("install", duration(&self.install)),
                ("uninstall", duration(&self.uninstall)),
                ("importMedia", duration(&self.import_media)),
                ("testDestination", duration(&self.test_destination)),
                ("xcresulttool", duration(&self.xcresulttool)),
                ("bootWait", duration(&self.boot_wait)),
            ],
        )
    }
}
impl ResultText for AppleThreadingConfiguration {
    fn result_text(&self) -> String {
        data(
            "ThreadingConfiguration",
            vec![
                ("deviceProviderThreads", number(self.device_provider_threads)),
                ("deviceThreads", number(self.device_threads)),
                ("provisioningConcurrency", number(self.provisioning_concurrency)),
            ],
        )
    }
}
impl ResultText for RsyncConfiguration {
    fn result_text(&self) -> String {
        data("RsyncConfiguration", vec![("remotePath", text(&self.remote_path))])
    }
}
impl ResultText for AppleTestParserConfiguration {
    fn result_text(&self) -> String {
        match self {
            Self::Nm { test_class_regexes } => data(
                "NmTestParserConfiguration",
                vec![("testClassRegexes", list(test_class_regexes, |r| r.pattern().into()))],
            ),
            Self::Xctest => text("XCTestParserConfiguration"),
        }
    }
}

/// The same declaration-order view also supports standalone configuration sections.
pub fn serialize<T>(configuration: &T) -> Result<String, crate::yaml::Error>
where
    for<'a> crate::yaml_views::View<'a, T>: serde::Serialize,
{
    crate::yaml::to_string(&crate::yaml_views::View(configuration))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InfluxDb2Configuration;
    use serde_json::Value;

    // Moved from jcompat's generic writer models: exercise the actual result
    // projection, retaining the historical configuration/masking expectations.
    #[test]
    fn recorded_configuration_text_and_masking_stay_exact() -> Result<(), Box<dyn std::error::Error>> {
        let rows = include_str!("../../../fixtures/oracle/jcompat/kotlin_to_string.jsonl")
            .lines()
            .skip(1)
            .map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()?;
        let row = |id: &str| &rows.iter().find(|r| r["id"] == id).unwrap()["toString"];
        let output = OutputConfiguration {
            max_path: 0,
            max_filename: 255,
        };
        for id in ["toMap-default", "toMap-full"] {
            assert_eq!(output.result_text(), row(id)["outputConfiguration"].as_str().unwrap());
        }
        let analytics = AnalyticsConfiguration::InfluxDb2(InfluxDb2Configuration {
            url: "u".into(),
            token: "t".into(),
            organization: "o".into(),
            bucket: "b".into(),
            retention_policy_configuration: InfluxDb2RetentionPolicyConfiguration::default_policy(),
            defaults: Defaults {
                success_rate: 0.0,
                duration: jcompat::jtime::duration_of(300, 0).unwrap(),
                observed_duration_min_tests: None,
            },
            read_only: false,
        });
        assert_eq!(analytics.result_text(), row("Duration-Instant-File").as_str().unwrap());
        let historical = row("object").as_str().unwrap();
        assert_eq!(
            SortingStrategyConfiguration::NoSorting.result_text(),
            historical.rsplit('$').next().unwrap().split('@').next().unwrap()
        );
        let ssh = SshConfiguration {
            authentication: Some(SshAuthentication::Password {
                username: "runner".into(),
                password: "secret-do-not-print".into(),
            }),
            known_hosts_path: None,
            keep_alive_interval: jcompat::jtime::duration_of(1, 0).unwrap(),
            keep_alive_max_count: 4,
            debug: false,
            share_worker_connection: true,
        };
        assert_eq!(
            ssh.result_text(),
            "SshConfiguration(authentication=PasswordAuthentication(username='runner'), knownHostsPath=null, keepAliveInterval=PT1S, keepAliveMaxCount=4, debug=false, shareWorkerConnection=true)"
        );
        Ok(())
    }
}
