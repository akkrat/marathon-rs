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
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/yaml/ExecutionTimeSortingStrategyConfigurationDeserializer.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/yaml/FixedSizeBatchingStrategyConfigurationDeserializer.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/yaml/ProbabilityBasedFlakinessStrategyConfigurationDeserializer.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/yaml/InfluxDbConfigurationDeserializer.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/yaml/GraphiteConfigurationDeserializer.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/yaml/RetentionPolicyConfigurationDeserializer.kt.
//!
//! Hand-written `bind_*` schema functions over the WP 2.1 binder; every
//! function is `fn(Value) -> Result<Option<T>>` so it composes with
//! `Field`/`list`/`polymorphic`. Defaults live here, never in the types.

use crate::{
    AnalyticsConfiguration, BatchingStrategyConfiguration, CiConfiguration, CompositionOperation, Configuration,
    Defaults, ExecutionMode, ExecutionStrategyConfiguration, FilteringConfiguration, FlakinessStrategyConfiguration,
    GraphiteConfiguration, InfluxDb2Configuration, InfluxDb2RetentionPolicyConfiguration, InfluxDbConfiguration,
    OutputConfiguration, PoolingStrategyConfiguration, RegexValuesFileFilter, RetentionPolicyConfiguration,
    RetryStrategyConfiguration, ScreenRecordingPolicy, ShardingStrategyConfiguration, SortingStrategyConfiguration,
    TestFilterConfiguration, TestPackageFilter, VendorConfiguration,
    binder::{Field, INVALID_FORMAT, JSON_MAPPING, MISMATCHED, Result, Value},
    binding::{BindingCause, ConfigurationError},
};
use jcompat::{
    JavaRegex,
    jtime::{Duration, Instant, duration_of},
};
use marathon_error::FailureKind;
use std::path::PathBuf;

pub const CONFIGURATION_EXCEPTION: FailureKind = FailureKind::Config(marathon_error::ConfigKind::Rejected);

fn config_error(value: &Value<'_>, message: impl Into<String>) -> Box<ConfigurationError> {
    let mut error = value.error(CONFIGURATION_EXCEPTION, message.into());
    if let BindingCause::Coercion(m) = &error.cause {
        error.cause = BindingCause::Custom(m.clone());
    }
    error
}

fn defaults_default() -> Defaults {
    Defaults {
        success_rate: 0.0,
        duration: duration_of(300, 0).expect("300 seconds is a valid Duration"),
        observed_duration_min_tests: None,
    }
}

/// A singleton subtype: any field other than the consumed `type` is unknown.
/// Shared with the vendor binders (WP 2.3a) instead of being copied there.
pub(crate) fn singleton(value: Value<'_>) -> Result<()> {
    let mut object = value.object()?;
    while let Some((_, child)) = object.next_field()? {
        child.unknown_now()?;
    }
    Ok(())
}

