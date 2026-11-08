//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/Configuration.kt (`toMap()`, with the
//! `toString()` of every configuration class it reaches),
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/yaml/RegexSerializer.kt and
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt (`serialize`).
//!
//! Two projections of the same model. `to_map` renders Kotlin `toString()`s
//! through `jcompat::writers::KotlinValue` (data classes in constructor order
//! with Kotlin property names, the two secret-masking overrides, plain
//! `object`s as `Class@hash`); WP 2.4 P3 pins it with the JVM witnesses in
//! `fixtures/oracle/config-validation-corpus.jsonl`. `serialize` projects into
//! `WriterValue` for 1.5's `JacksonYamlWriter` in Jackson's order and names
//! (`type` first, constructor order, `@JsonProperty` names, nulls omitted as
//! `NON_NULL` does, `Regex` as its pattern). WP 2.4 P1: it only has to round
//! trip through the loader; it is not held to Jackson's exact text.

use crate::{
    AnalyticsConfiguration, BatchingStrategyConfiguration, CiConfiguration, CompositionOperation, Configuration,
    Defaults, ExecutionMode, ExecutionStrategyConfiguration, FilteringConfiguration, FlakinessStrategyConfiguration,
    InfluxDb2RetentionPolicyConfiguration, OutputConfiguration, PoolingStrategyConfiguration, RegexValuesFileFilter,
    RetentionPolicyConfiguration, RetryStrategyConfiguration, ScreenRecordingPolicy, ShardingStrategyConfiguration,
    SortingStrategyConfiguration, TestFilterConfiguration, TestPackageFilter, VendorConfiguration, vendor::*,
};
use jcompat::{
    JavaRegex,
    file_manager::{JavaPath, absolute_file_path_utf16},
    jnum::double_to_string,
    jtime::{Duration, Instant},
    writers::{JacksonYamlWriter, JavaNumber, KotlinValue, WriterValue},
};

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
    pub fn to_map(&self, process_dir: &JavaPath) -> Vec<(String, String)> {
        let entries = [
            ("name", text(&self.name)),
            (
                "outputDir",
                KotlinValue::Text(String::from_utf16_lossy(
                    &absolute_file_path_utf16(&self.output_dir.0, &process_dir.0).0,
                )),
            ),
            ("outputConfiguration", self.output_configuration.kotlin()),
            ("analyticsConfiguration", self.analytics_configuration.kotlin()),
            ("pooling", self.pooling_strategy.kotlin()),
            ("sharding", self.sharding_strategy.kotlin()),
            ("sorting", self.sorting_strategy.kotlin()),
            ("batching", self.batching_strategy.kotlin()),
            ("flakiness", self.flakiness_strategy.kotlin()),
            ("retry", self.retry_strategy.kotlin()),
            ("filtering", self.filtering_configuration.kotlin()),
            ("ignoreFailures", boolean(self.ignore_failures)),
            ("isCodeCoverageEnabled", boolean(self.is_code_coverage_enabled)),
            ("executionStrategy", self.execution_strategy.kotlin()),
            ("includeSerialRegexes", list(&self.include_serial_regexes, regex)),
            ("excludeSerialRegexes", list(&self.exclude_serial_regexes, regex)),
            ("testBatchTimeoutMillis", number(self.test_batch_timeout_millis)),
            ("testOutputTimeoutMillis", number(self.test_output_timeout_millis)),
            ("debug", boolean(self.debug)),
            ("screenRecordingPolicy", enumeration(self.screen_recording_policy)),
            ("vendorConfiguration", self.vendor_configuration.kotlin()),
            (
                "deviceInitializationTimeoutMillis",
                number(self.device_initialization_timeout_millis),
            ),
            ("ciConfiguration", self.ci_configuration.kotlin()),
        ];
        entries
            .into_iter()
            .map(|(key, value)| (key.into(), value.render()))
            .collect()
    }
}

/// The `toString()` of a configuration value.
trait Kotlin {
    fn kotlin(&self) -> KotlinValue;
}

fn data(name: &str, fields: Vec<(&str, KotlinValue)>) -> KotlinValue {
    KotlinValue::DataClass {
        name: name.into(),
        fields: fields.into_iter().map(|(k, v)| (k.into(), v)).collect(),
    }
}
/// A plain Kotlin `object` (or class) without a `toString()` override:
/// `Object.toString()`, the binary class name and the identity hash. The JVM
/// hash differs on every run, so any value is as faithful as another; the
/// Java `String.hashCode()` of the class name keeps ours stable (F29).
fn object(class: &str) -> KotlinValue {
    let hash = class
        .encode_utf16()
        .fold(0_i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)));
    KotlinValue::Text(format!("{class}@{:x}", hash as u32))
}
fn text(s: &String) -> KotlinValue {
    KotlinValue::Text(s.clone())
}
fn quoted(s: &String) -> KotlinValue {
    let mut out = String::from("'");
    out.push_str(s);
    out.push('\'');
    KotlinValue::Text(out)
}
fn path(p: &JavaPath) -> KotlinValue {
    KotlinValue::Text(String::from_utf16_lossy(&p.0.clone()))
}
fn regex(r: &JavaRegex) -> KotlinValue {
    r.pattern().into()
}
fn boolean(b: bool) -> KotlinValue {
    if b { "true" } else { "false" }.into()
}
fn number(n: impl ToString) -> KotlinValue {
    KotlinValue::Text(n.to_string())
}
fn double(d: f64) -> KotlinValue {
    KotlinValue::Text(double_to_string(d))
}
fn duration(d: &Duration) -> KotlinValue {
    number(d)
}
fn instant(i: &Instant) -> KotlinValue {
    number(i)
}
fn enumeration(e: impl Names) -> KotlinValue {
    e.kotlin().into()
}
fn opt<T>(v: Option<&T>, f: impl FnOnce(&T) -> KotlinValue) -> KotlinValue {
    v.map_or(KotlinValue::Null, f)
}
fn list<T>(v: &[T], f: impl FnMut(&T) -> KotlinValue) -> KotlinValue {
    KotlinValue::List(v.iter().map(f).collect())
}
fn string_map(v: &[(String, String)]) -> KotlinValue {
    KotlinValue::Map(v.iter().map(|(k, v)| (k.as_str().into(), text(v))).collect())
}

impl Kotlin for OutputConfiguration {
    fn kotlin(&self) -> KotlinValue {
        data(
            "OutputConfiguration",
            vec![
                ("maxPath", number(self.max_path)),
                ("maxFilename", number(self.max_filename)),
            ],
        )
    }
}
impl Kotlin for Defaults {
    fn kotlin(&self) -> KotlinValue {
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
impl Kotlin for RetentionPolicyConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
impl Kotlin for InfluxDb2RetentionPolicyConfiguration {
    fn kotlin(&self) -> KotlinValue {
        data(
            "RetentionPolicyConfiguration",
            vec![
                ("everySeconds", number(self.every_seconds)),
                ("shardGroupDurationSeconds", number(self.shard_group_duration_seconds)),
            ],
        )
    }
}
impl Kotlin for AnalyticsConfiguration {
    fn kotlin(&self) -> KotlinValue {
        let masked = || KotlinValue::Text("'*****'".into());
        match self {
            Self::Disabled => object("com.malinskiy.marathon.config.AnalyticsConfiguration$DisabledAnalytics"),
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
                        c.retention_policy_configuration.kotlin(),
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
                        c.retention_policy_configuration.kotlin(),
                    ),
                ],
            ),
            Self::Graphite(c) => data(
                "GraphiteConfiguration",
                vec![
                    ("host", text(&c.host)),
                    ("port", opt(c.port.as_ref(), |n| number(n))),
                    ("prefix", opt(c.prefix.as_ref(), text)),
                    ("defaults", c.defaults.kotlin()),
                    ("readOnly", boolean(c.read_only)),
                ],
            ),
        }
    }
}

