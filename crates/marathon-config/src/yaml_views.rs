//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt
//! and the configuration and vendor property models it serializes.

// The view macro infers its generic parameters from declaration-order field names.
#![allow(non_camel_case_types)]

use crate::{render::Names, yaml, *};
use jcompat::{
    JavaRegex,
    jtime::{Duration, Instant},
};
use serde::{Serialize, Serializer};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

/// Borrowed configuration/section views keep output names and order separate from input binding.
pub struct View<'a, T>(pub &'a T);

// Every expansion derives a statically typed struct in declaration order, without a value tree.
macro_rules! view {
    (@collect [$($generic:ident,)*] [$($fields:tt)*] [$($values:tt)*];) => {
        {
            #[derive(Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Fields<$($generic,)*> { $($fields)* }
            Fields { $($values)* }
        }
    };
    (@collect [$($generic:ident,)*] [$($fields:tt)*] [$($values:tt)*];
        #[serde(skip_serializing_if = "yaml::omit_none")] $field:ident: $value:expr, $($rest:tt)*) => {
        view!(@collect [$($generic,)* $field,]
            [$($fields)* #[serde(skip_serializing_if = "yaml::omit_none")] $field: Option<$field>,]
            [$($values)* $field: $value,]; $($rest)*)
    };
    (@collect [$($generic:ident,)*] [$($fields:tt)*] [$($values:tt)*]; $field:ident: $value:expr, $($rest:tt)*) => {
        view!(@collect [$($generic,)* $field,] [$($fields)* $field: $field,]
            [$($values)* $field: $value,]; $($rest)*)
    };
    (@collect [$($generic:ident,)*] [$($fields:tt)*] [$($values:tt)*]; ,) => {
        view!(@collect [$($generic,)*] [$($fields)*] [$($values)*];)
    };
    (@collect $($invalid:tt)*) => { compile_error!("invalid YAML view fields") };
    ($($fields:tt)*) => { view!(@collect [] [] []; $($fields)* ,) };
}

fn w_text(s: &str) -> &str {
    s
}
fn w_path(p: &Path) -> Cow<'_, str> {
    p.to_string_lossy()
}
fn w_regex(r: &JavaRegex) -> &str {
    r.pattern()
}
fn w_int(n: impl Into<i64>) -> i64 {
    n.into()
}
fn w_double(d: f64) -> yaml::Float {
    yaml::Float(d)
}
fn w_duration(d: &Duration) -> String {
    d.to_string()
}
fn w_instant(i: &Instant) -> String {
    i.to_string()
}
fn w_enum(e: impl Names) -> &'static str {
    e.json()
}
fn w_opt<'a, T, U>(v: Option<&'a T>, f: impl FnOnce(&'a T) -> U) -> Option<U> {
    v.map(f)
}
fn w_list<'a, T, U>(v: &'a [T], f: impl FnMut(&'a T) -> U) -> Vec<U> {
    v.iter().map(f).collect()
}
fn w_paths(v: Option<&Vec<PathBuf>>) -> Option<Vec<Cow<'_, str>>> {
    v.map(|v| v.iter().map(|p| w_path(p)).collect())
}
fn w_map(v: &[(String, String)]) -> yaml::Entries<&str, &str> {
    yaml::Entries(v.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect())
}

