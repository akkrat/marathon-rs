//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/Configuration.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/AnalyticsConfiguration.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/CIConfiguration.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/FilteringConfiguration.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/OutputConfiguration.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/ScreenRecordingPolicy.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/analytics/Defaults.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/strategy/BatchingStrategyConfiguration.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/strategy/ExecutionStrategyConfiguration.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/strategy/FlakinessStrategyConfiguration.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/strategy/PoolingStrategyConfiguration.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/strategy/RetryStrategyConfiguration.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/strategy/ShardingStrategyConfiguration.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/strategy/SortingStrategyConfiguration.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/vendor/VendorConfiguration.kt.
//!
//! Types only: no `Default` impls carry binding defaults (WP 0.6); the
//! `bind_*` functions in `crate::bind` own every default. Equality is lawful
//! full-field equality with pattern-based regex equality (F7, S1). An `f64`
//! field compares as `java.lang.Double.equals` (`java_double_eq`), which is
//! what Kotlin data-class equality does for a `Double` property, so every
//! type is also `Eq`.

use jcompat::{
    JavaRegex,
    jtime::{Duration, Instant},
};
use std::fmt;
use std::path::PathBuf;

// `validate()` failures use the crate-wide `crate::vendor::ValidationError`
// (WP 2.3a integration): these bodies throw Kotlin `ConfigurationException`,
// so every error here has `ExceptionClass::Configuration`. Binding failures
// use `ConfigurationError` with that class name instead.

/// `java.lang.Double.equals`: `doubleToLongBits` equality, so NaN equals
/// NaN (every NaN collapses to the canonical one) and 0.0 differs from -0.0.
pub fn java_double_eq(a: f64, b: f64) -> bool {
    (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
}
fn java_option_double_eq(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => java_double_eq(a, b),
        (a, b) => a.is_none() && b.is_none(),
    }
}

pub mod analytics {
    use super::Duration;
    /// Port of config/analytics/Defaults.kt.
    #[derive(Clone, Copy, Debug)]
    pub struct Defaults {
        pub success_rate: f64,
        pub duration: Duration,
        pub observed_duration_min_tests: Option<i32>,
    }
    impl PartialEq for Defaults {
        fn eq(&self, other: &Self) -> bool {
            super::java_double_eq(self.success_rate, other.success_rate)
                && self.duration == other.duration
                && self.observed_duration_min_tests == other.observed_duration_min_tests
        }
    }
    impl Eq for Defaults {}
}
pub use analytics::Defaults;