const STRATEGY: &str = "com.malinskiy.marathon.config.strategy.";

impl Kotlin for PoolingStrategyConfiguration {
    fn kotlin(&self) -> KotlinValue {
        let simple = match self {
            Self::Omni => "OmniPoolingStrategyConfiguration",
            Self::Abi => "AbiPoolingStrategyConfiguration",
            Self::Manufacturer => "ManufacturerPoolingStrategyConfiguration",
            Self::Model => "ModelPoolingStrategyConfiguration",
            Self::OperatingSystemVersion => "OperatingSystemVersionPoolingStrategyConfiguration",
            Self::Combo(items) => {
                return data(
                    "ComboPoolingStrategyConfiguration",
                    vec![("list", list(items, Self::kotlin))],
                );
            }
        };
        object(&format!("{STRATEGY}PoolingStrategyConfiguration${simple}"))
    }
}
impl Kotlin for ShardingStrategyConfiguration {
    fn kotlin(&self) -> KotlinValue {
        match self {
            Self::Count { count } => data("CountShardingStrategyConfiguration", vec![("count", number(count))]),
            Self::Parallel => object(&format!(
                "{STRATEGY}ShardingStrategyConfiguration$ParallelShardingStrategyConfiguration"
            )),
        }
    }
}
impl Kotlin for SortingStrategyConfiguration {
    fn kotlin(&self) -> KotlinValue {
        match self {
            Self::ExecutionTime { percentile, time_limit } => data(
                "ExecutionTimeSortingStrategyConfiguration",
                vec![("percentile", double(*percentile)), ("timeLimit", instant(time_limit))],
            ),
            Self::NoSorting => object(&format!(
                "{STRATEGY}SortingStrategyConfiguration$NoSortingStrategyConfiguration"
            )),
            Self::RandomOrder => object(&format!(
                "{STRATEGY}SortingStrategyConfiguration$RandomOrderStrategyConfiguration"
            )),
            Self::SuccessRate { time_limit, ascending } => data(
                "SuccessRateSortingStrategyConfiguration",
                vec![("timeLimit", instant(time_limit)), ("ascending", boolean(*ascending))],
            ),
        }
    }
}
impl Kotlin for BatchingStrategyConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
            Self::ClassName => object(&format!(
                "{STRATEGY}BatchingStrategyConfiguration$ClassNameBatchingStrategyConfiguration"
            )),
            Self::Isolate => object(&format!(
                "{STRATEGY}BatchingStrategyConfiguration$IsolateBatchingStrategyConfiguration"
            )),
        }
    }
}
impl Kotlin for FlakinessStrategyConfiguration {
    fn kotlin(&self) -> KotlinValue {
        match self {
            Self::Ignore => object(&format!(
                "{STRATEGY}FlakinessStrategyConfiguration$IgnoreFlakinessStrategyConfiguration"
            )),
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
impl Kotlin for RetryStrategyConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
            Self::NoRetry => object(&format!(
                "{STRATEGY}RetryStrategyConfiguration$NoRetryStrategyConfiguration"
            )),
        }
    }
}
impl Kotlin for ExecutionStrategyConfiguration {
    fn kotlin(&self) -> KotlinValue {
        data(
            "ExecutionStrategyConfiguration",
            vec![("mode", enumeration(self.mode)), ("fast", boolean(self.fast))],
        )
    }
}
impl Kotlin for FilteringConfiguration {
    fn kotlin(&self) -> KotlinValue {
        data(
            "FilteringConfiguration",
            vec![
                ("allowlist", list(&self.allowlist, Kotlin::kotlin)),
                ("blocklist", list(&self.blocklist, Kotlin::kotlin)),
            ],
        )
    }
}
fn values_filter(name: &str, f: &RegexValuesFileFilter) -> KotlinValue {
    // Null `values` elements were dropped at bind (D9), so they cannot
    // show here, where Kotlin prints `null`.
    data(
        name,
        vec![
            ("regex", opt(f.regex.as_ref(), regex)),
            ("values", opt(f.values.as_ref(), |v| list(v, text))),
            ("file", opt(f.file.as_ref(), path)),
            ("enabled", boolean(f.enabled)),
        ],
    )
}
impl Kotlin for TestFilterConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
                    ("values", opt(values.as_ref(), |v| list(v, text))),
                    ("file", opt(file.as_ref(), path)),
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
                    ("filters", list(filters, Self::kotlin)),
                    ("op", enumeration(*op)),
                    ("enabled", boolean(*enabled)),
                ],
            ),
            Self::Allure { enabled } => data("AllureFilterConfiguration", vec![("enabled", boolean(*enabled))]),
        }
    }
}
impl Kotlin for CiConfiguration {
    fn kotlin(&self) -> KotlinValue {
        object(match self {
            Self::Auto => "com.malinskiy.marathon.config.CIConfiguration$Auto",
            Self::None => "com.malinskiy.marathon.config.CIConfiguration$None",
            Self::Teamcity => "com.malinskiy.marathon.config.CIConfiguration$Teamcity",
        })
    }
}

