//! Corpus-only canonical projections of the production WP 2.2 binders.
//! Every binding call goes through `marathon_config::bind`; this file only
//! renders the bound values in the oracle's Canonical JSON shape.
use jcompat::jtime::parse_instant;
use marathon_config::{
    AnalyticsConfiguration, BatchingStrategyConfiguration, CiConfiguration, CompositionOperation, Configuration,
    Defaults, ExecutionMode, ExecutionStrategyConfiguration, FilteringConfiguration, FlakinessStrategyConfiguration,
    InfluxDb2RetentionPolicyConfiguration, OutputConfiguration, PoolingStrategyConfiguration, RegexValuesFileFilter,
    RetentionPolicyConfiguration, RetryStrategyConfiguration, ScreenRecordingPolicy, ShardingStrategyConfiguration,
    SortingStrategyConfiguration, TestFilterConfiguration, VendorConfiguration,
    bind::{
        CONFIGURATION_EXCEPTION, bind_analytics_configuration, bind_batching_strategy, bind_ci_configuration,
        bind_configuration, bind_defaults, bind_execution_strategy, bind_filtering_configuration,
        bind_flakiness_strategy, bind_influxdb2_retention_policy_configuration, bind_output_configuration,
        bind_pooling_strategy, bind_retention_policy_configuration, bind_retry_strategy, bind_sharding_strategy,
        bind_sorting_strategy, bind_test_filter_configuration,
    },
    bind_vendor::bind_vendor_configuration,
    binder::{Binder, Value},
    binding::{BindingContext, ConfigurationError, Profile},
};
use serde_json::{Value as Json, json};
use snakeyaml::node::Stream;
use std::path::PathBuf;

pub fn corpus_context() -> BindingContext {
    BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: PathBuf::from("/Marathonfile"),
        marathonfile_dir: PathBuf::from("/"),
        process_dir: PathBuf::from("/"),
        environment: vec![],
        now: parse_instant("2026-10-12T00:00:00Z").unwrap(),
        analytics_tracking: false,
        bugsnag_reporting: false,
    }
}

pub enum Outcome {
    Ok(Json),
    NullDocument,
    Err(Box<ConfigurationError>),
}