/// Constructor fields follow Configuration.kt:19-54.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Configuration {
    pub name: String,
    pub output_dir: PathBuf,
    pub output_configuration: OutputConfiguration,
    pub analytics_configuration: AnalyticsConfiguration,
    pub pooling_strategy: PoolingStrategyConfiguration,
    pub sharding_strategy: ShardingStrategyConfiguration,
    pub sorting_strategy: SortingStrategyConfiguration,
    pub batching_strategy: BatchingStrategyConfiguration,
    pub flakiness_strategy: FlakinessStrategyConfiguration,
    pub retry_strategy: RetryStrategyConfiguration,
    pub filtering_configuration: FilteringConfiguration,
    pub ignore_failures: bool,
    pub execution_strategy: ExecutionStrategyConfiguration,
    pub is_code_coverage_enabled: bool,
    pub uncompleted_test_retry_quota: i32,
    pub include_serial_regexes: Vec<JavaRegex>,
    pub exclude_serial_regexes: Vec<JavaRegex>,
    pub test_batch_timeout_millis: i64,
    pub test_output_timeout_millis: i64,
    pub debug: bool,
    pub screen_recording_policy: ScreenRecordingPolicy,
    pub vendor_configuration: VendorConfiguration,
    pub analytics_tracking: bool,
    pub bugsnag_reporting: bool,
    pub device_initialization_timeout_millis: i64,
    pub ci_configuration: CiConfiguration,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputConfiguration {
    pub max_path: i32,
    pub max_filename: i32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenRecordingPolicy {
    OnFailure,
    OnAny,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CiConfiguration {
    Auto,
    None,
    Teamcity,
}
use crate::vendor::ValidationError;
use crate::vendor::{AndroidConfiguration, IosConfiguration, MacosConfiguration};

/// watchOS/tvOS/visionOS bind to Ios. No core traits appear in configuration.
/// The unboxed variants are the 0.6-frozen shape; boxing would change every
/// consumer's API for a once-per-run value.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VendorConfiguration {
    Android(AndroidConfiguration),
    Ios(IosConfiguration),
    Macos(MacosConfiguration),
    Stub,
    Empty,
}

// ---- Analytics (AnalyticsConfiguration.kt) --------------------------------

#[derive(Clone, PartialEq, Eq)]
pub enum AnalyticsConfiguration {
    Disabled,
    InfluxDb(InfluxDbConfiguration),
    InfluxDb2(InfluxDb2Configuration),
    Graphite(GraphiteConfiguration),
}
#[derive(Clone, PartialEq, Eq)]
pub struct InfluxDbConfiguration {
    pub url: String,
    pub user: String,
    pub password: String,
    pub db_name: String,
    pub retention_policy_configuration: RetentionPolicyConfiguration,
    pub defaults: Defaults,
    pub read_only: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionPolicyConfiguration {
    pub name: String,
    pub duration: String,
    pub shard_duration: String,
    pub replication_factor: i32,
    pub is_default: bool,
}
impl RetentionPolicyConfiguration {
    /// The Kotlin companion `RetentionPolicyConfiguration.default`.
    pub fn default_policy() -> Self {
        Self {
            name: "rpMarathon".into(),
            duration: "30d".into(),
            shard_duration: "30m".into(),
            replication_factor: 2,
            is_default: true,
        }
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct InfluxDb2Configuration {
    pub url: String,
    pub token: String,
    pub organization: String,
    pub bucket: String,
    pub retention_policy_configuration: InfluxDb2RetentionPolicyConfiguration,
    pub defaults: Defaults,
    pub read_only: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfluxDb2RetentionPolicyConfiguration {
    pub every_seconds: i32,
    pub shard_group_duration_seconds: i64,
}
impl InfluxDb2RetentionPolicyConfiguration {
    /// The Kotlin companion `RetentionPolicyConfiguration.default` (v2).
    pub fn default_policy() -> Self {
        Self {
            every_seconds: 86_400 * 30,
            shard_group_duration_seconds: 0,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphiteConfiguration {
    pub host: String,
    pub port: Option<i32>,
    pub prefix: Option<String>,
    pub defaults: Defaults,
    pub read_only: bool,
}

// Secrets are masked in Debug, mirroring the Kotlin `toString` shapes
// (AnalyticsConfiguration.kt:44-46,67-69; WP 2.2 decision 6). Graphite
// masks nothing in Kotlin, so it keeps the derived Debug above.
impl fmt::Debug for AnalyticsConfiguration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => f.write_str("Disabled"),
            Self::InfluxDb(c) => write!(f, "{c:?}"),
            Self::InfluxDb2(c) => write!(f, "{c:?}"),
            Self::Graphite(c) => write!(f, "{c:?}"),
        }
    }
}
impl fmt::Debug for InfluxDbConfiguration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "InfluxDbConfiguration(url='*****', user='*****', password='*****', dbName='{}', retentionPolicyConfiguration={:?})",
            self.db_name.to_owned(),
            self.retention_policy_configuration
        )
    }
}
impl fmt::Debug for InfluxDb2Configuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "InfluxDb2Configuration(url='*****', token='*****', organization='{}', bucket='{}', retentionPolicyConfiguration={:?})",
            self.organization.to_owned(),
            self.bucket.to_owned(),
            self.retention_policy_configuration
        )
    }
}