impl Serialize for View<'_, Configuration> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        view! {
            name: w_text(&self.0.name),
            output_dir: w_path(&self.0.output_dir),
            output_configuration: view! {
                max_path: w_int(self.0.output_configuration.max_path),
                max_filename: w_int(self.0.output_configuration.max_filename),
            },
            analytics_configuration: View(&self.0.analytics_configuration),
            pooling_strategy: View(&self.0.pooling_strategy),
            sharding_strategy: View(&self.0.sharding_strategy),
            sorting_strategy: View(&self.0.sorting_strategy),
            batching_strategy: View(&self.0.batching_strategy),
            flakiness_strategy: View(&self.0.flakiness_strategy),
            retry_strategy: View(&self.0.retry_strategy),
            filtering_configuration: View(&self.0.filtering_configuration),
            ignore_failures: self.0.ignore_failures,
            execution_strategy: view! {
                mode: w_enum(self.0.execution_strategy.mode),
                fast: self.0.execution_strategy.fast,
            },
            is_code_coverage_enabled: self.0.is_code_coverage_enabled,
            uncompleted_test_retry_quota: w_int(self.0.uncompleted_test_retry_quota),
            include_serial_regexes: w_list(&self.0.include_serial_regexes, w_regex),
            exclude_serial_regexes: w_list(&self.0.exclude_serial_regexes, w_regex),
            test_batch_timeout_millis: w_int(self.0.test_batch_timeout_millis),
            test_output_timeout_millis: w_int(self.0.test_output_timeout_millis),
            debug: self.0.debug,
            screen_recording_policy: w_enum(self.0.screen_recording_policy),
            vendor_configuration: View(&self.0.vendor_configuration),
            analytics_tracking: self.0.analytics_tracking,
            bugsnag_reporting: self.0.bugsnag_reporting,
            device_initialization_timeout_millis: w_int(self.0.device_initialization_timeout_millis),
            ci_configuration: view! {
                r#type: match self.0.ci_configuration {
                    CiConfiguration::Auto => "auto",
                    CiConfiguration::None => "none",
                    CiConfiguration::Teamcity => "teamcity",
                },
            },
        }
        .serialize(serializer)
    }
}
fn w_defaults(d: &Defaults) -> impl Serialize + '_ {
    view! {
        success_rate: w_double(d.success_rate),
        duration: w_duration(&d.duration),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        observed_duration_min_tests: w_opt(d.observed_duration_min_tests.as_ref(), |n| w_int(*n)),
    }
}
impl Serialize for View<'_, AnalyticsConfiguration> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            AnalyticsConfiguration::Disabled => view! { r#type: "disabled" }.serialize(serializer),
            AnalyticsConfiguration::InfluxDb(c) => {
                let rp = &c.retention_policy_configuration;
                view! {
                    r#type: "influxdb",
                    url: w_text(&c.url),
                    user: w_text(&c.user),
                    password: w_text(&c.password),
                    db_name: w_text(&c.db_name),
                    retention_policy_configuration: view! {
                        name: w_text(&rp.name),
                        duration: w_text(&rp.duration),
                        shard_duration: w_text(&rp.shard_duration),
                        replication_factor: w_int(rp.replication_factor),
                        is_default: rp.is_default,
                    },
                    defaults: w_defaults(&c.defaults),
                    read_only: c.read_only,
                }
                .serialize(serializer)
            }
            AnalyticsConfiguration::InfluxDb2(c) => view! {
                r#type: "influxdb2",
                url: w_text(&c.url),
                token: w_text(&c.token),
                organization: w_text(&c.organization),
                bucket: w_text(&c.bucket),
                retention_policy_configuration: view! {
                    every_seconds: w_int(c.retention_policy_configuration.every_seconds),
                    shard_group_duration_seconds: w_int(c.retention_policy_configuration.shard_group_duration_seconds),
                },
                defaults: w_defaults(&c.defaults),
                read_only: c.read_only,
            }
            .serialize(serializer),
            AnalyticsConfiguration::Graphite(c) => view! {
                r#type: "graphite",
                host: w_text(&c.host),
                #[serde(skip_serializing_if = "yaml::omit_none")]
                port: w_opt(c.port.as_ref(), |p| w_int(*p)),
                #[serde(skip_serializing_if = "yaml::omit_none")]
                prefix: w_opt(c.prefix.as_ref(), |s| w_text(s)),
                defaults: w_defaults(&c.defaults),
                read_only: c.read_only,
            }
            .serialize(serializer),
        }
    }
}
impl Serialize for View<'_, PoolingStrategyConfiguration> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            PoolingStrategyConfiguration::Omni => view! { r#type: "omni" }.serialize(serializer),
            PoolingStrategyConfiguration::Abi => view! { r#type: "abi" }.serialize(serializer),
            PoolingStrategyConfiguration::Manufacturer => view! { r#type: "manufacturer" }.serialize(serializer),
            PoolingStrategyConfiguration::Model => view! { r#type: "device-model" }.serialize(serializer),
            PoolingStrategyConfiguration::OperatingSystemVersion => {
                view! { r#type: "os-version" }.serialize(serializer)
            }
            PoolingStrategyConfiguration::Combo(items) => view! {
                r#type: "combo",
                list: w_list(items, View),
            }
            .serialize(serializer),
        }
    }
}
impl Serialize for View<'_, ShardingStrategyConfiguration> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            ShardingStrategyConfiguration::Count { count } => view! {
                r#type: "count",
                count: w_int(*count),
            }
            .serialize(serializer),
            ShardingStrategyConfiguration::Parallel => view! { r#type: "parallel" }.serialize(serializer),
        }
    }
}
impl Serialize for View<'_, SortingStrategyConfiguration> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            SortingStrategyConfiguration::ExecutionTime { percentile, time_limit } => view! {
                r#type: "execution-time",
                percentile: w_double(*percentile),
                time_limit: w_instant(time_limit),
            }
            .serialize(serializer),
            SortingStrategyConfiguration::NoSorting => view! { r#type: "no-sorting" }.serialize(serializer),
            SortingStrategyConfiguration::RandomOrder => view! { r#type: "random-order" }.serialize(serializer),
            SortingStrategyConfiguration::SuccessRate { time_limit, ascending } => view! {
                r#type: "success-rate",
                time_limit: w_instant(time_limit),
                ascending: *ascending,
            }
            .serialize(serializer),
        }
    }
}
impl Serialize for View<'_, BatchingStrategyConfiguration> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            BatchingStrategyConfiguration::FixedSize {
                size,
                duration_millis,
                percentile,
                time_limit,
                last_mile_length,
            } => view! {
                r#type: "fixed-size",
                size: w_int(*size),
                #[serde(skip_serializing_if = "yaml::omit_none")]
                duration_millis: w_opt(duration_millis.as_ref(), |n| w_int(*n)),
                #[serde(skip_serializing_if = "yaml::omit_none")]
                percentile: w_opt(percentile.as_ref(), |p| w_double(*p)),
                #[serde(skip_serializing_if = "yaml::omit_none")]
                time_limit: w_opt(time_limit.as_ref(), w_instant),
                last_mile_length: w_int(*last_mile_length),
            }
            .serialize(serializer),
            BatchingStrategyConfiguration::ClassName => view! { r#type: "class-name" }.serialize(serializer),
            BatchingStrategyConfiguration::Isolate => view! { r#type: "isolate" }.serialize(serializer),
        }
    }
}
impl Serialize for View<'_, FlakinessStrategyConfiguration> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            FlakinessStrategyConfiguration::Ignore => view! { r#type: "ignore" }.serialize(serializer),
            FlakinessStrategyConfiguration::Probability {
                min_success_rate,
                max_count,
                time_limit,
            } => view! {
                r#type: "probability",
                min_success_rate: w_double(*min_success_rate),
                max_count: w_int(*max_count),
                time_limit: w_instant(time_limit),
            }
            .serialize(serializer),
        }
    }
}
impl Serialize for View<'_, RetryStrategyConfiguration> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            RetryStrategyConfiguration::FixedQuota {
                total_allowed_retry_quota,
                retry_per_test_quota,
            } => view! {
                r#type: "fixed-quota",
                total_allowed_retry_quota: w_int(*total_allowed_retry_quota),
                retry_per_test_quota: w_int(*retry_per_test_quota),
            }
            .serialize(serializer),
            RetryStrategyConfiguration::NoRetry => view! { r#type: "no-retry" }.serialize(serializer),
        }
    }
}
impl Serialize for View<'_, FilteringConfiguration> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        view! {
            allowlist: w_list(&self.0.allowlist, View),
            blocklist: w_list(&self.0.blocklist, View),
        }
        .serialize(serializer)
    }
}
fn w_values_filter<'a>(id: &'a str, f: &'a RegexValuesFileFilter) -> impl Serialize + 'a {
    view! {
        r#type: id,
        #[serde(skip_serializing_if = "yaml::omit_none")]
        regex: w_opt(f.regex.as_ref(), w_regex),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        values: w_opt(f.values.as_ref(), |v| w_list(v, |s| w_text(s))),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        file: w_opt(f.file.as_ref(), |p| w_path(p)),
        enabled: f.enabled,
    }
}
impl Serialize for View<'_, TestFilterConfiguration> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            TestFilterConfiguration::SimpleClassname(f) => {
                w_values_filter("simple-class-name", f).serialize(serializer)
            }
            TestFilterConfiguration::SimpleTestname(f) => w_values_filter("simple-test-name", f).serialize(serializer),
            TestFilterConfiguration::FullyQualifiedClassname(f) => {
                w_values_filter("fully-qualified-class-name", f).serialize(serializer)
            }
            TestFilterConfiguration::FullyQualifiedTestname(f) => {
                w_values_filter("fully-qualified-test-name", f).serialize(serializer)
            }
            TestFilterConfiguration::TestMethod(f) => w_values_filter("method", f).serialize(serializer),
            TestFilterConfiguration::Annotation(f) => w_values_filter("annotation", f).serialize(serializer),
            TestFilterConfiguration::TestPackage(f) => view! {
                r#type: "package",
                #[serde(skip_serializing_if = "yaml::omit_none")]
                regex: w_opt(f.regex.as_ref(), w_regex),
                #[serde(skip_serializing_if = "yaml::omit_none")]
                values: w_opt(f.values.as_ref(), |v| w_list(v, |s| w_text(s))),
                #[serde(skip_serializing_if = "yaml::omit_none")]
                file: w_opt(f.file.as_ref(), |p| w_path(p)),
                subpackages: f.subpackages,
                enabled: f.enabled,
            }
            .serialize(serializer),
            TestFilterConfiguration::AnnotationData {
                name_regex,
                value_regex,
                enabled,
            } => view! {
                r#type: "annotationData",
                name_regex: w_regex(name_regex),
                value_regex: w_regex(value_regex),
                enabled: *enabled,
            }
            .serialize(serializer),
            TestFilterConfiguration::Fragmentation { index, count, enabled } => view! {
                r#type: "fragmentation",
                index: w_int(*index),
                count: w_int(*count),
                enabled: *enabled,
            }
            .serialize(serializer),
            TestFilterConfiguration::Composition { filters, op, enabled } => view! {
                r#type: "composition",
                filters: w_list(filters, View),
                op: w_enum(*op),
                enabled: *enabled,
            }
            .serialize(serializer),
            TestFilterConfiguration::Allure { enabled } => view! {
                r#type: "allure",
                enabled: *enabled,
            }
            .serialize(serializer),
        }
    }
}