fn s(v: &str) -> Json {
    json!(v.to_owned())
}
fn os(v: &Option<String>) -> Json {
    v.as_ref().map_or(Json::Null, |text| s(text))
}
fn f(v: f64) -> Json {
    if v.is_finite() {
        serde_json::Number::from_f64(v).map_or(Json::Null, Json::Number)
    } else {
        json!(jcompat::jnum::double_to_string(v))
    }
}
fn path(v: &std::path::Path) -> Json {
    json!(v.to_string_lossy().into_owned())
}
fn defaults(d: &Defaults) -> Json {
    json!({"duration": d.duration.to_string(), "observedDurationMinTests": d.observed_duration_min_tests, "successRate": f(d.success_rate)})
}
fn retention(r: &RetentionPolicyConfiguration) -> Json {
    json!({"duration": s(&r.duration), "isDefault": r.is_default, "name": s(&r.name), "replicationFactor": r.replication_factor, "shardDuration": s(&r.shard_duration)})
}
fn retention2(r: &InfluxDb2RetentionPolicyConfiguration) -> Json {
    json!({"everySeconds": r.every_seconds, "shardGroupDurationSeconds": r.shard_group_duration_seconds})
}
fn analytics(a: &AnalyticsConfiguration) -> Json {
    match a {
        AnalyticsConfiguration::Disabled => json!({"type": "disabled"}),
        AnalyticsConfiguration::InfluxDb(c) => json!({
            "dbName": s(&c.db_name), "defaults": defaults(&c.defaults), "password": s(&c.password),
            "readOnly": c.read_only, "retentionPolicyConfiguration": retention(&c.retention_policy_configuration),
            "type": "influxdb", "url": s(&c.url), "user": s(&c.user)}),
        AnalyticsConfiguration::InfluxDb2(c) => json!({
            "bucket": s(&c.bucket), "defaults": defaults(&c.defaults), "organization": s(&c.organization),
            "readOnly": c.read_only, "retentionPolicyConfiguration": retention2(&c.retention_policy_configuration),
            "token": s(&c.token), "type": "influxdb2", "url": s(&c.url)}),
        AnalyticsConfiguration::Graphite(c) => json!({
            "defaults": defaults(&c.defaults), "host": s(&c.host), "port": c.port, "prefix": os(&c.prefix),
            "readOnly": c.read_only, "type": "graphite"}),
    }
}
fn pooling(p: &PoolingStrategyConfiguration) -> Json {
    use PoolingStrategyConfiguration as P;
    match p {
        P::Omni => json!({"type": "omni"}),
        P::Abi => json!({"type": "abi"}),
        P::Manufacturer => json!({"type": "manufacturer"}),
        P::Model => json!({"type": "device-model"}),
        P::OperatingSystemVersion => json!({"type": "os-version"}),
        P::Combo(list) => json!({"list": list.iter().map(pooling).collect::<Vec<_>>(), "type": "combo"}),
    }
}
fn sharding(v: &ShardingStrategyConfiguration) -> Json {
    match v {
        ShardingStrategyConfiguration::Count { count } => json!({"count": count, "type": "count"}),
        ShardingStrategyConfiguration::Parallel => json!({"type": "parallel"}),
    }
}
fn sorting(v: &SortingStrategyConfiguration) -> Json {
    use SortingStrategyConfiguration as S;
    match v {
        S::ExecutionTime { percentile, time_limit } => {
            json!({"percentile": f(*percentile), "timeLimit": time_limit.to_string(), "type": "execution-time"})
        }
        S::NoSorting => json!({"type": "no-sorting"}),
        S::RandomOrder => json!({"type": "random-order"}),
        S::SuccessRate { time_limit, ascending } => {
            json!({"ascending": ascending, "timeLimit": time_limit.to_string(), "type": "success-rate"})
        }
    }
}
fn batching(v: &BatchingStrategyConfiguration) -> Json {
    use BatchingStrategyConfiguration as B;
    match v {
        B::FixedSize {
            size,
            duration_millis,
            percentile,
            time_limit,
            last_mile_length,
        } => json!({
            "durationMillis": duration_millis, "lastMileLength": last_mile_length,
            "percentile": percentile.map(f), "size": size,
            "timeLimit": time_limit.map(|t| t.to_string()), "type": "fixed-size"}),
        B::ClassName => json!({"type": "class-name"}),
        B::Isolate => json!({"type": "isolate"}),
    }
}
fn flakiness(v: &FlakinessStrategyConfiguration) -> Json {
    match v {
        FlakinessStrategyConfiguration::Ignore => json!({"type": "ignore"}),
        FlakinessStrategyConfiguration::Probability {
            min_success_rate,
            max_count,
            time_limit,
        } => json!({
            "maxCount": max_count, "minSuccessRate": f(*min_success_rate),
            "timeLimit": time_limit.to_string(), "type": "probability"}),
    }
}
fn retry(v: &RetryStrategyConfiguration) -> Json {
    match v {
        RetryStrategyConfiguration::FixedQuota {
            total_allowed_retry_quota,
            retry_per_test_quota,
        } => json!({
            "retryPerTestQuota": retry_per_test_quota, "totalAllowedRetryQuota": total_allowed_retry_quota, "type": "fixed-quota"}),
        RetryStrategyConfiguration::NoRetry => json!({"type": "no-retry"}),
    }
}
fn execution(v: &ExecutionStrategyConfiguration) -> Json {
    json!({"fast": v.fast, "mode": match v.mode { ExecutionMode::AnySuccess => "ANY_SUCCESS", ExecutionMode::AllSuccess => "ALL_SUCCESS" }})
}
fn output(v: &OutputConfiguration) -> Json {
    json!({"maxFilename": v.max_filename, "maxPath": v.max_path})
}
fn rvf(v: &RegexValuesFileFilter, ty: &str) -> Json {
    json!({"enabled": v.enabled, "file": v.file.as_ref().map(|v| path(v)), "regex": v.regex.as_ref().map(|r| r.pattern().to_owned()),
        "type": ty, "values": v.values.as_ref().map(|xs| xs.iter().map(|text| s(text)).collect::<Vec<_>>())})
}
fn filter(v: &TestFilterConfiguration) -> Json {
    use TestFilterConfiguration as F;
    match v {
        F::SimpleClassname(x) => rvf(x, "simple-class-name"),
        F::SimpleTestname(x) => rvf(x, "simple-test-name"),
        F::FullyQualifiedClassname(x) => rvf(x, "fully-qualified-class-name"),
        F::FullyQualifiedTestname(x) => rvf(x, "fully-qualified-test-name"),
        F::TestMethod(x) => rvf(x, "method"),
        F::Annotation(x) => rvf(x, "annotation"),
        F::TestPackage(x) => json!({"enabled": x.enabled, "file": x.file.as_ref().map(|v| path(v)),
            "regex": x.regex.as_ref().map(|r| r.pattern().to_owned()), "subpackages": x.subpackages,
            "type": "package", "values": x.values.as_ref().map(|xs| xs.iter().map(|text| s(text)).collect::<Vec<_>>())}),
        F::AnnotationData {
            name_regex,
            value_regex,
            enabled,
        } => json!({"enabled": enabled,
            "nameRegex": name_regex.pattern(), "type": "annotationData", "valueRegex": value_regex.pattern()}),
        F::Fragmentation { index, count, enabled } => {
            json!({"count": count, "enabled": enabled, "index": index, "type": "fragmentation"})
        }
        F::Composition { filters, op, enabled } => json!({"enabled": enabled,
            "filters": filters.iter().map(filter).collect::<Vec<_>>(),
            "op": match op { CompositionOperation::Union => "UNION", CompositionOperation::Intersection => "INTERSECTION", CompositionOperation::Subtract => "SUBTRACT" },
            "type": "composition"}),
        F::Allure { enabled } => json!({"enabled": enabled, "type": "allure"}),
    }
}
fn filtering(v: &FilteringConfiguration) -> Json {
    json!({"allowlist": v.allowlist.iter().map(filter).collect::<Vec<_>>(), "blocklist": v.blocklist.iter().map(filter).collect::<Vec<_>>()})
}
fn ci(v: CiConfiguration) -> Json {
    json!({"type": match v { CiConfiguration::Auto => "auto", CiConfiguration::None => "none", CiConfiguration::Teamcity => "teamcity" }})
}
fn vendor(v: &VendorConfiguration) -> Json {
    json!({"type": match v {
        VendorConfiguration::Android(_) => "Android", VendorConfiguration::Ios(_) => "iOS",
        VendorConfiguration::Macos(_) => "macOS", VendorConfiguration::Stub => "stub", VendorConfiguration::Empty => "empty"}})
}
fn configuration(c: &Configuration) -> Json {
    json!({
        "analyticsConfiguration": analytics(&c.analytics_configuration), "analyticsTracking": c.analytics_tracking,
        "batchingStrategy": batching(&c.batching_strategy), "bugsnagReporting": c.bugsnag_reporting,
        "ciConfiguration": ci(c.ci_configuration), "debug": c.debug,
        "deviceInitializationTimeoutMillis": c.device_initialization_timeout_millis,
        "excludeSerialRegexes": c.exclude_serial_regexes.iter().map(|r| r.pattern().to_owned()).collect::<Vec<_>>(),
        "executionStrategy": execution(&c.execution_strategy), "filteringConfiguration": filtering(&c.filtering_configuration),
        "flakinessStrategy": flakiness(&c.flakiness_strategy), "ignoreFailures": c.ignore_failures,
        "includeSerialRegexes": c.include_serial_regexes.iter().map(|r| r.pattern().to_owned()).collect::<Vec<_>>(),
        "isCodeCoverageEnabled": c.is_code_coverage_enabled, "name": s(&c.name),
        "outputConfiguration": output(&c.output_configuration), "outputDir": path(&c.output_dir),
        "poolingStrategy": pooling(&c.pooling_strategy), "retryStrategy": retry(&c.retry_strategy),
        "screenRecordingPolicy": match c.screen_recording_policy { ScreenRecordingPolicy::OnFailure => "ON_FAILURE", ScreenRecordingPolicy::OnAny => "ON_ANY" },
        "shardingStrategy": sharding(&c.sharding_strategy), "sortingStrategy": sorting(&c.sorting_strategy),
        "testBatchTimeoutMillis": c.test_batch_timeout_millis, "testOutputTimeoutMillis": c.test_output_timeout_millis,
        "uncompletedTestRetryQuota": c.uncompleted_test_retry_quota, "vendorConfiguration": vendor(&c.vendor_configuration)})
}