/// S4 (D9): a `null` collection element is rejected at bind time instead of
/// threading `Option` elements downstream. Kotlin accepts it and NPEs later.
/// Shared with the vendor binders (WP 2.3a) instead of being copied there.
pub(crate) fn list_non_null<T>(
    value: &Value<'_>,
    mut bind: impl FnMut(Value<'_>) -> Result<Option<T>>,
) -> Result<Option<Vec<T>>> {
    value.list(|elem| {
        let bound = bind(elem.clone())?;
        bound.ok_or_else(|| elem.error(MISMATCHED, "D9: null element in collection"))
    })
}

/// S4 narrowed (R1-claude-2): only for filter `values` lists. Kotlin's
/// runtime filters reach them solely through contains/intersect checks
/// (`SingleValueTestFilter`/`AnnotationFilter`), where a null element
/// never matches — so dropping it changes nothing downstream. One
/// exception (WP 2.2 review round 2): `TestPackageFilter` with
/// `subpackages: true` tests `startsWith("$it.")`, so a null element
/// becomes `"null."` and would match packages starting with `null.` —
/// effectively unreachable, since `null` is a reserved word in Java and
/// Kotlin. Everywhere Kotlin later dereferences the element,
/// `list_non_null` still rejects.
pub(crate) fn list_drop_nulls<T>(
    value: &Value<'_>,
    bind: impl FnMut(Value<'_>) -> Result<Option<T>>,
) -> Result<Option<Vec<T>>> {
    value
        .list(bind)
        .map(|opt| opt.map(|xs| xs.into_iter().flatten().collect()))
}

fn string_list(value: &Value<'_>) -> Result<Option<Vec<String>>> {
    // Filter `values` only (see list_drop_nulls).
    list_drop_nulls(value, |v| v.string())
}
fn regex_list(value: &Value<'_>) -> Result<Option<Vec<JavaRegex>>> {
    list_non_null(value, |v| v.regex())
}

// ---- Simple sections --------------------------------------------------------

pub fn bind_output_configuration(value: Value<'_>) -> Result<Option<OutputConfiguration>> {
    value.nullable(|v| {
        let mut max_path = Field::primitive(&v, 0);
        let mut max_filename = Field::primitive(&v, 255);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "maxPath" => {
                    let bound = child.int()?;
                    max_path.set(child, Some(bound));
                }
                "maxFilename" => {
                    let bound = child.int()?;
                    max_filename.set(child, Some(bound));
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(OutputConfiguration {
            max_path: max_path.required("maxPath")?,
            max_filename: max_filename.required("maxFilename")?,
        })
    })
}

pub fn bind_screen_recording_policy(value: Value<'_>) -> Result<Option<ScreenRecordingPolicy>> {
    Ok(value
        .enumeration(&["ON_FAILURE", "ON_ANY"])?
        .map(|i| [ScreenRecordingPolicy::OnFailure, ScreenRecordingPolicy::OnAny][i]))
}

pub fn bind_ci_configuration(value: Value<'_>) -> Result<Option<CiConfiguration>> {
    value.polymorphic(|id, v| {
        let ci = match id {
            "auto" => CiConfiguration::Auto,
            "none" => CiConfiguration::None,
            "teamcity" => CiConfiguration::Teamcity,
            _ => return None,
        };
        Some(singleton(v).map(|()| ci))
    })
}

pub fn bind_execution_strategy(value: Value<'_>) -> Result<Option<ExecutionStrategyConfiguration>> {
    value.nullable(|v| {
        let mut mode = Field::reference(&v, Some(ExecutionMode::AnySuccess));
        let mut fast = Field::primitive(&v, true);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "mode" => {
                    let bound = child
                        .enumeration(&["ANY_SUCCESS", "ALL_SUCCESS"])?
                        .map(|i| [ExecutionMode::AnySuccess, ExecutionMode::AllSuccess][i]);
                    mode.set(child, bound);
                }
                "fast" => {
                    let bound = child.boolean()?;
                    fast.set(child, Some(bound));
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(ExecutionStrategyConfiguration {
            mode: mode.required("mode")?,
            fast: fast.required("fast")?,
        })
    })
}

pub fn bind_defaults(value: Value<'_>) -> Result<Option<Defaults>> {
    value.nullable(|v| {
        let mut success_rate = Field::primitive(&v, 0.0_f64);
        let mut duration = Field::reference(&v, Some(defaults_default().duration));
        let mut observed = Field::nullable(&v, None::<i32>);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "successRate" => {
                    let bound = child.double()?;
                    success_rate.set(child, Some(bound));
                }
                "duration" => {
                    let bound = child.duration()?;
                    duration.set(child, bound);
                }
                "observedDurationMinTests" => {
                    // Kotlin `Int?`: Jackson boxed semantics, so "", blank
                    // and quoted "null" bind to null, not Some(0).
                    let bound = child.int_boxed()?;
                    observed.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(Defaults {
            success_rate: success_rate.required("successRate")?,
            duration: duration.required("duration")?,
            observed_duration_min_tests: observed.optional(),
        })
    })
}

/// `readTreeAsValue(Defaults)` inside the lenient deserializers: an explicit
/// null is a MismatchedInputException there, not the creator default.
fn bind_defaults_lenient_child(value: Value<'_>) -> Result<Defaults> {
    if value.is_null()? {
        return Err(value.error(MISMATCHED, "cannot deserialize Defaults from null"));
    }
    bind_defaults(value.clone())?.ok_or_else(|| value.error(MISMATCHED, "cannot deserialize Defaults from null"))
}

// ---- Lenient timeLimit (the three strategy deserializers) -------------------

/// `treeToValueOrNull`: only InvalidFormatException becomes null; every
/// other failure (containers, booleans) propagates as in Kotlin.
fn instant_or_null(value: &Value<'_>) -> Result<Option<Instant>> {
    match value.instant() {
        Ok(v) => Ok(v),
        Err(e) if e.kind == INVALID_FORMAT => Ok(None),
        Err(e) => Err(e),
    }
}
fn duration_or_null(value: &Value<'_>) -> Result<Option<Duration>> {
    match value.duration() {
        Ok(v) => Ok(v),
        Err(e) if e.kind == INVALID_FORMAT => Ok(None),
        Err(e) => Err(e),
    }
}
/// Try Instant, then Duration added to the one `BindingContext.now` (S2).
/// `None` means neither parsed; overflow is a plain binding error (S7).
fn time_limit(value: &Value<'_>) -> Result<Option<Instant>> {
    if let Some(instant) = instant_or_null(value)? {
        return Ok(Some(instant));
    }
    if let Some(duration) = duration_or_null(value)? {
        let instant = value
            .context()
            .now
            .checked_add(duration)
            .ok_or_else(|| value.error(JSON_MAPPING, "D9: now + Duration overflows Instant"))?;
        return Ok(Some(instant));
    }
    Ok(None)
}

// ---- Strategies --------------------------------------------------------------

pub fn bind_pooling_strategy(value: Value<'_>) -> Result<Option<PoolingStrategyConfiguration>> {
    value.polymorphic(|id, v| {
        use PoolingStrategyConfiguration as P;
        match id {
            "omni" => Some(singleton(v).map(|()| P::Omni)),
            "abi" => Some(singleton(v).map(|()| P::Abi)),
            "manufacturer" => Some(singleton(v).map(|()| P::Manufacturer)),
            "device-model" => Some(singleton(v).map(|()| P::Model)),
            "os-version" => Some(singleton(v).map(|()| P::OperatingSystemVersion)),
            "combo" => Some((|| {
                let mut list = Field::reference(&v, None::<Vec<P>>);
                let mut object = v.object()?;
                while let Some((name, child)) = object.next_field()? {
                    match name.as_str() {
                        "list" => {
                            let bound = list_non_null(&child, bind_pooling_strategy)?;
                            list.set(child, bound);
                        }
                        _ => child.unknown(false)?,
                    }
                }
                Ok(P::Combo(list.required("list")?))
            })()),
            _ => None,
        }
    })
}

pub fn bind_sharding_strategy(value: Value<'_>) -> Result<Option<ShardingStrategyConfiguration>> {
    value.polymorphic(|id, v| match id {
        "parallel" => Some(singleton(v).map(|()| ShardingStrategyConfiguration::Parallel)),
        "count" => Some((|| {
            let mut count = Field::primitive(&v, 0);
            let mut object = v.object()?;
            while let Some((name, child)) = object.next_field()? {
                match name.as_str() {
                    "count" => {
                        let bound = child.int()?;
                        count.set(child, Some(bound));
                    }
                    _ => child.unknown(false)?,
                }
            }
            Ok(ShardingStrategyConfiguration::Count {
                count: count.required("count")?,
            })
        })()),
        _ => None,
    })
}

fn bind_execution_time(value: Value<'_>) -> Result<SortingStrategyConfiguration> {
    // Lenient: readTree + findValue; "Invalid sorting strategy" (null tree)
    // is unreachable here because read_tree errors on those inputs instead.
    let tree = value.read_tree()?;
    let percentile = tree
        .find_value("percentile")?
        .map(|v| v.as_double())
        .ok_or_else(|| config_error(&value, "Missing percentile value"))?;
    let time = tree
        .find_value("timeLimit")?
        .ok_or_else(|| config_error(&value, "Missing time limit value"))?;
    let instant = time_limit(&time)?.ok_or_else(|| config_error(&value, "bbb"))?;
    Ok(SortingStrategyConfiguration::ExecutionTime {
        percentile,
        time_limit: instant,
    })
}

pub fn bind_sorting_strategy(value: Value<'_>) -> Result<Option<SortingStrategyConfiguration>> {
    value.polymorphic(|id, v| {
        use SortingStrategyConfiguration as S;
        match id {
            "execution-time" => Some(bind_execution_time(v)),
            "no-sorting" => Some(singleton(v).map(|()| S::NoSorting)),
            "random-order" => Some(singleton(v).map(|()| S::RandomOrder)),
            "success-rate" => Some((|| {
                let mut time = Field::reference(&v, None::<Instant>);
                let mut ascending = Field::primitive(&v, false);
                let mut object = v.object()?;
                while let Some((name, child)) = object.next_field()? {
                    match name.as_str() {
                        "timeLimit" => {
                            let bound = child.instant()?;
                            time.set(child, bound);
                        }
                        "ascending" => {
                            let bound = child.boolean()?;
                            ascending.set(child, Some(bound));
                        }
                        _ => child.unknown(false)?,
                    }
                }
                Ok(S::SuccessRate {
                    time_limit: time.required("timeLimit")?,
                    ascending: ascending.required("ascending")?,
                })
            })()),
            _ => None,
        }
    })
}

fn bind_fixed_size(value: Value<'_>) -> Result<BatchingStrategyConfiguration> {
    let tree = value.read_tree()?;
    let size = tree
        .find_value("size")?
        .map(|v| v.as_int())
        .ok_or_else(|| config_error(&value, "Missing size value"))?;
    let duration_millis = tree.find_value("durationMillis")?.map(|v| v.as_long());
    let percentile = tree.find_value("percentile")?.map(|v| v.as_double());
    let last_mile_length = tree.find_value("lastMileLength")?.map(|v| v.as_int()).unwrap_or(0);
    // F2: any double failure of the optional timeLimit silently yields null;
    // non-InvalidFormat failures still propagate through time_limit.
    let time = match tree.find_value("timeLimit")? {
        Some(node) => time_limit(&node)?,
        None => None,
    };
    Ok(BatchingStrategyConfiguration::FixedSize {
        size,
        duration_millis,
        percentile,
        time_limit: time,
        last_mile_length,
    })
}

pub fn bind_batching_strategy(value: Value<'_>) -> Result<Option<BatchingStrategyConfiguration>> {
    value.polymorphic(|id, v| {
        use BatchingStrategyConfiguration as B;
        match id {
            "fixed-size" => Some(bind_fixed_size(v)),
            "isolate" => Some(singleton(v).map(|()| B::Isolate)),
            "class-name" => Some(singleton(v).map(|()| B::ClassName)),
            _ => None,
        }
    })
}

fn bind_probability(value: Value<'_>) -> Result<FlakinessStrategyConfiguration> {
    let tree = value.read_tree()?;
    let min_success_rate = tree
        .find_value("minSuccessRate")?
        .map(|v| v.as_double())
        .ok_or_else(|| config_error(&value, "Missing minimum success rate value"))?;
    let max_count = tree
        .find_value("maxCount")?
        .map(|v| v.as_int())
        .ok_or_else(|| config_error(&value, "Missing maximum count value"))?;
    let time = tree
        .find_value("timeLimit")?
        .ok_or_else(|| config_error(&value, "Missing time limit value"))?;
    let rendered = time.node_string();
    let instant = time_limit(&time)?
        .ok_or_else(|| config_error(&value, format!("Unable to deserialize {rendered} into Instant")))?;
    Ok(FlakinessStrategyConfiguration::Probability {
        min_success_rate,
        max_count,
        time_limit: instant,
    })
}

pub fn bind_flakiness_strategy(value: Value<'_>) -> Result<Option<FlakinessStrategyConfiguration>> {
    value.polymorphic(|id, v| match id {
        "ignore" => Some(singleton(v).map(|()| FlakinessStrategyConfiguration::Ignore)),
        "probability" => Some(bind_probability(v)),
        _ => None,
    })
}

pub fn bind_retry_strategy(value: Value<'_>) -> Result<Option<RetryStrategyConfiguration>> {
    value.polymorphic(|id, v| match id {
        "no-retry" => Some(singleton(v).map(|()| RetryStrategyConfiguration::NoRetry)),
        "fixed-quota" => Some((|| {
            let mut total = Field::primitive(&v, 200);
            let mut per_test = Field::primitive(&v, 3);
            let mut object = v.object()?;
            while let Some((name, child)) = object.next_field()? {
                match name.as_str() {
                    "totalAllowedRetryQuota" => {
                        let bound = child.int()?;
                        total.set(child, Some(bound));
                    }
                    "retryPerTestQuota" => {
                        let bound = child.int()?;
                        per_test.set(child, Some(bound));
                    }
                    _ => child.unknown(false)?,
                }
            }
            Ok(RetryStrategyConfiguration::FixedQuota {
                total_allowed_retry_quota: total.required("totalAllowedRetryQuota")?,
                retry_per_test_quota: per_test.required("retryPerTestQuota")?,
            })
        })()),
        _ => None,
    })
}

// ---- Analytics ----------------------------------------------------------------

/// Lenient v1 retention policy: direct `get` + non-throwing accessors.
fn retention_policy(value: Value<'_>) -> Result<RetentionPolicyConfiguration> {
    let tree = value.read_tree()?;
    let name = tree.get("name")?.map(|v| v.as_text());
    let duration = tree.get("duration")?.map(|v| v.as_text());
    let shard_duration = tree.get("shardDuration")?.map(|v| v.as_text());
    let replication_factor = tree.get("replicationFactor")?.map(|v| v.as_int());
    let is_default = tree.get("isDefault")?.map(|v| v.as_boolean());
    let missing = |field: &str| {
        config_error(
            &value,
            format!("RetentionPolicyConfigurationDeserializer: {field} should be specified"),
        )
    };
    Ok(RetentionPolicyConfiguration {
        name: name.ok_or_else(|| missing("name"))?,
        duration: duration.ok_or_else(|| missing("duration"))?,
        shard_duration: shard_duration.ok_or_else(|| missing("shardDuration"))?,
        replication_factor: replication_factor.ok_or_else(|| missing("replicationFactor"))?,
        is_default: is_default.ok_or_else(|| missing("isDefault"))?,
    })
}
pub fn bind_retention_policy_configuration(value: Value<'_>) -> Result<Option<RetentionPolicyConfiguration>> {
    retention_policy(value).map(Some)
}

pub fn bind_influxdb2_retention_policy_configuration(
    value: Value<'_>,
) -> Result<Option<InfluxDb2RetentionPolicyConfiguration>> {
    value.nullable(|v| {
        let mut every = Field::primitive(&v, 0);
        let mut shard = Field::primitive(&v, 0_i64);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "everySeconds" => {
                    let bound = child.int()?;
                    every.set(child, Some(bound));
                }
                "shardGroupDurationSeconds" => {
                    let bound = child.long()?;
                    shard.set(child, Some(bound));
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(InfluxDb2RetentionPolicyConfiguration {
            every_seconds: every.required("everySeconds")?,
            shard_group_duration_seconds: shard.required("shardGroupDurationSeconds")?,
        })
    })
}

/// Lenient InfluxDB v1. `readOnly` is never read, so it stays false (F3).
/// S6: required-field checks run before the nested binds.
fn influx_db(value: Value<'_>) -> Result<InfluxDbConfiguration> {
    let tree = value.read_tree()?;
    let url = tree.get("url")?.map(|v| v.as_text());
    let user = tree.get("user")?.map(|v| v.as_text());
    let password = tree.get("password")?.map(|v| v.as_text());
    let db_name = tree.get("dbName")?.map(|v| v.as_text());
    let missing = |field: &str| {
        config_error(
            &value,
            format!("InfluxDbConfigurationDeserializer: {field} should be specified"),
        )
    };
    let url = url.ok_or_else(|| missing("url"))?;
    let user = user.ok_or_else(|| missing("user"))?;
    let password = password.ok_or_else(|| missing("password"))?;
    let db_name = db_name.ok_or_else(|| missing("dbName"))?;
    let retention = match tree.get("retentionPolicyConfiguration")? {
        Some(node) => retention_policy(node)?,
        None => RetentionPolicyConfiguration::default_policy(),
    };
    let defaults = match tree.get("defaults")? {
        Some(node) => bind_defaults_lenient_child(node)?,
        None => defaults_default(),
    };
    Ok(InfluxDbConfiguration {
        url,
        user,
        password,
        db_name,
        retention_policy_configuration: retention,
        defaults,
        read_only: false,
    })
}

/// Kotlin `String.toIntOrNull()` restricted to ASCII (S5): no trimming,
/// an optional sign, then digits; Rust's `i32` parse is exactly that.
fn to_int_or_null_ascii(text: &str) -> Option<i32> {
    if !text.is_ascii() {
        return None;
    }
    text.parse::<i32>().ok()
}

/// Lenient Graphite. `readOnly` is never read, so it stays false (F3).
fn graphite(value: Value<'_>) -> Result<GraphiteConfiguration> {
    let tree = value.read_tree()?;
    let host = tree.get("host")?.map(|v| v.as_text());
    let port_string = tree.get("port")?.map(|v| v.as_text());
    let prefix = tree.get("prefix")?.map(|v| v.as_text());
    // S6 + Kotlin check order: host, then port, then prefix, then nested.
    let host =
        host.ok_or_else(|| config_error(&value, "GraphiteConfigurationDeserializer: host should be specified"))?;
    let port = port_string.as_ref().map(|s| to_int_or_null_ascii(&s.to_owned()));
    if port_string.is_some() && port == Some(None) {
        return Err(config_error(
            &value,
            "GraphiteConfigurationDeserializer: port should be a number, e.g. 2003",
        ));
    }
    if prefix.as_ref().is_some_and(|p| p.is_empty()) {
        return Err(config_error(
            &value,
            "GraphiteConfigurationDeserializer: prefix cannot be empty",
        ));
    }
    let defaults = match tree.get("defaults")? {
        Some(node) => bind_defaults_lenient_child(node)?,
        None => defaults_default(),
    };
    Ok(GraphiteConfiguration {
        host,
        port: port.flatten(),
        prefix,
        defaults,
        read_only: false,
    })
}

fn influx_db2(value: Value<'_>) -> Result<InfluxDb2Configuration> {
    let mut url = Field::reference(&value, None::<String>);
    let mut token = Field::reference(&value, None::<String>);
    let mut organization = Field::reference(&value, None::<String>);
    let mut bucket = Field::reference(&value, None::<String>);
    let mut retention = Field::reference(&value, Some(InfluxDb2RetentionPolicyConfiguration::default_policy()));
    let mut defaults = Field::reference(&value, Some(defaults_default()));
    let mut read_only = Field::primitive(&value, false);
    let mut object = value.object()?;
    while let Some((name, child)) = object.next_field()? {
        match name.as_str() {
            "url" => {
                let bound = child.string()?;
                url.set(child, bound);
            }
            "token" => {
                let bound = child.string()?;
                token.set(child, bound);
            }
            "organization" => {
                let bound = child.string()?;
                organization.set(child, bound);
            }
            "bucket" => {
                let bound = child.string()?;
                bucket.set(child, bound);
            }
            "retentionPolicyConfiguration" => {
                let bound = bind_influxdb2_retention_policy_configuration(child.clone())?;
                retention.set(child, bound);
            }
            "defaults" => {
                let bound = bind_defaults(child.clone())?;
                defaults.set(child, bound);
            }
            "readOnly" => {
                let bound = child.boolean()?;
                read_only.set(child, Some(bound));
            }
            _ => child.unknown(false)?,
        }
    }
    Ok(InfluxDb2Configuration {
        url: url.required("url")?,
        token: token.required("token")?,
        organization: organization.required("organization")?,
        bucket: bucket.required("bucket")?,
        retention_policy_configuration: retention.required("retentionPolicyConfiguration")?,
        defaults: defaults.required("defaults")?,
        read_only: read_only.required("readOnly")?,
    })
}

pub fn bind_analytics_configuration(value: Value<'_>) -> Result<Option<AnalyticsConfiguration>> {
    value.polymorphic(|id, v| {
        use AnalyticsConfiguration as A;
        match id {
            "disabled" => Some(singleton(v).map(|()| A::Disabled)),
            "influxdb" => Some(influx_db(v).map(A::InfluxDb)),
            "influxdb2" => Some(influx_db2(v).map(A::InfluxDb2)),
            "graphite" => Some(graphite(v).map(A::Graphite)),
            _ => None,
        }
    })
}

// ---- Filtering ------------------------------------------------------------------

fn regex_values_file(value: Value<'_>) -> Result<RegexValuesFileFilter> {
    let mut regex = Field::nullable(&value, None::<JavaRegex>);
    let mut values = Field::nullable(&value, None::<Vec<String>>);
    let mut file = Field::nullable(&value, None::<PathBuf>);
    let mut enabled = Field::primitive(&value, true);
    let mut object = value.object()?;
    while let Some((name, child)) = object.next_field()? {
        match name.as_str() {
            "regex" => {
                let bound = child.regex()?;
                regex.set(child, bound);
            }
            "values" => {
                let bound = string_list(&child)?;
                values.set(child, bound);
            }
            "file" => {
                let bound = child.file()?;
                file.set(child, bound);
            }
            "enabled" => {
                let bound = child.boolean()?;
                enabled.set(child, Some(bound));
            }
            _ => child.unknown(false)?,
        }
    }
    Ok(RegexValuesFileFilter {
        regex: regex.optional(),
        values: values.optional(),
        file: file.optional(),
        enabled: enabled.required("enabled")?,
    })
}

fn test_package(value: Value<'_>) -> Result<TestPackageFilter> {
    let mut regex = Field::nullable(&value, None::<JavaRegex>);
    let mut values = Field::nullable(&value, None::<Vec<String>>);
    let mut file = Field::nullable(&value, None::<PathBuf>);
    let mut subpackages = Field::primitive(&value, false);
    let mut enabled = Field::primitive(&value, true);
    let mut object = value.object()?;
    while let Some((name, child)) = object.next_field()? {
        match name.as_str() {
            "regex" => {
                let bound = child.regex()?;
                regex.set(child, bound);
            }
            "values" => {
                let bound = string_list(&child)?;
                values.set(child, bound);
            }
            "file" => {
                let bound = child.file()?;
                file.set(child, bound);
            }
            "subpackages" => {
                let bound = child.boolean()?;
                subpackages.set(child, Some(bound));
            }
            "enabled" => {
                let bound = child.boolean()?;
                enabled.set(child, Some(bound));
            }
            _ => child.unknown(false)?,
        }
    }
    Ok(TestPackageFilter {
        regex: regex.optional(),
        values: values.optional(),
        file: file.optional(),
        subpackages: subpackages.required("subpackages")?,
        enabled: enabled.required("enabled")?,
    })
}

pub fn bind_test_filter_configuration(value: Value<'_>) -> Result<Option<TestFilterConfiguration>> {
    value.polymorphic(|id, v| {
        use TestFilterConfiguration as F;
        match id {
            "simple-class-name" => Some(regex_values_file(v).map(F::SimpleClassname)),
            "simple-test-name" => Some(regex_values_file(v).map(F::SimpleTestname)),
            "fully-qualified-class-name" => Some(regex_values_file(v).map(F::FullyQualifiedClassname)),
            "fully-qualified-test-name" => Some(regex_values_file(v).map(F::FullyQualifiedTestname)),
            "method" => Some(regex_values_file(v).map(F::TestMethod)),
            "annotation" => Some(regex_values_file(v).map(F::Annotation)),
            "package" => Some(test_package(v).map(F::TestPackage)),
            "annotationData" => Some((|| {
                let mut name_regex = Field::reference(&v, None::<JavaRegex>);
                let mut value_regex = Field::reference(&v, None::<JavaRegex>);
                let mut enabled = Field::primitive(&v, true);
                let mut object = v.object()?;
                while let Some((name, child)) = object.next_field()? {
                    match name.as_str() {
                        "nameRegex" => {
                            let bound = child.regex()?;
                            name_regex.set(child, bound);
                        }
                        "valueRegex" => {
                            let bound = child.regex()?;
                            value_regex.set(child, bound);
                        }
                        "enabled" => {
                            let bound = child.boolean()?;
                            enabled.set(child, Some(bound));
                        }
                        _ => child.unknown(false)?,
                    }
                }
                Ok(F::AnnotationData {
                    name_regex: name_regex.required("nameRegex")?,
                    value_regex: value_regex.required("valueRegex")?,
                    enabled: enabled.required("enabled")?,
                })
            })()),
            "fragmentation" => Some((|| {
                let mut index = Field::primitive(&v, 0);
                let mut count = Field::primitive(&v, 0);
                let mut enabled = Field::primitive(&v, true);
                let mut object = v.object()?;
                while let Some((name, child)) = object.next_field()? {
                    match name.as_str() {
                        "index" => {
                            let bound = child.int()?;
                            index.set(child, Some(bound));
                        }
                        "count" => {
                            let bound = child.int()?;
                            count.set(child, Some(bound));
                        }
                        "enabled" => {
                            let bound = child.boolean()?;
                            enabled.set(child, Some(bound));
                        }
                        _ => child.unknown(false)?,
                    }
                }
                Ok(F::Fragmentation {
                    index: index.required("index")?,
                    count: count.required("count")?,
                    enabled: enabled.required("enabled")?,
                })
            })()),
            "composition" => Some((|| {
                let mut filters = Field::reference(&v, None::<Vec<F>>);
                let mut op = Field::reference(&v, None::<CompositionOperation>);
                let mut enabled = Field::primitive(&v, true);
                let mut object = v.object()?;
                while let Some((name, child)) = object.next_field()? {
                    match name.as_str() {
                        "filters" => {
                            let bound = list_non_null(&child, bind_test_filter_configuration)?;
                            filters.set(child, bound);
                        }
                        "op" => {
                            let bound = child.enumeration(&["UNION", "INTERSECTION", "SUBTRACT"])?.map(|i| {
                                [
                                    CompositionOperation::Union,
                                    CompositionOperation::Intersection,
                                    CompositionOperation::Subtract,
                                ][i]
                            });
                            op.set(child, bound);
                        }
                        "enabled" => {
                            let bound = child.boolean()?;
                            enabled.set(child, Some(bound));
                        }
                        _ => child.unknown(false)?,
                    }
                }
                Ok(F::Composition {
                    filters: filters.required("filters")?,
                    op: op.required("op")?,
                    enabled: enabled.required("enabled")?,
                })
            })()),
            "allure" => Some((|| {
                let mut enabled = Field::primitive(&v, true);
                let mut object = v.object()?;
                while let Some((name, child)) = object.next_field()? {
                    match name.as_str() {
                        "enabled" => {
                            let bound = child.boolean()?;
                            enabled.set(child, Some(bound));
                        }
                        _ => child.unknown(false)?,
                    }
                }
                Ok(F::Allure {
                    enabled: enabled.required("enabled")?,
                })
            })()),
            _ => None,
        }
    })
}

pub fn bind_filtering_configuration(value: Value<'_>) -> Result<Option<FilteringConfiguration>> {
    value.nullable(|v| {
        let mut allowlist = Field::reference(&v, Some(Vec::new()));
        let mut blocklist = Field::reference(&v, Some(Vec::new()));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "allowlist" => {
                    let bound = list_non_null(&child, bind_test_filter_configuration)?;
                    allowlist.set(child, bound);
                }
                "blocklist" => {
                    let bound = list_non_null(&child, bind_test_filter_configuration)?;
                    blocklist.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(FilteringConfiguration {
            allowlist: allowlist.required("allowlist")?,
            blocklist: blocklist.required("blocklist")?,
        })
    })
}

// ---- Top level (Configuration.Builder) -------------------------------------------

/// `Configuration` through its Builder (Configuration.kt:148-212). Only
/// `name` and `outputDir` are required. The vendor binder is a parameter;
/// production passes `crate::bind_vendor::bind_vendor_configuration`
/// (WP 2.3a; WP 2.2's empty-only stub is deleted).
pub fn bind_configuration(
    value: Value<'_>,
    mut vendor: impl FnMut(Value<'_>) -> Result<Option<VendorConfiguration>>,
) -> Result<Option<Configuration>> {
    value.nullable(|v| {
        let mut name = Field::reference(&v, None::<String>);
        let mut output_dir = Field::reference(&v, None::<PathBuf>);
        let mut analytics = Field::reference(&v, Some(AnalyticsConfiguration::Disabled));
        let mut pooling = Field::reference(&v, Some(PoolingStrategyConfiguration::Omni));
        let mut sharding = Field::reference(&v, Some(ShardingStrategyConfiguration::Parallel));
        let mut sorting = Field::reference(&v, Some(SortingStrategyConfiguration::NoSorting));
        let mut batching = Field::reference(&v, Some(BatchingStrategyConfiguration::Isolate));
        let mut flakiness = Field::reference(&v, Some(FlakinessStrategyConfiguration::Ignore));
        let mut retry = Field::reference(&v, Some(RetryStrategyConfiguration::NoRetry));
        let mut filtering = Field::reference(
            &v,
            Some(FilteringConfiguration {
                allowlist: Vec::new(),
                blocklist: Vec::new(),
            }),
        );
        let mut ignore_failures = Field::primitive(&v, false);
        let mut coverage = Field::primitive(&v, false);
        let mut execution = Field::reference(
            &v,
            Some(ExecutionStrategyConfiguration {
                mode: ExecutionMode::AnySuccess,
                fast: true,
            }),
        );
        let mut quota = Field::primitive(&v, i32::MAX);
        let mut include = Field::reference(&v, Some(Vec::new()));
        let mut exclude = Field::reference(&v, Some(Vec::new()));
        let mut batch_timeout = Field::primitive(&v, 1_800_000_i64);
        let mut output_timeout = Field::primitive(&v, 300_000_i64);
        let mut debug = Field::primitive(&v, true);
        let mut screen = Field::reference(&v, Some(ScreenRecordingPolicy::OnFailure));
        let mut analytics_tracking = Field::primitive(&v, true);
        let mut bugsnag = Field::primitive(&v, true);
        let mut device_timeout = Field::primitive(&v, 180_000_i64);
        let mut output = Field::reference(
            &v,
            Some(OutputConfiguration {
                max_path: 0,
                max_filename: 255,
            }),
        );
        let mut vendor_field = Field::reference(&v, Some(VendorConfiguration::Empty));
        let mut ci = Field::reference(&v, Some(CiConfiguration::None));
        let mut object = v.object()?;
        while let Some((field, child)) = object.next_field()? {
            match field.as_str() {
                "name" => {
                    let bound = child.string()?;
                    name.set(child, bound);
                }
                "outputDir" => {
                    let bound = child.file()?;
                    output_dir.set(child, bound);
                }
                "analyticsConfiguration" => {
                    let bound = bind_analytics_configuration(child.clone())?;
                    analytics.set(child, bound);
                }
                "poolingStrategy" => {
                    let bound = bind_pooling_strategy(child.clone())?;
                    pooling.set(child, bound);
                }
                "shardingStrategy" => {
                    let bound = bind_sharding_strategy(child.clone())?;
                    sharding.set(child, bound);
                }
                "sortingStrategy" => {
                    let bound = bind_sorting_strategy(child.clone())?;
                    sorting.set(child, bound);
                }
                "batchingStrategy" => {
                    let bound = bind_batching_strategy(child.clone())?;
                    batching.set(child, bound);
                }
                "flakinessStrategy" => {
                    let bound = bind_flakiness_strategy(child.clone())?;
                    flakiness.set(child, bound);
                }
                "retryStrategy" => {
                    let bound = bind_retry_strategy(child.clone())?;
                    retry.set(child, bound);
                }
                "filteringConfiguration" => {
                    let bound = bind_filtering_configuration(child.clone())?;
                    filtering.set(child, bound);
                }
                "ignoreFailures" => {
                    let bound = child.boolean()?;
                    ignore_failures.set(child, Some(bound));
                }
                "isCodeCoverageEnabled" => {
                    let bound = child.boolean()?;
                    coverage.set(child, Some(bound));
                }
                "executionStrategy" => {
                    let bound = bind_execution_strategy(child.clone())?;
                    execution.set(child, bound);
                }
                "uncompletedTestRetryQuota" => {
                    let bound = child.int()?;
                    quota.set(child, Some(bound));
                }
                "includeSerialRegexes" => {
                    let bound = regex_list(&child)?;
                    include.set(child, bound);
                }
                "excludeSerialRegexes" => {
                    let bound = regex_list(&child)?;
                    exclude.set(child, bound);
                }
                "testBatchTimeoutMillis" => {
                    let bound = child.long()?;
                    batch_timeout.set(child, Some(bound));
                }
                "testOutputTimeoutMillis" => {
                    let bound = child.long()?;
                    output_timeout.set(child, Some(bound));
                }
                "debug" => {
                    let bound = child.boolean()?;
                    debug.set(child, Some(bound));
                }
                "screenRecordingPolicy" => {
                    let bound = bind_screen_recording_policy(child.clone())?;
                    screen.set(child, bound);
                }
                "analyticsTracking" => {
                    let bound = child.boolean()?;
                    analytics_tracking.set(child, Some(bound));
                }
                "bugsnagReporting" => {
                    let bound = child.boolean()?;
                    bugsnag.set(child, Some(bound));
                }
                "deviceInitializationTimeoutMillis" => {
                    let bound = child.long()?;
                    device_timeout.set(child, Some(bound));
                }
                "outputConfiguration" => {
                    let bound = bind_output_configuration(child.clone())?;
                    output.set(child, bound);
                }
                "vendorConfiguration" => {
                    let bound = vendor(child.clone())?;
                    vendor_field.set(child, bound);
                }
                "ciConfiguration" => {
                    let bound = bind_ci_configuration(child.clone())?;
                    ci.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(Configuration {
            name: name.required("name")?,
            output_dir: output_dir.required("outputDir")?,
            output_configuration: output.required("outputConfiguration")?,
            analytics_configuration: analytics.required("analyticsConfiguration")?,
            pooling_strategy: pooling.required("poolingStrategy")?,
            sharding_strategy: sharding.required("shardingStrategy")?,
            sorting_strategy: sorting.required("sortingStrategy")?,
            batching_strategy: batching.required("batchingStrategy")?,
            flakiness_strategy: flakiness.required("flakinessStrategy")?,
            retry_strategy: retry.required("retryStrategy")?,
            filtering_configuration: filtering.required("filteringConfiguration")?,
            ignore_failures: ignore_failures.required("ignoreFailures")?,
            execution_strategy: execution.required("executionStrategy")?,
            is_code_coverage_enabled: coverage.required("isCodeCoverageEnabled")?,
            uncompleted_test_retry_quota: quota.required("uncompletedTestRetryQuota")?,
            include_serial_regexes: include.required("includeSerialRegexes")?,
            exclude_serial_regexes: exclude.required("excludeSerialRegexes")?,
            test_batch_timeout_millis: batch_timeout.required("testBatchTimeoutMillis")?,
            test_output_timeout_millis: output_timeout.required("testOutputTimeoutMillis")?,
            debug: debug.required("debug")?,
            screen_recording_policy: screen.required("screenRecordingPolicy")?,
            vendor_configuration: vendor_field.required("vendorConfiguration")?,
            analytics_tracking: analytics_tracking.required("analyticsTracking")?,
            bugsnag_reporting: bugsnag.required("bugsnagReporting")?,
            device_initialization_timeout_millis: device_timeout.required("deviceInitializationTimeoutMillis")?,
            ci_configuration: ci.required("ciConfiguration")?,
        })
    })
}