impl Serialize for View<'_, VendorConfiguration> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            VendorConfiguration::Android(c) => android(c).serialize(serializer),
            // The canonical output discriminator is iOS, regardless of its accepted input aliases.
            VendorConfiguration::Ios(c) => ios(c).serialize(serializer),
            VendorConfiguration::Macos(c) => macos(c).serialize(serializer),
            VendorConfiguration::Stub => view! { r#type: "stub" }.serialize(serializer),
            VendorConfiguration::Empty => view! { r#type: "empty" }.serialize(serializer),
        }
    }
}
fn android(c: &AndroidConfiguration) -> impl Serialize + '_ {
    let video = &c.screen_record_configuration.video_configuration;
    let screenshot = &c.screen_record_configuration.screenshot_configuration;
    let t = &c.timeout_configuration;
    view! {
        r#type: "Android",
        #[serde(skip_serializing_if = "yaml::omit_none")]
        android_sdk: w_opt(c.android_sdk.as_ref(), |p| w_path(p)),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        application_apk: w_opt(c.application_output.as_ref(), |p| w_path(p)),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        test_application_apk: w_opt(c.test_application_output.as_ref(), |p| w_path(p)),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        split_apks: w_paths(c.split_apks.as_ref()),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        extra_applications_apk: w_paths(c.extra_applications_output.as_ref()),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        outputs: w_opt(c.outputs.as_ref(), |v| {
            w_list(v, |o| view! {
                #[serde(skip_serializing_if = "yaml::omit_none")]
                application: w_opt(o.application.as_ref(), |p| w_path(p)),
                test_application: w_path(&o.test_application),
                #[serde(skip_serializing_if = "yaml::omit_none")]
                extra_applications: w_paths(o.extra_applications.as_ref()),
                #[serde(skip_serializing_if = "yaml::omit_none")]
                split_apks: w_paths(o.split_apks.as_ref()),
            })
        }),
        auto_grant_permission: c.auto_grant_permission,
        instrumentation_args: w_map(&c.instrumentation_args),
        application_pm_clear: c.application_pm_clear,
        test_application_pm_clear: c.test_application_pm_clear,
        adb_init_timeout_millis: w_int(c.adb_init_timeout_millis),
        install_options: w_text(&c.install_options),
        serial_strategy: w_enum(c.serial_strategy),
        screen_record_configuration: view! {
            #[serde(skip_serializing_if = "yaml::omit_none")]
            preferable_recorder_type: w_opt(c.screen_record_configuration.preferable_recorder_type.as_ref(), |t| {
                w_enum(*t)
            }),
            video_configuration: view! {
                enabled: video.enabled,
                width: w_int(video.width),
                height: w_int(video.height),
                bitrate_mbps: w_int(video.bitrate_mbps),
                time_limit: w_int(video.time_limit),
                time_limit_units: w_enum(video.time_limit_units),
            },
            screenshot_configuration: view! {
                enabled: screenshot.enabled,
                width: w_int(screenshot.width),
                height: w_int(screenshot.height),
                delay_ms: w_int(screenshot.delay_ms),
            },
        },
        wait_for_devices_timeout_millis: w_int(c.wait_for_devices_timeout_millis),
        allure_configuration: view! {
            enabled: c.allure_configuration.enabled,
            relative_results_directory: w_text(&c.allure_configuration.relative_results_directory),
            path_root: w_enum(c.allure_configuration.path_root),
        },
        timeout_configuration: view! {
            shell: w_duration(&t.shell),
            list_files: w_duration(&t.list_files),
            push_file: w_duration(&t.push_file),
            push_folder: w_duration(&t.push_folder),
            pull_file: w_duration(&t.pull_file),
            uninstall: w_duration(&t.uninstall),
            install: w_duration(&t.install),
            screenrecorder: w_duration(&t.screenrecorder),
            screencapturer: w_duration(&t.screencapturer),
            socket_idle_timeout: w_duration(&t.socket_idle_timeout),
            port_forward: w_duration(&t.port_forward),
            boot: w_duration(&t.boot),
        },
        file_sync_configuration: view! {
            pull: w_list(&c.file_sync_configuration.pull, |e| view! {
                relative_path: w_text(&e.relative_path),
                path_root: w_enum(e.path_root),
                aggregation_mode: w_enum(e.aggregation_mode),
            }),
            push: w_list(&c.file_sync_configuration.push, |e| view! {
                path: w_text(&e.path),
                path_root: w_enum(e.path_root),
            }),
        },
        threading_configuration: view! {
            boot_waiting_threads: w_int(c.threading_configuration.boot_waiting_threads),
            adb_io_threads: w_int(c.threading_configuration.adb_io_threads),
            install_threads: w_int(c.threading_configuration.install_threads),
        },
        test_parser_configuration: View(&c.test_parser_configuration),
        test_access_configuration: view! {
            adb: c.test_access_configuration.adb,
            grpc: c.test_access_configuration.grpc,
            console: c.test_access_configuration.console,
            console_token: w_text(&c.test_access_configuration.console_token),
        },
        adb_servers: w_list(&c.adb_servers, |e| view! {
            host: w_text(&e.host),
            port: w_int(e.port),
        }),
        disable_window_animation: c.disable_window_animation,
        profiling_configuration: view! {
            enabled: c.profiling_configuration.enabled,
            #[serde(skip_serializing_if = "yaml::omit_none")]
            pbtxt: w_opt(c.profiling_configuration.pbtxt.as_ref(), |p| w_path(p)),
            url_template: w_text(&c.profiling_configuration.url_template),
        },
        mock_location: c.mock_location,
    }
}
/// Only the constructor properties; the resolved `app`/`testApp`/`xctest`
/// are `@JsonIgnore` and are found again by the re-parse's `validate()`.
fn w_bundle(b: &AppleTestBundleConfiguration) -> impl Serialize + '_ {
    view! {
        #[serde(skip_serializing_if = "yaml::omit_none")]
        application: w_opt(b.application.as_ref(), |p| w_path(p)),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        test_application: w_opt(b.test_application.as_ref(), |p| w_path(p)),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        extra_applications: w_paths(b.extra_applications.as_ref()),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        derived_data_dir: w_opt(b.derived_data_dir.as_ref(), |p| w_path(p)),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        test_type: w_opt(b.test_type.as_ref(), |t| w_enum(*t)),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        xctestrun: w_opt(b.xctestrun.as_ref(), |p| w_path(p)),
    }
}
impl Serialize for View<'_, DeviceProvider> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            DeviceProvider::Static { path } => view! {
                r#type: "static",
                #[serde(skip_serializing_if = "yaml::omit_none")]
                path: w_opt(path.as_ref(), |p| w_path(p)),
            }
            .serialize(serializer),
            DeviceProvider::Dynamic { host, port } => view! {
                r#type: "dynamic",
                host: w_text(host),
                port: w_int(*port),
            }
            .serialize(serializer),
        }
    }
}
fn w_ssh(s: &SshConfiguration) -> impl Serialize + '_ {
    view! {
        #[serde(skip_serializing_if = "yaml::omit_none")]
        authentication: w_opt(s.authentication.as_ref(), View),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        known_hosts_path: w_opt(s.known_hosts_path.as_ref(), |p| w_path(p)),
        keep_alive_interval: w_duration(&s.keep_alive_interval),
        keep_alive_max_count: w_int(s.keep_alive_max_count),
        debug: s.debug,
        share_worker_connection: s.share_worker_connection,
    }
}
impl Serialize for View<'_, XcresultConfiguration> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        view! {
            #[serde(skip_serializing_if = "yaml::omit_none")]
            pull: w_opt(self.0.pull.as_ref(), |b| *b),
            pulling_policy: w_enum(self.0.pulling_policy),
            remote_clean: self.0.remote_clean,
            attachments: view! {
                system_attachment_lifetime: w_enum(self.0.attachments.system_attachment_lifetime),
                user_attachment_lifetime: w_enum(self.0.attachments.user_attachment_lifetime),
            },
            #[serde(skip_serializing_if = "yaml::omit_none")]
            preferred_screen_capture_format: w_opt(self.0.preferred_screen_capture_format.as_ref(), |f| w_enum(*f)),
            queue: view! {
                capacity: w_int(self.0.queue.capacity),
                concurrency: w_int(self.0.queue.concurrency),
                #[serde(skip_serializing_if = "yaml::omit_none")]
                worker_capacity: w_opt(self.0.queue.worker_capacity.as_ref(), |n| w_int(*n)),
            },
        }
        .serialize(serializer)
    }
}
fn w_xctestrun_env(e: &XctestrunEnvConfiguration) -> impl Serialize + '_ {
    view! {
        app: w_map(&e.app_envs),
        test: w_map(&e.test_envs),
    }
}
fn w_apple_timeouts(t: &AppleTimeoutConfiguration) -> impl Serialize + '_ {
    // Derived values (`xcresulttool`, `bootWait`, the shell-defaulted ones)
    // are plain fields by now and are written, so the re-parse keeps them.
    view! {
        shell: w_duration(&t.shell),
        shell_idle: w_duration(&t.shell_idle),
        reachability: w_duration(&t.reachability),
        screenshot: w_duration(&t.screenshot),
        video: w_duration(&t.video),
        erase: w_duration(&t.erase),
        shutdown: w_duration(&t.shutdown),
        delete: w_duration(&t.delete),
        create: w_duration(&t.create),
        boot: w_duration(&t.boot),
        install: w_duration(&t.install),
        uninstall: w_duration(&t.uninstall),
        import_media: w_duration(&t.import_media),
        test_destination: w_duration(&t.test_destination),
        xcresulttool: w_duration(&t.xcresulttool),
        boot_wait: w_duration(&t.boot_wait),
    }
}
fn w_apple_threading(t: &AppleThreadingConfiguration) -> impl Serialize + '_ {
    view! {
        device_provider_threads: w_int(t.device_provider_threads),
        device_threads: w_int(t.device_threads),
        provisioning_concurrency: w_int(t.provisioning_concurrency),
    }
}
impl Serialize for View<'_, AppleTestParserConfiguration> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            // Kotlin writes the default pattern, which the regex crate cannot
            // compile (D6, WP 2.3a decision 7); leaving the key out rebinds the
            // same default.
            AppleTestParserConfiguration::Nm { test_class_regexes }
                if matches!(test_class_regexes.as_slice(), [TestClassRegex::Default]) =>
            {
                view! { r#type: "nm" }.serialize(serializer)
            }
            AppleTestParserConfiguration::Nm { test_class_regexes } => view! {
                r#type: "nm",
                test_class_regexes: w_list(test_class_regexes, |r| r.pattern()),
            }
            .serialize(serializer),
            AppleTestParserConfiguration::Xctest => view! { r#type: "xctest" }.serialize(serializer),
        }
    }
}
fn ios(c: &IosConfiguration) -> impl Serialize + '_ {
    let screen = &c.screen_record_configuration;
    let video = &screen.video_configuration;
    let screenshot = &screen.screenshot_configuration;
    view! {
        r#type: "iOS",
        #[serde(skip_serializing_if = "yaml::omit_none")]
        bundle: w_opt(c.bundle.as_ref(), w_bundle),
        device_provider: View(&c.device_provider),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        devices: w_opt(c.devices_file.as_ref(), |p| w_path(p)),
        ssh: w_ssh(&c.ssh),
        worker_connection: view! {
            attempts: w_int(c.worker_connection.attempts),
            backoff: w_duration(&c.worker_connection.backoff),
            retry_interval: w_duration(&c.worker_connection.retry_interval),
            late_join: c.worker_connection.late_join,
            late_join_cutoff: w_duration(&c.worker_connection.late_join_cutoff),
        },
        xcresult: View(&c.xcresult),
        screen_record_configuration: view! {
            #[serde(skip_serializing_if = "yaml::omit_none")]
            preferable_recorder_type: w_opt(screen.preferable_recorder_type.as_ref(), |t| w_enum(*t)),
            video_configuration: view! {
                enabled: video.enabled,
                codec: w_enum(video.codec),
                display: w_enum(video.display),
                mask: w_enum(video.mask),
                transcoding: view! {
                    enabled: video.transcoding.enabled,
                    ffmpeg_path: w_text(&video.transcoding.binary),
                    size: w_enum(video.transcoding.size),
                },
            },
            screenshot_configuration: view! {
                enabled: screenshot.enabled,
                r#type: w_enum(screenshot.screenshot_type),
                display: w_enum(screenshot.display),
                mask: w_enum(screenshot.mask),
                delay: w_duration(&screenshot.delay),
                width: w_int(screenshot.width),
                height: w_int(screenshot.height),
            },
        },
        xctestrun_env: w_xctestrun_env(&c.xctestrun_env),
        lifecycle: view! {
            shutdown_unused: c.lifecycle_configuration.shutdown_unused,
            on_prepare: w_list(&c.lifecycle_configuration.on_prepare, |a| w_enum(*a)),
            on_dispose: w_list(&c.lifecycle_configuration.on_dispose, |a| w_enum(*a)),
        },
        permissions: view! {
            grant: w_list(&c.permissions.grant, |p| w_enum(*p)),
            lifecycle: w_enum(c.permissions.lifecycle),
        },
        #[serde(skip_serializing_if = "yaml::omit_none")]
        import_media: w_paths(c.media_files.as_ref()),
        timeout_configuration: w_apple_timeouts(&c.timeout_configuration),
        threading_configuration: w_apple_threading(&c.threading_configuration),
        hide_runner_output: c.hide_runner_output,
        compact_output: c.compact_output,
        rsync: view! {
            remote_path: w_text(&c.rsync.remote_path),
        },
        xcodebuild_test_args: w_map(&c.xcodebuild_test_args),
        data_container_clear: c.data_container_clear,
        test_parser_configuration: View(&c.test_parser_configuration),
        device_log: c.device_log,
        signing: view! {
            #[serde(skip_serializing_if = "yaml::omit_none")]
            test_runner_provisioning_profile: w_opt(c.signing.test_runner_provisioning_profile.as_ref(), |s| w_text(s)),
        },
        skip_copying_derived_data: c.skip_copying_derived_data,
        max_batches_without_progress: w_int(c.max_batches_without_progress),
    }
}
fn macos(c: &MacosConfiguration) -> impl Serialize + '_ {
    view! {
        r#type: "macOS",
        #[serde(skip_serializing_if = "yaml::omit_none")]
        bundle: w_opt(c.bundle.as_ref(), w_bundle),
        device_provider: View(&c.device_provider),
        #[serde(skip_serializing_if = "yaml::omit_none")]
        devices: w_opt(c.devices_file.as_ref(), |p| w_path(p)),
        ssh: w_ssh(&c.ssh),
        xcresult: View(&c.xcresult),
        xctestrun_env: w_xctestrun_env(&c.xctestrun_env),
        timeout_configuration: w_apple_timeouts(&c.timeout_configuration),
        threading_configuration: w_apple_threading(&c.threading_configuration),
        hide_runner_output: c.hide_runner_output,
        compact_output: c.compact_output,
        rsync: view! {
            remote_path: w_text(&c.rsync.remote_path),
        },
        xcodebuild_test_args: w_map(&c.xcodebuild_test_args),
        test_parser_configuration: View(&c.test_parser_configuration),
        permissions: view! {
            grant: yaml::Entries(
                c.permissions
                    .grant
                    .iter()
                    .map(|(k, v)| (k.as_str(), w_list(v, |p| w_enum(*p))))
                    .collect(),
            ),
        },
    }
}

impl Serialize for View<'_, AndroidTestParserConfiguration> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            AndroidTestParserConfiguration::Local => view! { r#type: "local" }.serialize(serializer),
            AndroidTestParserConfiguration::Remote { instrumentation_args } => view! {
                r#type: "remote",
                instrumentation_args: w_map(instrumentation_args),
            }
            .serialize(serializer),
        }
    }
}
impl Serialize for View<'_, SshAuthentication> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            SshAuthentication::Password { username, password } => view! {
                r#type: "password",
                username: w_text(username),
                password: w_text(password),
            }
            .serialize(serializer),
            SshAuthentication::PublicKey { username, key } => view! {
                r#type: "publicKey",
                username: w_text(username),
                key: w_path(key),
            }
            .serialize(serializer),
        }
    }
}