fn bind_one<T>(
    stream: &Stream,
    bind: impl FnOnce(Value<'_>) -> marathon_config::binder::Result<Option<T>>,
    render: impl FnOnce(T) -> Json,
) -> Outcome {
    let context = corpus_context();
    let binder = Binder::new(&context, stream);
    match binder.first(stream, bind) {
        // first() already maps a null document to None; a schema returning
        // None on a non-null document cannot happen for these targets, and
        // the JVM renders both as its null-document outcome at top level.
        Ok(Some(Some(v))) => Outcome::Ok(render(v)),
        Ok(_) => Outcome::NullDocument,
        Err(e) => Outcome::Err(e),
    }
}

pub fn bind_target(stream: &Stream, target: &str) -> Outcome {
    match target {
        "Configuration" => bind_one(
            stream,
            |v| bind_configuration(v, bind_vendor_configuration),
            |v| configuration(&v),
        ),
        "AnalyticsConfiguration" => bind_one(stream, bind_analytics_configuration, |v| analytics(&v)),
        "RetentionPolicy" => bind_one(stream, bind_retention_policy_configuration, |v| retention(&v)),
        "RetentionPolicy2" => bind_one(stream, bind_influxdb2_retention_policy_configuration, |v| {
            retention2(&v)
        }),
        "Defaults" => bind_one(stream, bind_defaults, |v| defaults(&v)),
        "OutputConfiguration" => bind_one(stream, bind_output_configuration, |v| output(&v)),
        "ExecutionStrategy" | "ExecutionStrategyConfiguration" => {
            bind_one(stream, bind_execution_strategy, |v| execution(&v))
        }
        "CIConfiguration" => bind_one(stream, bind_ci_configuration, ci),
        "FilteringConfiguration" => bind_one(stream, bind_filtering_configuration, |v| filtering(&v)),
        "TestFilterConfiguration" => bind_one(stream, bind_test_filter_configuration, |v| filter(&v)),
        "Pooling" | "PoolingStrategyConfiguration" => bind_one(stream, bind_pooling_strategy, |v| pooling(&v)),
        "Sharding" | "ShardingStrategyConfiguration" => bind_one(stream, bind_sharding_strategy, |v| sharding(&v)),
        "Sorting" | "SortingStrategyConfiguration" => bind_one(stream, bind_sorting_strategy, |v| sorting(&v)),
        "Batching" | "BatchingStrategyConfiguration" => bind_one(stream, bind_batching_strategy, |v| batching(&v)),
        "Flakiness" | "FlakinessStrategyConfiguration" => bind_one(stream, bind_flakiness_strategy, |v| flakiness(&v)),
        "Retry" | "RetryStrategyConfiguration" => bind_one(stream, bind_retry_strategy, |v| retry(&v)),
        other => panic!("unknown schema corpus target {other}"),
    }
}

/// Compare one outcome with a JVM `{ok}|{error:{class,message}}|{nullDocument}`
/// expectation. Jackson wording is ours (PLAN §3.10), so only the class is
/// compared, except custom `ConfigurationException`s, whose message is exact.
pub fn outcome_matches(got: &Outcome, expected: &Json) -> Result<(), String> {
    match (
        got,
        expected.get("ok"),
        expected.get("error"),
        expected.get("nullDocument"),
    ) {
        (Outcome::Ok(v), Some(ok), _, _) if v == ok => Ok(()),
        (Outcome::NullDocument, _, _, Some(n)) if n == &Json::Bool(true) => Ok(()),
        (Outcome::Err(e), _, Some(error), _) => {
            let class = error["class"].as_str().unwrap_or("?");
            if e.kind.legacy_class() != class {
                return Err(format!("error class {class} != {}", e.kind.legacy_class().to_owned()));
            }
            if class == CONFIGURATION_EXCEPTION.legacy_class() {
                let message = e.message.as_ref().map(|m| m.to_owned());
                if message.as_deref() != error["message"].as_str() {
                    return Err(format!("custom message {:?} != {:?}", error["message"], message));
                }
            }
            Ok(())
        }
        (Outcome::Ok(v), Some(ok), _, _) => Err(format!("ok value differs: expected {ok} got {v}")),
        (Outcome::Ok(v), _, _, _) => Err(format!("expected error, got ok {v}")),
        (Outcome::NullDocument, _, _, _) => Err("expected a value, got a null document".into()),
        (Outcome::Err(e), _, _, _) => Err(format!(
            "expected ok, got error {} {:?}",
            e.kind.legacy_class().to_owned(),
            e.message
        )),
    }
}