impl Kotlin for VendorConfiguration {
    fn kotlin(&self) -> KotlinValue {
        match self {
            Self::Android(c) => c.kotlin(),
            Self::Ios(c) => c.kotlin(),
            Self::Macos(c) => c.kotlin(),
            Self::Stub => object("com.malinskiy.marathon.config.vendor.VendorConfiguration$StubVendorConfiguration"),
            Self::Empty => object("com.malinskiy.marathon.config.vendor.VendorConfiguration$EmptyVendorConfiguration"),
        }
    }
}
fn paths(v: Option<&Vec<JavaPath>>) -> KotlinValue {
    opt(v, |v| list(v, path))
}
impl Kotlin for AndroidConfiguration {
    fn kotlin(&self) -> KotlinValue {
        data(
            "AndroidConfiguration",
            vec![
                ("androidSdk", opt(self.android_sdk.as_ref(), path)),
                ("applicationOutput", opt(self.application_output.as_ref(), path)),
                (
                    "testApplicationOutput",
                    opt(self.test_application_output.as_ref(), path),
                ),
                ("splitApks", paths(self.split_apks.as_ref())),
                (
                    "extraApplicationsOutput",
                    paths(self.extra_applications_output.as_ref()),
                ),
                ("outputs", opt(self.outputs.as_ref(), |v| list(v, Kotlin::kotlin))),
                ("autoGrantPermission", boolean(self.auto_grant_permission)),
                ("instrumentationArgs", string_map(&self.instrumentation_args)),
                ("applicationPmClear", boolean(self.application_pm_clear)),
                ("testApplicationPmClear", boolean(self.test_application_pm_clear)),
                ("adbInitTimeoutMillis", number(self.adb_init_timeout_millis)),
                ("installOptions", text(&self.install_options)),
                ("serialStrategy", enumeration(self.serial_strategy)),
                ("screenRecordConfiguration", self.screen_record_configuration.kotlin()),
                (
                    "waitForDevicesTimeoutMillis",
                    number(self.wait_for_devices_timeout_millis),
                ),
                ("allureConfiguration", self.allure_configuration.kotlin()),
                ("timeoutConfiguration", self.timeout_configuration.kotlin()),
                ("fileSyncConfiguration", self.file_sync_configuration.kotlin()),
                ("threadingConfiguration", self.threading_configuration.kotlin()),
                ("testParserConfiguration", self.test_parser_configuration.kotlin()),
                ("testAccessConfiguration", self.test_access_configuration.kotlin()),
                ("adbServers", list(&self.adb_servers, Kotlin::kotlin)),
                ("disableWindowAnimation", boolean(self.disable_window_animation)),
                ("profilingConfiguration", self.profiling_configuration.kotlin()),
                ("mockLocation", boolean(self.mock_location)),
            ],
        )
    }
}
impl Kotlin for AndroidTestBundleConfiguration {
    fn kotlin(&self) -> KotlinValue {
        data(
            "AndroidTestBundleConfiguration",
            vec![
                ("application", opt(self.application.as_ref(), path)),
                ("testApplication", path(&self.test_application)),
                ("extraApplications", paths(self.extra_applications.as_ref())),
                ("splitApks", paths(self.split_apks.as_ref())),
            ],
        )
    }
}
impl Kotlin for AndroidScreenRecordConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
impl Kotlin for AllureConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
impl Kotlin for AndroidTimeoutConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
impl Kotlin for FileSyncConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
impl Kotlin for AndroidThreadingConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
impl Kotlin for AndroidTestParserConfiguration {
    fn kotlin(&self) -> KotlinValue {
        match self {
            Self::Local => object(
                "com.malinskiy.marathon.config.vendor.android.TestParserConfiguration$LocalTestParserConfiguration",
            ),
            Self::Remote { instrumentation_args } => data(
                "RemoteTestParserConfiguration",
                vec![("instrumentationArgs", string_map(instrumentation_args))],
            ),
        }
    }
}
impl Kotlin for TestAccessConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
impl Kotlin for AdbEndpoint {
    fn kotlin(&self) -> KotlinValue {
        data(
            "AdbEndpoint",
            vec![("host", text(&self.host)), ("port", number(self.port))],
        )
    }
}
impl Kotlin for ProfilingConfiguration {
    fn kotlin(&self) -> KotlinValue {
        data(
            "ProfilingConfiguration",
            vec![
                ("enabled", boolean(self.enabled)),
                ("pbtxt", opt(self.pbtxt.as_ref(), path)),
                ("urlTemplate", text(&self.url_template)),
            ],
        )
    }
}