// ---- Strategies (config/strategy/*.kt) -------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoolingStrategyConfiguration {
    Omni,
    Abi,
    Manufacturer,
    Model,
    OperatingSystemVersion,
    Combo(Vec<PoolingStrategyConfiguration>),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShardingStrategyConfiguration {
    Count { count: i32 },
    Parallel,
}
#[derive(Clone, Debug)]
pub enum SortingStrategyConfiguration {
    ExecutionTime { percentile: f64, time_limit: Instant },
    NoSorting,
    RandomOrder,
    SuccessRate { time_limit: Instant, ascending: bool },
}
impl PartialEq for SortingStrategyConfiguration {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::ExecutionTime { percentile, time_limit },
                Self::ExecutionTime {
                    percentile: p,
                    time_limit: t,
                },
            ) => java_double_eq(*percentile, *p) && time_limit == t,
            (Self::NoSorting, Self::NoSorting) | (Self::RandomOrder, Self::RandomOrder) => true,
            (
                Self::SuccessRate { time_limit, ascending },
                Self::SuccessRate {
                    time_limit: t,
                    ascending: a,
                },
            ) => time_limit == t && ascending == a,
            _ => false,
        }
    }
}
impl Eq for SortingStrategyConfiguration {}
#[derive(Clone, Debug)]
pub enum BatchingStrategyConfiguration {
    FixedSize {
        size: i32,
        duration_millis: Option<i64>,
        percentile: Option<f64>,
        time_limit: Option<Instant>,
        last_mile_length: i32,
    },
    ClassName,
    Isolate,
}
impl PartialEq for BatchingStrategyConfiguration {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::FixedSize {
                    size,
                    duration_millis,
                    percentile,
                    time_limit,
                    last_mile_length,
                },
                Self::FixedSize {
                    size: s,
                    duration_millis: d,
                    percentile: p,
                    time_limit: t,
                    last_mile_length: l,
                },
            ) => {
                size == s
                    && duration_millis == d
                    && java_option_double_eq(*percentile, *p)
                    && time_limit == t
                    && last_mile_length == l
            }
            (Self::ClassName, Self::ClassName) | (Self::Isolate, Self::Isolate) => true,
            _ => false,
        }
    }
}
impl Eq for BatchingStrategyConfiguration {}
#[derive(Clone, Debug)]
pub enum FlakinessStrategyConfiguration {
    Ignore,
    Probability {
        min_success_rate: f64,
        max_count: i32,
        time_limit: Instant,
    },
}
impl PartialEq for FlakinessStrategyConfiguration {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ignore, Self::Ignore) => true,
            (
                Self::Probability {
                    min_success_rate,
                    max_count,
                    time_limit,
                },
                Self::Probability {
                    min_success_rate: r,
                    max_count: m,
                    time_limit: t,
                },
            ) => java_double_eq(*min_success_rate, *r) && max_count == m && time_limit == t,
            _ => false,
        }
    }
}
impl Eq for FlakinessStrategyConfiguration {}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetryStrategyConfiguration {
    FixedQuota {
        total_allowed_retry_quota: i32,
        retry_per_test_quota: i32,
    },
    NoRetry,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionStrategyConfiguration {
    pub mode: ExecutionMode,
    pub fast: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionMode {
    AnySuccess,
    AllSuccess,
}

// ---- Filtering (FilteringConfiguration.kt) ----------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilteringConfiguration {
    pub allowlist: Vec<TestFilterConfiguration>,
    pub blocklist: Vec<TestFilterConfiguration>,
}
impl FilteringConfiguration {
    /// Allowlist then blocklist, the order `LogicalConfigurationValidator`
    /// uses; WP 2.4 owns when this runs.
    pub fn validate(&self) -> Result<(), ValidationError> {
        for filter in self.allowlist.iter().chain(&self.blocklist) {
            filter.validate()?;
        }
        Ok(())
    }
}

/// The shared `(regex?, values?, file?, enabled)` shape of seven variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegexValuesFileFilter {
    pub regex: Option<JavaRegex>,
    pub values: Option<Vec<String>>,
    pub file: Option<PathBuf>,
    pub enabled: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestPackageFilter {
    pub regex: Option<JavaRegex>,
    pub values: Option<Vec<String>>,
    pub file: Option<PathBuf>,
    pub subpackages: bool,
    pub enabled: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompositionOperation {
    Union,
    Intersection,
    Subtract,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestFilterConfiguration {
    SimpleClassname(RegexValuesFileFilter),
    SimpleTestname(RegexValuesFileFilter),
    FullyQualifiedClassname(RegexValuesFileFilter),
    FullyQualifiedTestname(RegexValuesFileFilter),
    TestMethod(RegexValuesFileFilter),
    Annotation(RegexValuesFileFilter),
    TestPackage(TestPackageFilter),
    AnnotationData {
        name_regex: JavaRegex,
        value_regex: JavaRegex,
        enabled: bool,
    },
    Fragmentation {
        index: i32,
        count: i32,
        enabled: bool,
    },
    Composition {
        filters: Vec<TestFilterConfiguration>,
        op: CompositionOperation,
        enabled: bool,
    },
    Allure {
        enabled: bool,
    },
}

fn validate_regex_values_file(regex: bool, values: bool, file: bool, simple_name: &str) -> Result<(), ValidationError> {
    let i = [regex, values, file].iter().filter(|b| **b).count();
    if i > 1 {
        return Err(ValidationError::configuration(format!(
            "Only one of [regex,values,file] can be specified for {simple_name}"
        )));
    }
    if i == 0 {
        return Err(ValidationError::configuration(format!(
            "At least one of [regex,values,file] should be specified for {simple_name}"
        )));
    }
    Ok(())
}

impl TestFilterConfiguration {
    pub fn enabled(&self) -> bool {
        match self {
            Self::SimpleClassname(f)
            | Self::SimpleTestname(f)
            | Self::FullyQualifiedClassname(f)
            | Self::FullyQualifiedTestname(f)
            | Self::TestMethod(f)
            | Self::Annotation(f) => f.enabled,
            Self::TestPackage(f) => f.enabled,
            Self::AnnotationData { enabled, .. }
            | Self::Fragmentation { enabled, .. }
            | Self::Composition { enabled, .. }
            | Self::Allure { enabled } => *enabled,
        }
    }
    /// Port of each variant's `validate()` in FilteringConfiguration.kt.
    /// Messages keep the Kotlin class simple name verbatim.
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::SimpleClassname(f) => validate_regex_values_file(
                f.regex.is_some(),
                f.values.is_some(),
                f.file.is_some(),
                "SimpleClassnameFilterConfiguration",
            ),
            Self::SimpleTestname(f) => validate_regex_values_file(
                f.regex.is_some(),
                f.values.is_some(),
                f.file.is_some(),
                "SimpleTestnameFilterConfiguration",
            ),
            Self::FullyQualifiedClassname(f) => validate_regex_values_file(
                f.regex.is_some(),
                f.values.is_some(),
                f.file.is_some(),
                "FullyQualifiedClassnameFilterConfiguration",
            ),
            Self::FullyQualifiedTestname(f) => validate_regex_values_file(
                f.regex.is_some(),
                f.values.is_some(),
                f.file.is_some(),
                "FullyQualifiedTestnameFilterConfiguration",
            ),
            Self::TestMethod(f) => validate_regex_values_file(
                f.regex.is_some(),
                f.values.is_some(),
                f.file.is_some(),
                "TestMethodFilterConfiguration",
            ),
            Self::Annotation(f) => validate_regex_values_file(
                f.regex.is_some(),
                f.values.is_some(),
                f.file.is_some(),
                "AnnotationFilterConfiguration",
            ),
            Self::TestPackage(f) => validate_regex_values_file(
                f.regex.is_some(),
                f.values.is_some(),
                f.file.is_some(),
                "TestPackageFilterConfiguration",
            ),
            Self::AnnotationData { .. } | Self::Allure { .. } => Ok(()),
            Self::Fragmentation { index, count, .. } => {
                if *index < 0 {
                    return Err(ValidationError::configuration(format!(
                        "Fragment index [{index}] should be >= 0"
                    )));
                }
                if *count < 0 {
                    return Err(ValidationError::configuration(format!(
                        "Fragment count [{count}] should be >= 0"
                    )));
                }
                if *index >= *count {
                    return Err(ValidationError::configuration(format!(
                        "Fragment index [{index}] should be less than count [{count}]"
                    )));
                }
                Ok(())
            }
            Self::Composition { filters, .. } => {
                for filter in filters {
                    filter.validate()?;
                }
                Ok(())
            }
        }
    }
}

// The WP 0.6 `MarathonDevices` slot is filled by WP 2.3b in
// `crate::devices` (the P2 types live there, not in this P1 schema file).