impl Kotlin for IosConfiguration {
    fn kotlin(&self) -> KotlinValue {
        data(
            "IOSConfiguration",
            vec![
                ("bundle", opt(self.bundle.as_ref(), Kotlin::kotlin)),
                ("deviceProvider", self.device_provider.kotlin()),
                ("devicesFile", opt(self.devices_file.as_ref(), path)),
                ("ssh", self.ssh.kotlin()),
                ("workerConnection", self.worker_connection.kotlin()),
                ("xcresult", self.xcresult.kotlin()),
                ("screenRecordConfiguration", self.screen_record_configuration.kotlin()),
                ("xctestrunEnv", self.xctestrun_env.kotlin()),
                ("lifecycleConfiguration", self.lifecycle_configuration.kotlin()),
                ("permissions", self.permissions.kotlin()),
                ("mediaFiles", paths(self.media_files.as_ref())),
                ("timeoutConfiguration", self.timeout_configuration.kotlin()),
                ("threadingConfiguration", self.threading_configuration.kotlin()),
                ("hideRunnerOutput", boolean(self.hide_runner_output)),
                ("compactOutput", boolean(self.compact_output)),
                ("rsync", self.rsync.kotlin()),
                ("xcodebuildTestArgs", string_map(&self.xcodebuild_test_args)),
                ("dataContainerClear", boolean(self.data_container_clear)),
                ("testParserConfiguration", self.test_parser_configuration.kotlin()),
                ("deviceLog", boolean(self.device_log)),
                (
                    "signing",
                    data(
                        "SigningConfiguration",
                        vec![(
                            "testRunnerProvisioningProfile",
                            opt(self.signing.test_runner_provisioning_profile.as_ref(), text),
                        )],
                    ),
                ),
                ("skipCopyingDerivedData", boolean(self.skip_copying_derived_data)),
                ("maxBatchesWithoutProgress", number(self.max_batches_without_progress)),
            ],
        )
    }
}
impl Kotlin for MacosConfiguration {
    fn kotlin(&self) -> KotlinValue {
        data(
            "MacosConfiguration",
            vec![
                ("bundle", opt(self.bundle.as_ref(), Kotlin::kotlin)),
                ("deviceProvider", self.device_provider.kotlin()),
                ("devicesFile", opt(self.devices_file.as_ref(), path)),
                ("ssh", self.ssh.kotlin()),
                ("xcresult", self.xcresult.kotlin()),
                ("xctestrunEnv", self.xctestrun_env.kotlin()),
                ("timeoutConfiguration", self.timeout_configuration.kotlin()),
                ("threadingConfiguration", self.threading_configuration.kotlin()),
                ("hideRunnerOutput", boolean(self.hide_runner_output)),
                ("compactOutput", boolean(self.compact_output)),
                ("rsync", self.rsync.kotlin()),
                ("xcodebuildTestArgs", string_map(&self.xcodebuild_test_args)),
                ("testParserConfiguration", self.test_parser_configuration.kotlin()),
                (
                    "permissions",
                    data(
                        "PermissionsConfiguration",
                        vec![(
                            "grant",
                            KotlinValue::Map(
                                self.permissions
                                    .grant
                                    .iter()
                                    .map(|(k, v)| (k.as_str().into(), list(v, |p| enumeration(*p))))
                                    .collect(),
                            ),
                        )],
                    ),
                ),
            ],
        )
    }
}
impl Kotlin for AppleTestBundleConfiguration {
    fn kotlin(&self) -> KotlinValue {
        // `app`/`testApp`/`xctest` are body properties, outside the data
        // class `toString()`; the constructor's `tempDirFor` lambda is in it.
        data(
            "AppleTestBundleConfiguration",
            vec![
                ("application", opt(self.application.as_ref(), path)),
                ("testApplication", opt(self.test_application.as_ref(), path)),
                ("extraApplications", paths(self.extra_applications.as_ref())),
                ("derivedDataDir", opt(self.derived_data_dir.as_ref(), path)),
                ("testType", opt(self.test_type.as_ref(), |t| enumeration(*t))),
                ("xctestrun", opt(self.xctestrun.as_ref(), path)),
                ("tempDirFor", "(java.io.File) -> java.io.File".into()),
            ],
        )
    }
}
impl Kotlin for DeviceProvider {
    fn kotlin(&self) -> KotlinValue {
        match self {
            Self::Static { path: p } => data("Static", vec![("path", opt(p.as_ref(), path))]),
            Self::Dynamic { host, port } => data("Dynamic", vec![("host", text(host)), ("port", number(port))]),
        }
    }
}
impl Kotlin for SshConfiguration {
    fn kotlin(&self) -> KotlinValue {
        let authentication = self.authentication.as_ref().map_or(KotlinValue::Null, |a| match a {
            // `PasswordAuthentication.toString()` hides the password (SshConfiguration.kt:58-60).
            SshAuthentication::Password { username, .. } => {
                data("PasswordAuthentication", vec![("username", quoted(username))])
            }
            SshAuthentication::PublicKey { username, key } => data(
                "PublicKeyAuthentication",
                vec![("username", text(username)), ("key", path(key))],
            ),
        });
        data(
            "SshConfiguration",
            vec![
                ("authentication", authentication),
                ("knownHostsPath", opt(self.known_hosts_path.as_ref(), path)),
                ("keepAliveInterval", duration(&self.keep_alive_interval)),
                ("keepAliveMaxCount", number(self.keep_alive_max_count)),
                ("debug", boolean(self.debug)),
                ("shareWorkerConnection", boolean(self.share_worker_connection)),
            ],
        )
    }
}
impl Kotlin for WorkerConnectionConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
impl Kotlin for XcresultConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
impl Kotlin for IosScreenRecordConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
impl Kotlin for XctestrunEnvConfiguration {
    fn kotlin(&self) -> KotlinValue {
        data(
            "XctestrunEnvConfiguration",
            vec![
                ("appEnvs", string_map(&self.app_envs)),
                ("testEnvs", string_map(&self.test_envs)),
            ],
        )
    }
}
impl Kotlin for LifecycleConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
impl Kotlin for IosPermissionsConfiguration {
    fn kotlin(&self) -> KotlinValue {
        data(
            "PermissionsConfiguration",
            vec![
                ("grant", list(&self.grant, |p| enumeration(*p))),
                ("lifecycle", enumeration(self.lifecycle)),
            ],
        )
    }
}
impl Kotlin for AppleTimeoutConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
impl Kotlin for AppleThreadingConfiguration {
    fn kotlin(&self) -> KotlinValue {
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
impl Kotlin for RsyncConfiguration {
    fn kotlin(&self) -> KotlinValue {
        data("RsyncConfiguration", vec![("remotePath", text(&self.remote_path))])
    }
}
impl Kotlin for AppleTestParserConfiguration {
    fn kotlin(&self) -> KotlinValue {
        match self {
            Self::Nm { test_class_regexes } => data(
                "NmTestParserConfiguration",
                vec![("testClassRegexes", list(test_class_regexes, |r| r.pattern().into()))],
            ),
            Self::Xctest => {
                object("com.malinskiy.marathon.config.vendor.apple.TestParserConfiguration$XCTestParserConfiguration")
            }
        }
    }
}

// ---- serialize() --------------------------------------------------------------

/// `ConfigurationFactory.serialize` (ConfigurationFactory.kt:205-207): the
/// configuration as Marathonfile YAML that the loader parses back to an
/// equal configuration.
pub fn serialize(configuration: &Configuration) -> String {
    JacksonYamlWriter.write(&configuration.writer_value())
}

/// The Jackson view of a configuration value for 1.5's writers. Nulls are
/// `WriterValue::Null` entries, which the YAML writer omits (`NON_NULL`).
pub trait WriterProjection {
    fn writer_value(&self) -> WriterValue;
}

fn obj(entries: Vec<(&str, WriterValue)>) -> WriterValue {
    WriterValue::Object(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
}
/// `@JsonTypeInfo(As.PROPERTY)`: the type id comes first.
fn typed(id: &str, mut entries: Vec<(&str, WriterValue)>) -> WriterValue {
    entries.insert(0, ("type", id.into()));
    obj(entries)
}
fn w_text(s: &String) -> WriterValue {
    WriterValue::String(s.clone())
}
fn w_path(p: &JavaPath) -> WriterValue {
    WriterValue::String(String::from_utf16_lossy(&p.0.clone()))
}
/// `RegexSerializer`: the pattern string.
fn w_regex(r: &JavaRegex) -> WriterValue {
    r.pattern().into()
}
fn w_int(n: impl Into<i64>) -> WriterValue {
    WriterValue::from(n.into())
}
fn w_double(d: f64) -> WriterValue {
    match JavaNumber::new(double_to_string(d)) {
        Ok(n) => WriterValue::Number(n),
        // Unreachable: `JavaNumber` accepts every `Double.toString` spelling,
        // NaN and ±Infinity included, so those are written as plain
        // scalars (`successRate: NaN`), as Jackson writes them.
        Err(e) => WriterValue::String(e.0),
    }
}
/// `WRITE_DURATIONS_AS_TIMESTAMPS`/`WRITE_DATES_AS_TIMESTAMPS` off: ISO text.
fn w_duration(d: &Duration) -> WriterValue {
    d.to_string().as_str().into()
}
fn w_instant(i: &Instant) -> WriterValue {
    i.to_string().as_str().into()
}
fn w_enum(e: impl Names) -> WriterValue {
    e.json().into()
}
fn w_opt<T>(v: Option<&T>, f: impl FnOnce(&T) -> WriterValue) -> WriterValue {
    v.map_or(WriterValue::Null, f)
}
fn w_list<T>(v: &[T], f: impl FnMut(&T) -> WriterValue) -> WriterValue {
    WriterValue::Array(v.iter().map(f).collect())
}
fn w_paths(v: Option<&Vec<JavaPath>>) -> WriterValue {
    w_opt(v, |v| w_list(v, w_path))
}
fn w_map(v: &[(String, String)]) -> WriterValue {
    WriterValue::Object(v.iter().map(|(k, v)| (k.as_str().into(), w_text(v))).collect())
}

impl WriterProjection for Configuration {
    fn writer_value(&self) -> WriterValue {
        obj(vec![
            ("name", w_text(&self.name)),
            ("outputDir", w_path(&self.output_dir)),
            (
                "outputConfiguration",
                obj(vec![
                    ("maxPath", w_int(self.output_configuration.max_path)),
                    ("maxFilename", w_int(self.output_configuration.max_filename)),
                ]),
            ),
            ("analyticsConfiguration", self.analytics_configuration.writer_value()),
            ("poolingStrategy", self.pooling_strategy.writer_value()),
            ("shardingStrategy", self.sharding_strategy.writer_value()),
            ("sortingStrategy", self.sorting_strategy.writer_value()),
            ("batchingStrategy", self.batching_strategy.writer_value()),
            ("flakinessStrategy", self.flakiness_strategy.writer_value()),
            ("retryStrategy", self.retry_strategy.writer_value()),
            ("filteringConfiguration", self.filtering_configuration.writer_value()),
            ("ignoreFailures", WriterValue::Bool(self.ignore_failures)),
            (
                "executionStrategy",
                obj(vec![
                    ("mode", w_enum(self.execution_strategy.mode)),
                    ("fast", WriterValue::Bool(self.execution_strategy.fast)),
                ]),
            ),
            (
                "isCodeCoverageEnabled",
                WriterValue::Bool(self.is_code_coverage_enabled),
            ),
            ("uncompletedTestRetryQuota", w_int(self.uncompleted_test_retry_quota)),
            ("includeSerialRegexes", w_list(&self.include_serial_regexes, w_regex)),
            ("excludeSerialRegexes", w_list(&self.exclude_serial_regexes, w_regex)),
            ("testBatchTimeoutMillis", w_int(self.test_batch_timeout_millis)),
            ("testOutputTimeoutMillis", w_int(self.test_output_timeout_millis)),
            ("debug", WriterValue::Bool(self.debug)),
            ("screenRecordingPolicy", w_enum(self.screen_recording_policy)),
            ("vendorConfiguration", self.vendor_configuration.writer_value()),
            ("analyticsTracking", WriterValue::Bool(self.analytics_tracking)),
            ("bugsnagReporting", WriterValue::Bool(self.bugsnag_reporting)),
            (
                "deviceInitializationTimeoutMillis",
                w_int(self.device_initialization_timeout_millis),
            ),
            (
                "ciConfiguration",
                typed(
                    match self.ci_configuration {
                        CiConfiguration::Auto => "auto",
                        CiConfiguration::None => "none",
                        CiConfiguration::Teamcity => "teamcity",
                    },
                    vec![],
                ),
            ),
        ])
    }
}
fn w_defaults(d: &Defaults) -> WriterValue {
    obj(vec![
        ("successRate", w_double(d.success_rate)),
        ("duration", w_duration(&d.duration)),
        (
            "observedDurationMinTests",
            w_opt(d.observed_duration_min_tests.as_ref(), |n| w_int(*n)),
        ),
    ])
}
impl WriterProjection for AnalyticsConfiguration {
    fn writer_value(&self) -> WriterValue {
        match self {
            Self::Disabled => typed("disabled", vec![]),
            Self::InfluxDb(c) => {
                let rp = &c.retention_policy_configuration;
                typed(
                    "influxdb",
                    vec![
                        ("url", w_text(&c.url)),
                        ("user", w_text(&c.user)),
                        ("password", w_text(&c.password)),
                        ("dbName", w_text(&c.db_name)),
                        (
                            "retentionPolicyConfiguration",
                            obj(vec![
                                ("name", w_text(&rp.name)),
                                ("duration", w_text(&rp.duration)),
                                ("shardDuration", w_text(&rp.shard_duration)),
                                ("replicationFactor", w_int(rp.replication_factor)),
                                ("isDefault", WriterValue::Bool(rp.is_default)),
                            ]),
                        ),
                        ("defaults", w_defaults(&c.defaults)),
                        ("readOnly", WriterValue::Bool(c.read_only)),
                    ],
                )
            }
            Self::InfluxDb2(c) => typed(
                "influxdb2",
                vec![
                    ("url", w_text(&c.url)),
                    ("token", w_text(&c.token)),
                    ("organization", w_text(&c.organization)),
                    ("bucket", w_text(&c.bucket)),
                    (
                        "retentionPolicyConfiguration",
                        obj(vec![
                            ("everySeconds", w_int(c.retention_policy_configuration.every_seconds)),
                            (
                                "shardGroupDurationSeconds",
                                w_int(c.retention_policy_configuration.shard_group_duration_seconds),
                            ),
                        ]),
                    ),
                    ("defaults", w_defaults(&c.defaults)),
                    ("readOnly", WriterValue::Bool(c.read_only)),
                ],
            ),
            Self::Graphite(c) => typed(
                "graphite",
                vec![
                    ("host", w_text(&c.host)),
                    ("port", w_opt(c.port.as_ref(), |p| w_int(*p))),
                    ("prefix", w_opt(c.prefix.as_ref(), w_text)),
                    ("defaults", w_defaults(&c.defaults)),
                    ("readOnly", WriterValue::Bool(c.read_only)),
                ],
            ),
        }
    }
}
impl WriterProjection for PoolingStrategyConfiguration {
    fn writer_value(&self) -> WriterValue {
        match self {
            Self::Omni => typed("omni", vec![]),
            Self::Abi => typed("abi", vec![]),
            Self::Manufacturer => typed("manufacturer", vec![]),
            Self::Model => typed("device-model", vec![]),
            Self::OperatingSystemVersion => typed("os-version", vec![]),
            Self::Combo(items) => typed("combo", vec![("list", w_list(items, Self::writer_value))]),
        }
    }
}
impl WriterProjection for ShardingStrategyConfiguration {
    fn writer_value(&self) -> WriterValue {
        match self {
            Self::Count { count } => typed("count", vec![("count", w_int(*count))]),
            Self::Parallel => typed("parallel", vec![]),
        }
    }
}
impl WriterProjection for SortingStrategyConfiguration {
    fn writer_value(&self) -> WriterValue {
        match self {
            Self::ExecutionTime { percentile, time_limit } => typed(
                "execution-time",
                vec![
                    ("percentile", w_double(*percentile)),
                    ("timeLimit", w_instant(time_limit)),
                ],
            ),
            Self::NoSorting => typed("no-sorting", vec![]),
            Self::RandomOrder => typed("random-order", vec![]),
            Self::SuccessRate { time_limit, ascending } => typed(
                "success-rate",
                vec![
                    ("timeLimit", w_instant(time_limit)),
                    ("ascending", WriterValue::Bool(*ascending)),
                ],
            ),
        }
    }
}
impl WriterProjection for BatchingStrategyConfiguration {
    fn writer_value(&self) -> WriterValue {
        match self {
            Self::FixedSize {
                size,
                duration_millis,
                percentile,
                time_limit,
                last_mile_length,
            } => typed(
                "fixed-size",
                vec![
                    ("size", w_int(*size)),
                    ("durationMillis", w_opt(duration_millis.as_ref(), |n| w_int(*n))),
                    ("percentile", w_opt(percentile.as_ref(), |p| w_double(*p))),
                    ("timeLimit", w_opt(time_limit.as_ref(), w_instant)),
                    ("lastMileLength", w_int(*last_mile_length)),
                ],
            ),
            Self::ClassName => typed("class-name", vec![]),
            Self::Isolate => typed("isolate", vec![]),
        }
    }
}
impl WriterProjection for FlakinessStrategyConfiguration {
    fn writer_value(&self) -> WriterValue {
        match self {
            Self::Ignore => typed("ignore", vec![]),
            Self::Probability {
                min_success_rate,
                max_count,
                time_limit,
            } => typed(
                "probability",
                vec![
                    ("minSuccessRate", w_double(*min_success_rate)),
                    ("maxCount", w_int(*max_count)),
                    ("timeLimit", w_instant(time_limit)),
                ],
            ),
        }
    }
}
impl WriterProjection for RetryStrategyConfiguration {
    fn writer_value(&self) -> WriterValue {
        match self {
            Self::FixedQuota {
                total_allowed_retry_quota,
                retry_per_test_quota,
            } => typed(
                "fixed-quota",
                vec![
                    ("totalAllowedRetryQuota", w_int(*total_allowed_retry_quota)),
                    ("retryPerTestQuota", w_int(*retry_per_test_quota)),
                ],
            ),
            Self::NoRetry => typed("no-retry", vec![]),
        }
    }
}
impl WriterProjection for FilteringConfiguration {
    fn writer_value(&self) -> WriterValue {
        obj(vec![
            ("allowlist", w_list(&self.allowlist, WriterProjection::writer_value)),
            ("blocklist", w_list(&self.blocklist, WriterProjection::writer_value)),
        ])
    }
}
fn w_values_filter(id: &str, f: &RegexValuesFileFilter) -> WriterValue {
    typed(
        id,
        vec![
            ("regex", w_opt(f.regex.as_ref(), w_regex)),
            ("values", w_opt(f.values.as_ref(), |v| w_list(v, w_text))),
            ("file", w_opt(f.file.as_ref(), w_path)),
            ("enabled", WriterValue::Bool(f.enabled)),
        ],
    )
}
impl WriterProjection for TestFilterConfiguration {
    fn writer_value(&self) -> WriterValue {
        match self {
            Self::SimpleClassname(f) => w_values_filter("simple-class-name", f),
            Self::SimpleTestname(f) => w_values_filter("simple-test-name", f),
            Self::FullyQualifiedClassname(f) => w_values_filter("fully-qualified-class-name", f),
            Self::FullyQualifiedTestname(f) => w_values_filter("fully-qualified-test-name", f),
            Self::TestMethod(f) => w_values_filter("method", f),
            Self::Annotation(f) => w_values_filter("annotation", f),
            Self::TestPackage(f) => typed(
                "package",
                vec![
                    ("regex", w_opt(f.regex.as_ref(), w_regex)),
                    ("values", w_opt(f.values.as_ref(), |v| w_list(v, w_text))),
                    ("file", w_opt(f.file.as_ref(), w_path)),
                    ("subpackages", WriterValue::Bool(f.subpackages)),
                    ("enabled", WriterValue::Bool(f.enabled)),
                ],
            ),
            Self::AnnotationData {
                name_regex,
                value_regex,
                enabled,
            } => typed(
                "annotationData",
                vec![
                    ("nameRegex", w_regex(name_regex)),
                    ("valueRegex", w_regex(value_regex)),
                    ("enabled", WriterValue::Bool(*enabled)),
                ],
            ),
            Self::Fragmentation { index, count, enabled } => typed(
                "fragmentation",
                vec![
                    ("index", w_int(*index)),
                    ("count", w_int(*count)),
                    ("enabled", WriterValue::Bool(*enabled)),
                ],
            ),
            Self::Composition { filters, op, enabled } => typed(
                "composition",
                vec![
                    ("filters", w_list(filters, Self::writer_value)),
                    ("op", w_enum(*op)),
                    ("enabled", WriterValue::Bool(*enabled)),
                ],
            ),
            Self::Allure { enabled } => typed("allure", vec![("enabled", WriterValue::Bool(*enabled))]),
        }
    }
}

impl WriterProjection for VendorConfiguration {
    fn writer_value(&self) -> WriterValue {
        match self {
            Self::Android(c) => android(c),
            // iOS is registered under four names; Jackson writes the first.
            Self::Ios(c) => ios(c),
            Self::Macos(c) => macos(c),
            Self::Stub => typed("stub", vec![]),
            Self::Empty => typed("empty", vec![]),
        }
    }
}
fn android(c: &AndroidConfiguration) -> WriterValue {
    let video = &c.screen_record_configuration.video_configuration;
    let screenshot = &c.screen_record_configuration.screenshot_configuration;
    let t = &c.timeout_configuration;
    typed(
        "Android",
        vec![
            ("androidSdk", w_opt(c.android_sdk.as_ref(), w_path)),
            ("applicationApk", w_opt(c.application_output.as_ref(), w_path)),
            ("testApplicationApk", w_opt(c.test_application_output.as_ref(), w_path)),
            ("splitApks", w_paths(c.split_apks.as_ref())),
            ("extraApplicationsApk", w_paths(c.extra_applications_output.as_ref())),
            (
                "outputs",
                w_opt(c.outputs.as_ref(), |v| {
                    w_list(v, |o| {
                        obj(vec![
                            ("application", w_opt(o.application.as_ref(), w_path)),
                            ("testApplication", w_path(&o.test_application)),
                            ("extraApplications", w_paths(o.extra_applications.as_ref())),
                            ("splitApks", w_paths(o.split_apks.as_ref())),
                        ])
                    })
                }),
            ),
            ("autoGrantPermission", WriterValue::Bool(c.auto_grant_permission)),
            ("instrumentationArgs", w_map(&c.instrumentation_args)),
            ("applicationPmClear", WriterValue::Bool(c.application_pm_clear)),
            ("testApplicationPmClear", WriterValue::Bool(c.test_application_pm_clear)),
            ("adbInitTimeoutMillis", w_int(c.adb_init_timeout_millis)),
            ("installOptions", w_text(&c.install_options)),
            ("serialStrategy", w_enum(c.serial_strategy)),
            (
                "screenRecordConfiguration",
                obj(vec![
                    (
                        "preferableRecorderType",
                        w_opt(c.screen_record_configuration.preferable_recorder_type.as_ref(), |t| {
                            w_enum(*t)
                        }),
                    ),
                    (
                        "videoConfiguration",
                        obj(vec![
                            ("enabled", WriterValue::Bool(video.enabled)),
                            ("width", w_int(video.width)),
                            ("height", w_int(video.height)),
                            ("bitrateMbps", w_int(video.bitrate_mbps)),
                            ("timeLimit", w_int(video.time_limit)),
                            ("timeLimitUnits", w_enum(video.time_limit_units)),
                        ]),
                    ),
                    (
                        "screenshotConfiguration",
                        obj(vec![
                            ("enabled", WriterValue::Bool(screenshot.enabled)),
                            ("width", w_int(screenshot.width)),
                            ("height", w_int(screenshot.height)),
                            ("delayMs", w_int(screenshot.delay_ms)),
                        ]),
                    ),
                ]),
            ),
            ("waitForDevicesTimeoutMillis", w_int(c.wait_for_devices_timeout_millis)),
            (
                "allureConfiguration",
                obj(vec![
                    ("enabled", WriterValue::Bool(c.allure_configuration.enabled)),
                    (
                        "relativeResultsDirectory",
                        w_text(&c.allure_configuration.relative_results_directory),
                    ),
                    ("pathRoot", w_enum(c.allure_configuration.path_root)),
                ]),
            ),
            (
                "timeoutConfiguration",
                obj(vec![
                    ("shell", w_duration(&t.shell)),
                    ("listFiles", w_duration(&t.list_files)),
                    ("pushFile", w_duration(&t.push_file)),
                    ("pushFolder", w_duration(&t.push_folder)),
                    ("pullFile", w_duration(&t.pull_file)),
                    ("uninstall", w_duration(&t.uninstall)),
                    ("install", w_duration(&t.install)),
                    ("screenrecorder", w_duration(&t.screenrecorder)),
                    ("screencapturer", w_duration(&t.screencapturer)),
                    ("socketIdleTimeout", w_duration(&t.socket_idle_timeout)),
                    ("portForward", w_duration(&t.port_forward)),
                    ("boot", w_duration(&t.boot)),
                ]),
            ),
            (
                "fileSyncConfiguration",
                obj(vec![
                    (
                        "pull",
                        w_list(&c.file_sync_configuration.pull, |e| {
                            obj(vec![
                                ("relativePath", w_text(&e.relative_path)),
                                ("pathRoot", w_enum(e.path_root)),
                                ("aggregationMode", w_enum(e.aggregation_mode)),
                            ])
                        }),
                    ),
                    (
                        "push",
                        w_list(&c.file_sync_configuration.push, |e| {
                            obj(vec![("path", w_text(&e.path)), ("pathRoot", w_enum(e.path_root))])
                        }),
                    ),
                ]),
            ),
            (
                "threadingConfiguration",
                obj(vec![
                    (
                        "bootWaitingThreads",
                        w_int(c.threading_configuration.boot_waiting_threads),
                    ),
                    ("adbIoThreads", w_int(c.threading_configuration.adb_io_threads)),
                    ("installThreads", w_int(c.threading_configuration.install_threads)),
                ]),
            ),
            (
                "testParserConfiguration",
                match &c.test_parser_configuration {
                    AndroidTestParserConfiguration::Local => typed("local", vec![]),
                    AndroidTestParserConfiguration::Remote { instrumentation_args } => {
                        typed("remote", vec![("instrumentationArgs", w_map(instrumentation_args))])
                    }
                },
            ),
            (
                "testAccessConfiguration",
                obj(vec![
                    ("adb", WriterValue::Bool(c.test_access_configuration.adb)),
                    ("grpc", WriterValue::Bool(c.test_access_configuration.grpc)),
                    ("console", WriterValue::Bool(c.test_access_configuration.console)),
                    ("consoleToken", w_text(&c.test_access_configuration.console_token)),
                ]),
            ),
            (
                "adbServers",
                w_list(&c.adb_servers, |e| {
                    obj(vec![("host", w_text(&e.host)), ("port", w_int(e.port))])
                }),
            ),
            ("disableWindowAnimation", WriterValue::Bool(c.disable_window_animation)),
            (
                "profilingConfiguration",
                obj(vec![
                    ("enabled", WriterValue::Bool(c.profiling_configuration.enabled)),
                    ("pbtxt", w_opt(c.profiling_configuration.pbtxt.as_ref(), w_path)),
                    ("urlTemplate", w_text(&c.profiling_configuration.url_template)),
                ]),
            ),
            ("mockLocation", WriterValue::Bool(c.mock_location)),
        ],
    )
}
/// Only the constructor properties; the resolved `app`/`testApp`/`xctest`
/// are `@JsonIgnore` and are found again by the re-parse's `validate()`.
fn w_bundle(b: &AppleTestBundleConfiguration) -> WriterValue {
    obj(vec![
        ("application", w_opt(b.application.as_ref(), w_path)),
        ("testApplication", w_opt(b.test_application.as_ref(), w_path)),
        ("extraApplications", w_paths(b.extra_applications.as_ref())),
        ("derivedDataDir", w_opt(b.derived_data_dir.as_ref(), w_path)),
        ("testType", w_opt(b.test_type.as_ref(), |t| w_enum(*t))),
        ("xctestrun", w_opt(b.xctestrun.as_ref(), w_path)),
    ])
}
fn w_device_provider(p: &DeviceProvider) -> WriterValue {
    match p {
        DeviceProvider::Static { path } => typed("static", vec![("path", w_opt(path.as_ref(), w_path))]),
        DeviceProvider::Dynamic { host, port } => {
            typed("dynamic", vec![("host", w_text(host)), ("port", w_int(*port))])
        }
    }
}
fn w_ssh(s: &SshConfiguration) -> WriterValue {
    obj(vec![
        (
            "authentication",
            w_opt(s.authentication.as_ref(), |a| match a {
                SshAuthentication::Password { username, password } => typed(
                    "password",
                    vec![("username", w_text(username)), ("password", w_text(password))],
                ),
                SshAuthentication::PublicKey { username, key } => {
                    typed("publicKey", vec![("username", w_text(username)), ("key", w_path(key))])
                }
            }),
        ),
        ("knownHostsPath", w_opt(s.known_hosts_path.as_ref(), w_path)),
        ("keepAliveInterval", w_duration(&s.keep_alive_interval)),
        ("keepAliveMaxCount", w_int(s.keep_alive_max_count)),
        ("debug", WriterValue::Bool(s.debug)),
        ("shareWorkerConnection", WriterValue::Bool(s.share_worker_connection)),
    ])
}
impl WriterProjection for XcresultConfiguration {
    /// `effectiveWorkerCapacity()` is a function, not a property, so it is
    /// never written (XcresultQueueConfigurationTest).
    fn writer_value(&self) -> WriterValue {
        obj(vec![
            ("pull", w_opt(self.pull.as_ref(), |b| WriterValue::Bool(*b))),
            ("pullingPolicy", w_enum(self.pulling_policy)),
            ("remoteClean", WriterValue::Bool(self.remote_clean)),
            (
                "attachments",
                obj(vec![
                    (
                        "systemAttachmentLifetime",
                        w_enum(self.attachments.system_attachment_lifetime),
                    ),
                    (
                        "userAttachmentLifetime",
                        w_enum(self.attachments.user_attachment_lifetime),
                    ),
                ]),
            ),
            (
                "preferredScreenCaptureFormat",
                w_opt(self.preferred_screen_capture_format.as_ref(), |f| w_enum(*f)),
            ),
            (
                "queue",
                obj(vec![
                    ("capacity", w_int(self.queue.capacity)),
                    ("concurrency", w_int(self.queue.concurrency)),
                    (
                        "workerCapacity",
                        w_opt(self.queue.worker_capacity.as_ref(), |n| w_int(*n)),
                    ),
                ]),
            ),
        ])
    }
}
fn w_xctestrun_env(e: &XctestrunEnvConfiguration) -> WriterValue {
    obj(vec![("app", w_map(&e.app_envs)), ("test", w_map(&e.test_envs))])
}
fn w_apple_timeouts(t: &AppleTimeoutConfiguration) -> WriterValue {
    // Derived values (`xcresulttool`, `bootWait`, the shell-defaulted ones)
    // are plain fields by now and are written, so the re-parse keeps them.
    obj(vec![
        ("shell", w_duration(&t.shell)),
        ("shellIdle", w_duration(&t.shell_idle)),
        ("reachability", w_duration(&t.reachability)),
        ("screenshot", w_duration(&t.screenshot)),
        ("video", w_duration(&t.video)),
        ("erase", w_duration(&t.erase)),
        ("shutdown", w_duration(&t.shutdown)),
        ("delete", w_duration(&t.delete)),
        ("create", w_duration(&t.create)),
        ("boot", w_duration(&t.boot)),
        ("install", w_duration(&t.install)),
        ("uninstall", w_duration(&t.uninstall)),
        ("importMedia", w_duration(&t.import_media)),
        ("testDestination", w_duration(&t.test_destination)),
        ("xcresulttool", w_duration(&t.xcresulttool)),
        ("bootWait", w_duration(&t.boot_wait)),
    ])
}
fn w_apple_threading(t: &AppleThreadingConfiguration) -> WriterValue {
    obj(vec![
        ("deviceProviderThreads", w_int(t.device_provider_threads)),
        ("deviceThreads", w_int(t.device_threads)),
        ("provisioningConcurrency", w_int(t.provisioning_concurrency)),
    ])
}
fn w_apple_test_parser(p: &AppleTestParserConfiguration) -> WriterValue {
    match p {
        // Kotlin writes the default pattern, which the regex crate cannot
        // compile (D6, WP 2.3a decision 7); leaving the key out rebinds the
        // same default.
        AppleTestParserConfiguration::Nm { test_class_regexes }
            if matches!(test_class_regexes.as_slice(), [TestClassRegex::Default]) =>
        {
            typed("nm", vec![])
        }
        AppleTestParserConfiguration::Nm { test_class_regexes } => typed(
            "nm",
            vec![("testClassRegexes", w_list(test_class_regexes, |r| r.pattern().into()))],
        ),
        AppleTestParserConfiguration::Xctest => typed("xctest", vec![]),
    }
}
fn ios(c: &IosConfiguration) -> WriterValue {
    let screen = &c.screen_record_configuration;
    let video = &screen.video_configuration;
    let screenshot = &screen.screenshot_configuration;
    typed(
        "iOS",
        vec![
            ("bundle", w_opt(c.bundle.as_ref(), w_bundle)),
            ("deviceProvider", w_device_provider(&c.device_provider)),
            ("devices", w_opt(c.devices_file.as_ref(), w_path)),
            ("ssh", w_ssh(&c.ssh)),
            (
                "workerConnection",
                obj(vec![
                    ("attempts", w_int(c.worker_connection.attempts)),
                    ("backoff", w_duration(&c.worker_connection.backoff)),
                    ("retryInterval", w_duration(&c.worker_connection.retry_interval)),
                    ("lateJoin", WriterValue::Bool(c.worker_connection.late_join)),
                    ("lateJoinCutoff", w_duration(&c.worker_connection.late_join_cutoff)),
                ]),
            ),
            ("xcresult", c.xcresult.writer_value()),
            (
                "screenRecordConfiguration",
                obj(vec![
                    (
                        "preferableRecorderType",
                        w_opt(screen.preferable_recorder_type.as_ref(), |t| w_enum(*t)),
                    ),
                    (
                        "videoConfiguration",
                        obj(vec![
                            ("enabled", WriterValue::Bool(video.enabled)),
                            ("codec", w_enum(video.codec)),
                            ("display", w_enum(video.display)),
                            ("mask", w_enum(video.mask)),
                            (
                                "transcoding",
                                obj(vec![
                                    ("enabled", WriterValue::Bool(video.transcoding.enabled)),
                                    ("ffmpegPath", w_text(&video.transcoding.binary)),
                                    ("size", w_enum(video.transcoding.size)),
                                ]),
                            ),
                        ]),
                    ),
                    (
                        "screenshotConfiguration",
                        obj(vec![
                            ("enabled", WriterValue::Bool(screenshot.enabled)),
                            ("type", w_enum(screenshot.screenshot_type)),
                            ("display", w_enum(screenshot.display)),
                            ("mask", w_enum(screenshot.mask)),
                            ("delay", w_duration(&screenshot.delay)),
                            ("width", w_int(screenshot.width)),
                            ("height", w_int(screenshot.height)),
                        ]),
                    ),
                ]),
            ),
            ("xctestrunEnv", w_xctestrun_env(&c.xctestrun_env)),
            (
                "lifecycle",
                obj(vec![
                    (
                        "shutdownUnused",
                        WriterValue::Bool(c.lifecycle_configuration.shutdown_unused),
                    ),
                    (
                        "onPrepare",
                        w_list(&c.lifecycle_configuration.on_prepare, |a| w_enum(*a)),
                    ),
                    (
                        "onDispose",
                        w_list(&c.lifecycle_configuration.on_dispose, |a| w_enum(*a)),
                    ),
                ]),
            ),
            (
                "permissions",
                obj(vec![
                    ("grant", w_list(&c.permissions.grant, |p| w_enum(*p))),
                    ("lifecycle", w_enum(c.permissions.lifecycle)),
                ]),
            ),
            ("importMedia", w_paths(c.media_files.as_ref())),
            ("timeoutConfiguration", w_apple_timeouts(&c.timeout_configuration)),
            ("threadingConfiguration", w_apple_threading(&c.threading_configuration)),
            ("hideRunnerOutput", WriterValue::Bool(c.hide_runner_output)),
            ("compactOutput", WriterValue::Bool(c.compact_output)),
            ("rsync", obj(vec![("remotePath", w_text(&c.rsync.remote_path))])),
            ("xcodebuildTestArgs", w_map(&c.xcodebuild_test_args)),
            ("dataContainerClear", WriterValue::Bool(c.data_container_clear)),
            (
                "testParserConfiguration",
                w_apple_test_parser(&c.test_parser_configuration),
            ),
            ("deviceLog", WriterValue::Bool(c.device_log)),
            (
                "signing",
                obj(vec![(
                    "testRunnerProvisioningProfile",
                    w_opt(c.signing.test_runner_provisioning_profile.as_ref(), w_text),
                )]),
            ),
            ("skipCopyingDerivedData", WriterValue::Bool(c.skip_copying_derived_data)),
            ("maxBatchesWithoutProgress", w_int(c.max_batches_without_progress)),
        ],
    )
}
fn macos(c: &MacosConfiguration) -> WriterValue {
    typed(
        "macOS",
        vec![
            ("bundle", w_opt(c.bundle.as_ref(), w_bundle)),
            ("deviceProvider", w_device_provider(&c.device_provider)),
            ("devices", w_opt(c.devices_file.as_ref(), w_path)),
            ("ssh", w_ssh(&c.ssh)),
            ("xcresult", c.xcresult.writer_value()),
            ("xctestrunEnv", w_xctestrun_env(&c.xctestrun_env)),
            ("timeoutConfiguration", w_apple_timeouts(&c.timeout_configuration)),
            ("threadingConfiguration", w_apple_threading(&c.threading_configuration)),
            ("hideRunnerOutput", WriterValue::Bool(c.hide_runner_output)),
            ("compactOutput", WriterValue::Bool(c.compact_output)),
            ("rsync", obj(vec![("remotePath", w_text(&c.rsync.remote_path))])),
            ("xcodebuildTestArgs", w_map(&c.xcodebuild_test_args)),
            (
                "testParserConfiguration",
                w_apple_test_parser(&c.test_parser_configuration),
            ),
            (
                "permissions",
                obj(vec![(
                    "grant",
                    WriterValue::Object(
                        c.permissions
                            .grant
                            .iter()
                            .map(|(k, v)| (k.as_str().into(), w_list(v, |p| w_enum(*p))))
                            .collect(),
                    ),
                )]),
            ),
        ],
    )
}
