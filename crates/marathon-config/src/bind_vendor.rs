//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/vendor/VendorConfiguration.kt,
//! config/vendor/android/*.kt, config/vendor/apple/*.kt, config/vendor/apple/ios/*.kt and
//! config/vendor/apple/macos/PermissionsConfiguration.kt (binding only).
//!
//! Hand-written `bind_*` schema functions over the WP 2.1 binder, in the
//! shape WP 2.2's `bind_configuration` takes as its vendor-binder parameter:
//! `fn(Value) -> Result<Option<VendorConfiguration>>`. Defaults live here,
//! never in the types. All `@JsonCreator` enums uppercase
//! locale-invariantly in Kotlin (`String.uppercase()` is `Locale.ROOT`;
//! the other two creators pass `Locale.ENGLISH` explicitly), so the
//! former P1 simplification is moot. Approved simplification: P7 (sets
//! as first-seen-ordered, duplicate-collapsed vectors; Kotlin sets are
//! `HashSet`s whose iteration order is unspecified).

use crate::{
    VendorConfiguration,
    bind::{list_non_null, singleton},
    binder::{Field, MISMATCHED, Result, Value},
    vendor::*,
};
use jcompat::{file_manager::JavaPath, jtime::Duration};

fn file_list(value: &Value<'_>) -> Result<Option<Vec<JavaPath>>> {
    list_non_null(value, |v| v.file())
}

/// P7: a Kotlin `Set` binds as an ordered vector with duplicates collapsed in
/// first-seen order. Jackson builds a `HashSet`, whose order is unspecified, so
/// first-seen order is an approved simplification, not parity.
fn set_non_null<T: PartialEq>(
    value: &Value<'_>,
    bind: impl FnMut(Value<'_>) -> Result<Option<T>>,
) -> Result<Option<Vec<T>>> {
    Ok(list_non_null(value, bind)?.map(|items| {
        let mut out: Vec<T> = Vec::new();
        for item in items {
            if !out.contains(&item) {
                out.push(item);
            }
        }
        out
    }))
}

fn string_map(value: &Value<'_>) -> Result<Option<Vec<(String, String)>>> {
    value.map(|elem| {
        elem.string()?
            .ok_or_else(|| elem.error(MISMATCHED, "D9: null element in collection"))
    })
}

fn bind_enum<T: Copy>(value: &Value<'_>, names: &[&str], variants: &[T]) -> Result<Option<T>> {
    Ok(value.enumeration(names)?.map(|i| variants[i]))
}

/// `@JsonCreator` enums: locale-invariant uppercase (`Locale.ROOT` /
/// `Locale.ENGLISH` in Kotlin; `str::to_uppercase` here), then exact
/// constant name.
fn creator<T: Copy>(names: &[&str], variants: &[T]) -> impl FnOnce(&str) -> Option<T> + use<T> {
    let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
    let variants: Vec<T> = variants.to_vec();
    move |text: &str| {
        let upper = text.to_uppercase();
        names.iter().position(|n| n == &upper).map(|i| variants[i])
    }
}

fn secs(seconds: i64) -> Duration {
    Duration::of_seconds(seconds)
}
fn millis(millis: i64) -> Duration {
    jcompat::jtime::duration_of(0, millis * 1_000_000).unwrap_or(Duration::of_seconds(0))
}

// ---- Default instances (Kotlin primary-constructor defaults) ------------------

fn default_adb_endpoint() -> AdbEndpoint {
    AdbEndpoint {
        host: "127.0.0.1".into(),
        port: 5037,
    }
}
fn default_allure() -> AllureConfiguration {
    AllureConfiguration {
        enabled: false,
        relative_results_directory: "/files/allure-results".into(),
        path_root: PathRoot::AppData,
    }
}
fn default_android_video() -> AndroidVideoConfiguration {
    AndroidVideoConfiguration {
        enabled: true,
        width: 720,
        height: 1280,
        bitrate_mbps: 1,
        time_limit: 180,
        time_limit_units: TimeUnit::Seconds,
    }
}
fn default_android_screenshot() -> AndroidScreenshotConfiguration {
    AndroidScreenshotConfiguration {
        enabled: true,
        width: 720,
        height: 1280,
        delay_ms: 500,
    }
}
fn default_android_screen_record() -> AndroidScreenRecordConfiguration {
    AndroidScreenRecordConfiguration {
        preferable_recorder_type: None,
        video_configuration: default_android_video(),
        screenshot_configuration: default_android_screenshot(),
    }
}
fn default_profiling() -> ProfilingConfiguration {
    ProfilingConfiguration {
        enabled: false,
        pbtxt: None,
        url_template: "https://cloud.marathonlabs.io/".into(),
    }
}
fn default_test_access() -> TestAccessConfiguration {
    TestAccessConfiguration {
        adb: false,
        grpc: false,
        console: false,
        console_token: "".into(),
    }
}
fn default_android_threading() -> AndroidThreadingConfiguration {
    AndroidThreadingConfiguration {
        boot_waiting_threads: 4,
        adb_io_threads: 4,
        install_threads: 8,
    }
}
fn default_ssh() -> SshConfiguration {
    SshConfiguration {
        authentication: None,
        known_hosts_path: None,
        keep_alive_interval: secs(15),
        keep_alive_max_count: 4,
        debug: false,
        share_worker_connection: false,
    }
}
fn default_worker_connection() -> WorkerConnectionConfiguration {
    WorkerConnectionConfiguration {
        attempts: 3,
        backoff: secs(10),
        retry_interval: secs(60),
        late_join: true,
        late_join_cutoff: secs(600),
    }
}
fn default_xcresult() -> XcresultConfiguration {
    XcresultConfiguration {
        pull: None,
        pulling_policy: PullingPolicy::Always,
        remote_clean: true,
        attachments: AttachmentsConfiguration {
            system_attachment_lifetime: Lifetime::DeleteOnSuccess,
            user_attachment_lifetime: Lifetime::KeepAlways,
        },
        preferred_screen_capture_format: None,
        queue: XcresultQueueConfiguration::default_instance(),
    }
}
fn default_ios_screenshot() -> IosScreenshotConfiguration {
    IosScreenshotConfiguration {
        enabled: true,
        screenshot_type: ScreenshotType::Jpeg,
        display: Display::Internal,
        mask: Mask::Black,
        delay: millis(500),
        width: 720,
        height: 1280,
    }
}
fn default_ios_video() -> IosVideoConfiguration {
    IosVideoConfiguration {
        enabled: true,
        codec: Codec::H264,
        display: Display::Internal,
        mask: Mask::Black,
        transcoding: TranscodingConfiguration {
            enabled: false,
            binary: "/opt/homebrew/bin/ffmpeg".into(),
            size: TranscodingSize::Hd720,
        },
    }
}
fn default_ios_screen_record() -> IosScreenRecordConfiguration {
    IosScreenRecordConfiguration {
        preferable_recorder_type: None,
        video_configuration: default_ios_video(),
        screenshot_configuration: default_ios_screenshot(),
    }
}
fn default_xctestrun_env() -> XctestrunEnvConfiguration {
    XctestrunEnvConfiguration {
        app_envs: Vec::new(),
        test_envs: Vec::new(),
    }
}
fn default_lifecycle() -> LifecycleConfiguration {
    LifecycleConfiguration {
        shutdown_unused: true,
        on_prepare: Vec::new(),
        on_dispose: Vec::new(),
    }
}
fn default_ios_permissions() -> IosPermissionsConfiguration {
    IosPermissionsConfiguration {
        grant: Vec::new(),
        lifecycle: GrantLifecycle::BeforeTestRun,
    }
}
fn default_rsync() -> RsyncConfiguration {
    RsyncConfiguration {
        remote_path: "/usr/bin/rsync".into(),
    }
}
fn default_signing() -> SigningConfiguration {
    SigningConfiguration {
        test_runner_provisioning_profile: None,
    }
}
fn default_nm() -> AppleTestParserConfiguration {
    AppleTestParserConfiguration::Nm {
        test_class_regexes: vec![TestClassRegex::Default],
    }
}

// ---- Android leaves ------------------------------------------------------------

pub fn bind_adb_endpoint(value: Value<'_>) -> Result<Option<AdbEndpoint>> {
    value.nullable(|v| {
        let mut host = Field::reference(&v, Some(String::from("127.0.0.1")));
        let mut port = Field::primitive(&v, 5037);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "host" => {
                    let bound = child.string()?;
                    host.set(child, bound);
                }
                "port" => {
                    let bound = child.int()?;
                    port.set(child, Some(bound));
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(AdbEndpoint {
            host: host.required("host")?,
            port: port.required("port")?,
        })
    })
}

const PATH_ROOT_NAMES: [&str; 3] = ["EXTERNAL_STORAGE", "APP_DATA", "LOCAL_TMP"];
const PATH_ROOTS: [PathRoot; 3] = [PathRoot::ExternalStorage, PathRoot::AppData, PathRoot::LocalTmp];
const AGGREGATION_NAMES: [&str; 4] = ["DEVICE", "POOL", "DEVICE_AND_POOL", "TEST_RUN"];
const AGGREGATIONS: [AggregationMode; 4] = [
    AggregationMode::Device,
    AggregationMode::Pool,
    AggregationMode::DeviceAndPool,
    AggregationMode::TestRun,
];
const TIME_UNIT_NAMES: [&str; 7] = [
    "NANOSECONDS",
    "MICROSECONDS",
    "MILLISECONDS",
    "SECONDS",
    "MINUTES",
    "HOURS",
    "DAYS",
];
const TIME_UNITS: [TimeUnit; 7] = [
    TimeUnit::Nanoseconds,
    TimeUnit::Microseconds,
    TimeUnit::Milliseconds,
    TimeUnit::Seconds,
    TimeUnit::Minutes,
    TimeUnit::Hours,
    TimeUnit::Days,
];

pub fn bind_allure_configuration(value: Value<'_>) -> Result<Option<AllureConfiguration>> {
    value.nullable(|v| {
        let mut enabled = Field::primitive(&v, false);
        let mut relative = Field::reference(&v, Some(String::from("/files/allure-results")));
        let mut path_root = Field::reference(&v, Some(PathRoot::AppData));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "enabled" => {
                    let bound = child.boolean()?;
                    enabled.set(child, Some(bound));
                }
                "relativeResultsDirectory" => {
                    let bound = child.string()?;
                    relative.set(child, bound);
                }
                "pathRoot" => {
                    let bound = bind_enum(&child, &PATH_ROOT_NAMES, &PATH_ROOTS)?;
                    path_root.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(AllureConfiguration {
            enabled: enabled.required("enabled")?,
            relative_results_directory: relative.required("relativeResultsDirectory")?,
            path_root: path_root.required("pathRoot")?,
        })
    })
}

pub fn bind_android_test_bundle_configuration(value: Value<'_>) -> Result<Option<AndroidTestBundleConfiguration>> {
    value.nullable(|v| {
        let mut application = Field::nullable(&v, None::<JavaPath>);
        let mut test_application = Field::reference(&v, None::<JavaPath>);
        let mut extra = Field::nullable(&v, None::<Vec<JavaPath>>);
        let mut split = Field::nullable(&v, None::<Vec<JavaPath>>);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "application" => {
                    let bound = child.file()?;
                    application.set(child, bound);
                }
                "testApplication" => {
                    let bound = child.file()?;
                    test_application.set(child, bound);
                }
                "extraApplications" => {
                    let bound = file_list(&child)?;
                    extra.set(child, bound);
                }
                "splitApks" => {
                    let bound = file_list(&child)?;
                    split.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(AndroidTestBundleConfiguration {
            application: application.optional(),
            test_application: test_application.required("testApplication")?,
            extra_applications: extra.optional(),
            split_apks: split.optional(),
        })
    })
}

pub fn bind_file_sync_entry(value: Value<'_>) -> Result<Option<FileSyncEntry>> {
    value.nullable(|v| {
        let mut relative_path = Field::reference(&v, None::<String>);
        let mut path_root = Field::reference(&v, Some(PathRoot::ExternalStorage));
        let mut aggregation = Field::reference(&v, Some(AggregationMode::Device));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "relativePath" => {
                    let bound = child.string()?;
                    relative_path.set(child, bound);
                }
                "pathRoot" => {
                    let bound = bind_enum(&child, &PATH_ROOT_NAMES, &PATH_ROOTS)?;
                    path_root.set(child, bound);
                }
                "aggregationMode" => {
                    let bound = bind_enum(&child, &AGGREGATION_NAMES, &AGGREGATIONS)?;
                    aggregation.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(FileSyncEntry {
            relative_path: relative_path.required("relativePath")?,
            path_root: path_root.required("pathRoot")?,
            aggregation_mode: aggregation.required("aggregationMode")?,
        })
    })
}

pub fn bind_file_push_entry(value: Value<'_>) -> Result<Option<FilePushEntry>> {
    value.nullable(|v| {
        let mut path = Field::reference(&v, None::<String>);
        let mut path_root = Field::reference(&v, Some(PathRoot::LocalTmp));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "path" => {
                    let bound = child.string()?;
                    path.set(child, bound);
                }
                "pathRoot" => {
                    let bound = bind_enum(&child, &PATH_ROOT_NAMES, &PATH_ROOTS)?;
                    path_root.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(FilePushEntry {
            path: path.required("path")?,
            path_root: path_root.required("pathRoot")?,
        })
    })
}

pub fn bind_file_sync_configuration(value: Value<'_>) -> Result<Option<FileSyncConfiguration>> {
    value.nullable(|v| {
        let mut pull = Field::reference(&v, Some(Vec::new()));
        let mut push = Field::reference(&v, Some(Vec::new()));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "pull" => {
                    let bound = set_non_null(&child, bind_file_sync_entry)?;
                    pull.set(child, bound);
                }
                "push" => {
                    let bound = set_non_null(&child, bind_file_push_entry)?;
                    push.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(FileSyncConfiguration {
            pull: pull.required("pull")?,
            push: push.required("push")?,
        })
    })
}

pub fn bind_profiling_configuration(value: Value<'_>) -> Result<Option<ProfilingConfiguration>> {
    value.nullable(|v| {
        let mut enabled = Field::primitive(&v, false);
        let mut pbtxt = Field::nullable(&v, None::<JavaPath>);
        let mut url_template = Field::reference(&v, Some(String::from("https://cloud.marathonlabs.io/")));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "enabled" => {
                    let bound = child.boolean()?;
                    enabled.set(child, Some(bound));
                }
                "pbtxt" => {
                    let bound = child.file()?;
                    pbtxt.set(child, bound);
                }
                "urlTemplate" => {
                    let bound = child.string()?;
                    url_template.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(ProfilingConfiguration {
            enabled: enabled.required("enabled")?,
            pbtxt: pbtxt.optional(),
            url_template: url_template.required("urlTemplate")?,
        })
    })
}

pub fn bind_android_video_configuration(value: Value<'_>) -> Result<Option<AndroidVideoConfiguration>> {
    value.nullable(|v| {
        let mut enabled = Field::primitive(&v, true);
        let mut width = Field::primitive(&v, 720);
        let mut height = Field::primitive(&v, 1280);
        let mut bitrate = Field::primitive(&v, 1);
        let mut time_limit = Field::primitive(&v, 180_i64);
        let mut units = Field::reference(&v, Some(TimeUnit::Seconds));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "enabled" => {
                    let bound = child.boolean()?;
                    enabled.set(child, Some(bound));
                }
                "width" => {
                    let bound = child.int()?;
                    width.set(child, Some(bound));
                }
                "height" => {
                    let bound = child.int()?;
                    height.set(child, Some(bound));
                }
                "bitrateMbps" => {
                    let bound = child.int()?;
                    bitrate.set(child, Some(bound));
                }
                "timeLimit" => {
                    let bound = child.long()?;
                    time_limit.set(child, Some(bound));
                }
                "timeLimitUnits" => {
                    let bound = bind_enum(&child, &TIME_UNIT_NAMES, &TIME_UNITS)?;
                    units.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(AndroidVideoConfiguration {
            enabled: enabled.required("enabled")?,
            width: width.required("width")?,
            height: height.required("height")?,
            bitrate_mbps: bitrate.required("bitrateMbps")?,
            time_limit: time_limit.required("timeLimit")?,
            time_limit_units: units.required("timeLimitUnits")?,
        })
    })
}

pub fn bind_android_screenshot_configuration(value: Value<'_>) -> Result<Option<AndroidScreenshotConfiguration>> {
    value.nullable(|v| {
        let mut enabled = Field::primitive(&v, true);
        let mut width = Field::primitive(&v, 720);
        let mut height = Field::primitive(&v, 1280);
        let mut delay_ms = Field::primitive(&v, 500);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "enabled" => {
                    let bound = child.boolean()?;
                    enabled.set(child, Some(bound));
                }
                "width" => {
                    let bound = child.int()?;
                    width.set(child, Some(bound));
                }
                "height" => {
                    let bound = child.int()?;
                    height.set(child, Some(bound));
                }
                "delayMs" => {
                    let bound = child.int()?;
                    delay_ms.set(child, Some(bound));
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(AndroidScreenshotConfiguration {
            enabled: enabled.required("enabled")?,
            width: width.required("width")?,
            height: height.required("height")?,
            delay_ms: delay_ms.required("delayMs")?,
        })
    })
}

const RECORDER_NAMES: [&str; 2] = ["VIDEO", "SCREENSHOT"];
const ANDROID_RECORDERS: [AndroidRecorderType; 2] = [AndroidRecorderType::Video, AndroidRecorderType::Screenshot];
const IOS_RECORDERS: [IosRecorderType; 2] = [IosRecorderType::Video, IosRecorderType::Screenshot];

pub fn bind_android_screen_record_configuration(value: Value<'_>) -> Result<Option<AndroidScreenRecordConfiguration>> {
    value.nullable(|v| {
        let mut recorder = Field::nullable(&v, None::<AndroidRecorderType>);
        let mut video = Field::reference(&v, Some(default_android_video()));
        let mut screenshot = Field::reference(&v, Some(default_android_screenshot()));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "preferableRecorderType" => {
                    let bound = child.enum_creator_named(
                        "com.malinskiy.marathon.config.vendor.android.RecorderType",
                        creator(&RECORDER_NAMES, &ANDROID_RECORDERS),
                    )?;
                    recorder.set(child, bound);
                }
                "videoConfiguration" => {
                    let bound = bind_android_video_configuration(child.clone())?;
                    video.set(child, bound);
                }
                "screenshotConfiguration" => {
                    let bound = bind_android_screenshot_configuration(child.clone())?;
                    screenshot.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(AndroidScreenRecordConfiguration {
            preferable_recorder_type: recorder.optional(),
            video_configuration: video.required("videoConfiguration")?,
            screenshot_configuration: screenshot.required("screenshotConfiguration")?,
        })
    })
}

pub fn bind_serial_strategy(value: Value<'_>) -> Result<Option<SerialStrategy>> {
    value.enum_creator_named(
        "com.malinskiy.marathon.config.vendor.android.SerialStrategy",
        creator(
            &["AUTOMATIC", "MARATHON_PROPERTY", "BOOT_PROPERTY", "HOSTNAME", "DDMS"],
            &[
                SerialStrategy::Automatic,
                SerialStrategy::MarathonProperty,
                SerialStrategy::BootProperty,
                SerialStrategy::Hostname,
                SerialStrategy::Ddms,
            ],
        ),
    )
}

pub fn bind_test_access_configuration(value: Value<'_>) -> Result<Option<TestAccessConfiguration>> {
    value.nullable(|v| {
        let mut adb = Field::primitive(&v, false);
        let mut grpc = Field::primitive(&v, false);
        let mut console = Field::primitive(&v, false);
        let mut token = Field::reference(&v, Some(String::from("")));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "adb" => {
                    let bound = child.boolean()?;
                    adb.set(child, Some(bound));
                }
                "grpc" => {
                    let bound = child.boolean()?;
                    grpc.set(child, Some(bound));
                }
                "console" => {
                    let bound = child.boolean()?;
                    console.set(child, Some(bound));
                }
                "consoleToken" => {
                    let bound = child.string()?;
                    token.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(TestAccessConfiguration {
            adb: adb.required("adb")?,
            grpc: grpc.required("grpc")?,
            console: console.required("console")?,
            console_token: token.required("consoleToken")?,
        })
    })
}

pub fn bind_android_test_parser_configuration(value: Value<'_>) -> Result<Option<AndroidTestParserConfiguration>> {
    value.polymorphic(|id, v| match id {
        "local" => Some(singleton(v).map(|()| AndroidTestParserConfiguration::Local)),
        "remote" => Some(
            v.nullable(|v| {
                let mut args = Field::reference(&v, Some(Vec::new()));
                let mut object = v.object()?;
                while let Some((name, child)) = object.next_field()? {
                    match name.as_str() {
                        "instrumentationArgs" => {
                            let bound = string_map(&child)?;
                            args.set(child, bound);
                        }
                        _ => child.unknown(false)?,
                    }
                }
                Ok(AndroidTestParserConfiguration::Remote {
                    instrumentation_args: args.required("instrumentationArgs")?,
                })
            })
            .and_then(|bound| bound.ok_or_else(|| v.error(MISMATCHED, "non-null parameter is null"))),
        ),
        _ => None,
    })
}

pub fn bind_android_threading_configuration(value: Value<'_>) -> Result<Option<AndroidThreadingConfiguration>> {
    value.nullable(|v| {
        let mut boot_waiting = Field::primitive(&v, 4);
        let mut adb_io = Field::primitive(&v, 4);
        let mut install = Field::primitive(&v, 8);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "bootWaitingThreads" => {
                    let bound = child.int()?;
                    boot_waiting.set(child, Some(bound));
                }
                "adbIoThreads" => {
                    let bound = child.int()?;
                    adb_io.set(child, Some(bound));
                }
                "installThreads" => {
                    let bound = child.int()?;
                    install.set(child, Some(bound));
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(AndroidThreadingConfiguration {
            boot_waiting_threads: boot_waiting.required("bootWaitingThreads")?,
            adb_io_threads: adb_io.required("adbIoThreads")?,
            install_threads: install.required("installThreads")?,
        })
    })
}

/// Derived defaults compute from the bound values regardless of document
/// order, so both timeout binders collect `Option`s first (with a present
/// flag per field, so an explicit `null` on a non-null `Duration` is rejected
/// while an absent field keeps its derived default) and construct once.
fn reject_explicit_nulls(present: &[(bool, Option<Duration>)], sources: &[Option<Value<'_>>]) -> Result<()> {
    for ((seen, value), source) in present.iter().zip(sources) {
        // A seen field always recorded its source.
        if *seen
            && value.is_none()
            && let Some(source) = source
        {
            return Err(source.error(crate::binder::MISSING_KOTLIN, "missing or null non-null parameter"));
        }
    }
    Ok(())
}

pub fn bind_android_timeout_configuration(value: Value<'_>) -> Result<Option<AndroidTimeoutConfiguration>> {
    value.nullable(|v| {
        let mut p = AndroidTimeoutParameters::empty();
        let mut sources: [Option<Value<'_>>; 12] = [const { None }; 12];
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            macro_rules! dur {
                ($index:literal, $field:ident) => {{
                    p.$field = child.duration()?;
                    sources[$index] = Some(child.clone());
                }};
            }
            match name.as_str() {
                "shell" => dur!(0, shell),
                "listFiles" => dur!(1, list_files),
                "pushFile" => dur!(2, push_file),
                "pushFolder" => dur!(3, push_folder),
                "pullFile" => dur!(4, pull_file),
                "uninstall" => dur!(5, uninstall),
                "install" => dur!(6, install),
                "screenrecorder" => dur!(7, screenrecorder),
                "screencapturer" => dur!(8, screencapturer),
                "socketIdleTimeout" => dur!(9, socket_idle_timeout),
                "portForward" => dur!(10, port_forward),
                "boot" => dur!(11, boot),
                _ => child.unknown(false)?,
            }
        }
        let present = [
            (sources[0].is_some(), p.shell),
            (sources[1].is_some(), p.list_files),
            (sources[2].is_some(), p.push_file),
            (sources[3].is_some(), p.push_folder),
            (sources[4].is_some(), p.pull_file),
            (sources[5].is_some(), p.uninstall),
            (sources[6].is_some(), p.install),
            (sources[7].is_some(), p.screenrecorder),
            (sources[8].is_some(), p.screencapturer),
            (sources[9].is_some(), p.socket_idle_timeout),
            (sources[10].is_some(), p.port_forward),
            (sources[11].is_some(), p.boot),
        ];
        reject_explicit_nulls(&present, &sources)?;
        Ok(android_timeout_configuration(p))
    })
}

pub fn bind_android_configuration(value: Value<'_>) -> Result<Option<AndroidConfiguration>> {
    value.nullable(|v| {
        let mut android_sdk = Field::nullable(&v, None::<JavaPath>);
        let mut application_output = Field::nullable(&v, None::<JavaPath>);
        let mut test_application_output = Field::nullable(&v, None::<JavaPath>);
        let mut split_apks = Field::nullable(&v, None::<Vec<JavaPath>>);
        let mut extra_output = Field::nullable(&v, None::<Vec<JavaPath>>);
        let mut outputs = Field::nullable(&v, None::<Vec<AndroidTestBundleConfiguration>>);
        let mut auto_grant = Field::primitive(&v, false);
        let mut instrumentation_args = Field::reference(&v, Some(Vec::new()));
        let mut app_pm_clear = Field::primitive(&v, false);
        let mut test_pm_clear = Field::primitive(&v, false);
        let mut adb_init = Field::primitive(&v, 30_000);
        let mut install_options = Field::reference(&v, Some(String::from("")));
        let mut serial_strategy = Field::reference(&v, Some(SerialStrategy::Automatic));
        let mut screen_record = Field::reference(&v, Some(default_android_screen_record()));
        let mut wait_for_devices = Field::primitive(&v, 30_000_i64);
        let mut allure = Field::reference(&v, Some(default_allure()));
        let mut timeout = Field::reference(
            &v,
            Some(android_timeout_configuration(AndroidTimeoutParameters::empty())),
        );
        let mut file_sync = Field::reference(
            &v,
            Some(FileSyncConfiguration {
                pull: Vec::new(),
                push: Vec::new(),
            }),
        );
        let mut threading = Field::reference(&v, Some(default_android_threading()));
        let mut test_parser = Field::reference(&v, Some(AndroidTestParserConfiguration::Local));
        let mut test_access = Field::reference(&v, Some(default_test_access()));
        let mut adb_servers = Field::reference(&v, Some(vec![default_adb_endpoint()]));
        let mut disable_animation = Field::primitive(&v, true);
        let mut profiling = Field::reference(&v, Some(default_profiling()));
        let mut mock_location = Field::primitive(&v, false);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "androidSdk" => {
                    let bound = child.file()?;
                    android_sdk.set(child, bound);
                }
                "applicationApk" => {
                    let bound = child.file()?;
                    application_output.set(child, bound);
                }
                "testApplicationApk" => {
                    let bound = child.file()?;
                    test_application_output.set(child, bound);
                }
                "splitApks" => {
                    let bound = file_list(&child)?;
                    split_apks.set(child, bound);
                }
                "extraApplicationsApk" => {
                    let bound = file_list(&child)?;
                    extra_output.set(child, bound);
                }
                "outputs" => {
                    let bound = list_non_null(&child, bind_android_test_bundle_configuration)?;
                    outputs.set(child, bound);
                }
                "autoGrantPermission" => {
                    let bound = child.boolean()?;
                    auto_grant.set(child, Some(bound));
                }
                "instrumentationArgs" => {
                    let bound = string_map(&child)?;
                    instrumentation_args.set(child, bound);
                }
                "applicationPmClear" => {
                    let bound = child.boolean()?;
                    app_pm_clear.set(child, Some(bound));
                }
                "testApplicationPmClear" => {
                    let bound = child.boolean()?;
                    test_pm_clear.set(child, Some(bound));
                }
                "adbInitTimeoutMillis" => {
                    let bound = child.int()?;
                    adb_init.set(child, Some(bound));
                }
                "installOptions" => {
                    let bound = child.string()?;
                    install_options.set(child, bound);
                }
                "serialStrategy" => {
                    let bound = bind_serial_strategy(child.clone())?;
                    serial_strategy.set(child, bound);
                }
                "screenRecordConfiguration" => {
                    let bound = bind_android_screen_record_configuration(child.clone())?;
                    screen_record.set(child, bound);
                }
                "waitForDevicesTimeoutMillis" => {
                    let bound = child.long()?;
                    wait_for_devices.set(child, Some(bound));
                }
                "allureConfiguration" => {
                    let bound = bind_allure_configuration(child.clone())?;
                    allure.set(child, bound);
                }
                "timeoutConfiguration" => {
                    let bound = bind_android_timeout_configuration(child.clone())?;
                    timeout.set(child, bound);
                }
                "fileSyncConfiguration" => {
                    let bound = bind_file_sync_configuration(child.clone())?;
                    file_sync.set(child, bound);
                }
                "threadingConfiguration" => {
                    let bound = bind_android_threading_configuration(child.clone())?;
                    threading.set(child, bound);
                }
                "testParserConfiguration" => {
                    let bound = bind_android_test_parser_configuration(child.clone())?;
                    test_parser.set(child, bound);
                }
                "testAccessConfiguration" => {
                    let bound = bind_test_access_configuration(child.clone())?;
                    test_access.set(child, bound);
                }
                "adbServers" => {
                    let bound = list_non_null(&child, bind_adb_endpoint)?;
                    adb_servers.set(child, bound);
                }
                "disableWindowAnimation" => {
                    let bound = child.boolean()?;
                    disable_animation.set(child, Some(bound));
                }
                "profilingConfiguration" => {
                    let bound = bind_profiling_configuration(child.clone())?;
                    profiling.set(child, bound);
                }
                "mockLocation" => {
                    let bound = child.boolean()?;
                    mock_location.set(child, Some(bound));
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(AndroidConfiguration {
            android_sdk: android_sdk.optional(),
            application_output: application_output.optional(),
            test_application_output: test_application_output.optional(),
            split_apks: split_apks.optional(),
            extra_applications_output: extra_output.optional(),
            outputs: outputs.optional(),
            auto_grant_permission: auto_grant.required("autoGrantPermission")?,
            instrumentation_args: instrumentation_args.required("instrumentationArgs")?,
            application_pm_clear: app_pm_clear.required("applicationPmClear")?,
            test_application_pm_clear: test_pm_clear.required("testApplicationPmClear")?,
            adb_init_timeout_millis: adb_init.required("adbInitTimeoutMillis")?,
            install_options: install_options.required("installOptions")?,
            serial_strategy: serial_strategy.required("serialStrategy")?,
            screen_record_configuration: screen_record.required("screenRecordConfiguration")?,
            wait_for_devices_timeout_millis: wait_for_devices.required("waitForDevicesTimeoutMillis")?,
            allure_configuration: allure.required("allureConfiguration")?,
            timeout_configuration: timeout.required("timeoutConfiguration")?,
            file_sync_configuration: file_sync.required("fileSyncConfiguration")?,
            threading_configuration: threading.required("threadingConfiguration")?,
            test_parser_configuration: test_parser.required("testParserConfiguration")?,
            test_access_configuration: test_access.required("testAccessConfiguration")?,
            adb_servers: adb_servers.required("adbServers")?,
            disable_window_animation: disable_animation.required("disableWindowAnimation")?,
            profiling_configuration: profiling.required("profilingConfiguration")?,
            mock_location: mock_location.required("mockLocation")?,
        })
    })
}

// ---- Apple shared leaves ---------------------------------------------------------

pub fn bind_device_provider(value: Value<'_>) -> Result<Option<DeviceProvider>> {
    value.polymorphic(|id, v| match id {
        "static" | "marathondevices" => Some(
            bind_object(v.clone(), |v| {
                let mut path = Field::nullable(v, None::<JavaPath>);
                let mut object = v.object()?;
                while let Some((name, child)) = object.next_field()? {
                    match name.as_str() {
                        "path" => {
                            let bound = child.file()?;
                            path.set(child, bound);
                        }
                        _ => child.unknown(false)?,
                    }
                }
                Ok(DeviceProvider::Static { path: path.optional() })
            })
            .and_then(|bound| bound.ok_or_else(|| v.error(MISMATCHED, "non-null parameter is null"))),
        ),
        "dynamic" => Some(
            bind_object(v.clone(), |v| {
                let mut host = Field::reference(v, Some(String::from("127.0.0.1")));
                let mut port = Field::primitive(v, 5037);
                let mut object = v.object()?;
                while let Some((name, child)) = object.next_field()? {
                    match name.as_str() {
                        "host" => {
                            let bound = child.string()?;
                            host.set(child, bound);
                        }
                        "port" => {
                            let bound = child.int()?;
                            port.set(child, Some(bound));
                        }
                        _ => child.unknown(false)?,
                    }
                }
                Ok(DeviceProvider::Dynamic {
                    host: host.required("host")?,
                    port: port.required("port")?,
                })
            })
            .and_then(|bound| bound.ok_or_else(|| v.error(MISMATCHED, "non-null parameter is null"))),
        ),
        _ => None,
    })
}

/// Shorthand for the object body of a non-null composite behind `nullable`.
fn bind_object<T>(value: Value<'_>, bind: impl FnOnce(&Value<'_>) -> Result<T>) -> Result<Option<T>> {
    value.nullable(|v| bind(&v))
}

pub fn bind_rsync_configuration(value: Value<'_>) -> Result<Option<RsyncConfiguration>> {
    value.nullable(|v| {
        let mut remote_path = Field::reference(&v, Some(String::from("/usr/bin/rsync")));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "remotePath" => {
                    let bound = child.string()?;
                    remote_path.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(RsyncConfiguration {
            remote_path: remote_path.required("remotePath")?,
        })
    })
}

pub fn bind_ssh_authentication(value: Value<'_>) -> Result<Option<SshAuthentication>> {
    value.polymorphic(|id, v| match id {
        "password" => Some(
            bind_object(v.clone(), |v| {
                let mut username = Field::reference(v, None::<String>);
                let mut password = Field::reference(v, None::<String>);
                let mut object = v.object()?;
                while let Some((name, child)) = object.next_field()? {
                    match name.as_str() {
                        "username" => {
                            let bound = child.string()?;
                            username.set(child, bound);
                        }
                        "password" => {
                            let bound = child.string()?;
                            password.set(child, bound);
                        }
                        _ => child.unknown(false)?,
                    }
                }
                Ok(SshAuthentication::Password {
                    username: username.required("username")?,
                    password: password.required("password")?,
                })
            })
            .and_then(|bound| bound.ok_or_else(|| v.error(MISMATCHED, "non-null parameter is null"))),
        ),
        "publicKey" => Some(
            bind_object(v.clone(), |v| {
                let mut username = Field::reference(v, None::<String>);
                let mut key = Field::reference(v, None::<JavaPath>);
                let mut object = v.object()?;
                while let Some((name, child)) = object.next_field()? {
                    match name.as_str() {
                        "username" => {
                            let bound = child.string()?;
                            username.set(child, bound);
                        }
                        "key" => {
                            let bound = child.file()?;
                            key.set(child, bound);
                        }
                        _ => child.unknown(false)?,
                    }
                }
                Ok(SshAuthentication::PublicKey {
                    username: username.required("username")?,
                    key: key.required("key")?,
                })
            })
            .and_then(|bound| bound.ok_or_else(|| v.error(MISMATCHED, "non-null parameter is null"))),
        ),
        _ => None,
    })
}

pub fn bind_ssh_configuration(value: Value<'_>) -> Result<Option<SshConfiguration>> {
    value.nullable(|v| {
        let mut authentication = Field::nullable(&v, None::<SshAuthentication>);
        let mut known_hosts = Field::nullable(&v, None::<JavaPath>);
        let mut keep_alive_interval = Field::reference(&v, Some(secs(15)));
        let mut keep_alive_max = Field::primitive(&v, 4);
        let mut debug = Field::primitive(&v, false);
        let mut share = Field::primitive(&v, false);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "authentication" => {
                    let bound = bind_ssh_authentication(child.clone())?;
                    authentication.set(child, bound);
                }
                "knownHostsPath" => {
                    let bound = child.file()?;
                    known_hosts.set(child, bound);
                }
                "keepAliveInterval" => {
                    let bound = child.duration()?;
                    keep_alive_interval.set(child, bound);
                }
                "keepAliveMaxCount" => {
                    let bound = child.int()?;
                    keep_alive_max.set(child, Some(bound));
                }
                "debug" => {
                    let bound = child.boolean()?;
                    debug.set(child, Some(bound));
                }
                "shareWorkerConnection" => {
                    let bound = child.boolean()?;
                    share.set(child, Some(bound));
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(SshConfiguration {
            authentication: authentication.optional(),
            known_hosts_path: known_hosts.optional(),
            keep_alive_interval: keep_alive_interval.required("keepAliveInterval")?,
            keep_alive_max_count: keep_alive_max.required("keepAliveMaxCount")?,
            debug: debug.required("debug")?,
            share_worker_connection: share.required("shareWorkerConnection")?,
        })
    })
}

pub fn bind_test_type(value: &Value<'_>) -> Result<Option<TestType>> {
    bind_enum(
        value,
        &["xcuitest", "xctest", "logic_test"],
        &[TestType::Xcuitest, TestType::Xctest, TestType::LogicTest],
    )
}

pub fn bind_apple_test_bundle_configuration(value: Value<'_>) -> Result<Option<AppleTestBundleConfiguration>> {
    value.nullable(|v| {
        let mut application = Field::nullable(&v, None::<JavaPath>);
        let mut test_application = Field::nullable(&v, None::<JavaPath>);
        let mut extra = Field::nullable(&v, None::<Vec<JavaPath>>);
        let mut derived = Field::nullable(&v, None::<JavaPath>);
        let mut test_type = Field::nullable(&v, None::<TestType>);
        let mut xctestrun = Field::nullable(&v, None::<JavaPath>);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "application" => {
                    let bound = child.file()?;
                    application.set(child, bound);
                }
                "testApplication" => {
                    let bound = child.file()?;
                    test_application.set(child, bound);
                }
                "extraApplications" => {
                    let bound = file_list(&child)?;
                    extra.set(child, bound);
                }
                "derivedDataDir" => {
                    let bound = child.file()?;
                    derived.set(child, bound);
                }
                "testType" => {
                    let bound = bind_test_type(&child)?;
                    test_type.set(child, bound);
                }
                "xctestrun" => {
                    let bound = child.file()?;
                    xctestrun.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        let mut bundle = AppleTestBundleConfiguration::unresolved(
            application.optional(),
            test_application.optional(),
            extra.optional(),
            derived.optional(),
            test_type.optional(),
        );
        bundle.xctestrun = xctestrun.optional();
        Ok(bundle)
    })
}

pub fn bind_apple_threading_configuration(value: Value<'_>) -> Result<Option<AppleThreadingConfiguration>> {
    value.nullable(|v| {
        let mut provider_threads = Field::primitive(&v, 8);
        let mut device_threads = Field::primitive(&v, 2);
        let mut provisioning = Field::primitive(&v, 2);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "deviceProviderThreads" => {
                    let bound = child.int()?;
                    provider_threads.set(child, Some(bound));
                }
                "deviceThreads" => {
                    let bound = child.int()?;
                    device_threads.set(child, Some(bound));
                }
                "provisioningConcurrency" => {
                    let bound = child.int()?;
                    provisioning.set(child, Some(bound));
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(AppleThreadingConfiguration {
            device_provider_threads: provider_threads.required("deviceProviderThreads")?,
            device_threads: device_threads.required("deviceThreads")?,
            provisioning_concurrency: provisioning.required("provisioningConcurrency")?,
        })
    })
}

pub fn bind_apple_timeout_configuration(value: Value<'_>) -> Result<Option<AppleTimeoutConfiguration>> {
    value.nullable(|v| {
        let mut p = AppleTimeoutParameters::empty();
        let mut sources: [Option<Value<'_>>; 16] = [const { None }; 16];
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            macro_rules! dur {
                ($index:literal, $field:ident) => {{
                    p.$field = child.duration()?;
                    sources[$index] = Some(child.clone());
                }};
            }
            match name.as_str() {
                "shell" => dur!(0, shell),
                "shellIdle" => dur!(1, shell_idle),
                "reachability" => dur!(2, reachability),
                "screenshot" => dur!(3, screenshot),
                "video" => dur!(4, video),
                "erase" => dur!(5, erase),
                "shutdown" => dur!(6, shutdown),
                "delete" => dur!(7, delete),
                "create" => dur!(8, create),
                "boot" => dur!(9, boot),
                "install" => dur!(10, install),
                "uninstall" => dur!(11, uninstall),
                "importMedia" => dur!(12, import_media),
                "testDestination" => dur!(13, test_destination),
                "xcresulttool" => dur!(14, xcresulttool),
                "bootWait" => dur!(15, boot_wait),
                _ => child.unknown(false)?,
            }
        }
        let present = [
            (sources[0].is_some(), p.shell),
            (sources[1].is_some(), p.shell_idle),
            (sources[2].is_some(), p.reachability),
            (sources[3].is_some(), p.screenshot),
            (sources[4].is_some(), p.video),
            (sources[5].is_some(), p.erase),
            (sources[6].is_some(), p.shutdown),
            (sources[7].is_some(), p.delete),
            (sources[8].is_some(), p.create),
            (sources[9].is_some(), p.boot),
            (sources[10].is_some(), p.install),
            (sources[11].is_some(), p.uninstall),
            (sources[12].is_some(), p.import_media),
            (sources[13].is_some(), p.test_destination),
            (sources[14].is_some(), p.xcresulttool),
            (sources[15].is_some(), p.boot_wait),
        ];
        reject_explicit_nulls(&present, &sources)?;
        Ok(apple_timeout_configuration(p))
    })
}

pub fn bind_apple_test_parser_configuration(value: Value<'_>) -> Result<Option<AppleTestParserConfiguration>> {
    value.polymorphic(|id, v| match id {
        "nm" => Some(
            bind_object(v.clone(), |v| {
                let mut regexes = Field::reference(v, Some(vec![TestClassRegex::Default]));
                let mut object = v.object()?;
                while let Some((name, child)) = object.next_field()? {
                    match name.as_str() {
                        "testClassRegexes" => {
                            let bound = list_non_null(&child, |elem| Ok(elem.regex()?.map(TestClassRegex::Explicit)))?;
                            regexes.set(child, bound);
                        }
                        _ => child.unknown(false)?,
                    }
                }
                Ok(AppleTestParserConfiguration::Nm {
                    test_class_regexes: regexes.required("testClassRegexes")?,
                })
            })
            .and_then(|bound| bound.ok_or_else(|| v.error(MISMATCHED, "non-null parameter is null"))),
        ),
        "xctest" => Some(singleton(v).map(|()| AppleTestParserConfiguration::Xctest)),
        _ => None,
    })
}

// ---- iOS leaves ------------------------------------------------------------------

const DISPLAY_NAMES: [&str; 2] = ["internal", "external"];
const DISPLAYS: [Display; 2] = [Display::Internal, Display::External];
const MASK_NAMES: [&str; 2] = ["ignored", "black"];
const MASKS: [Mask; 2] = [Mask::Ignored, Mask::Black];
const LIFECYCLE_ACTION_NAMES: [&str; 3] = ["TERMINATE", "SHUTDOWN", "ERASE"];
const LIFECYCLE_ACTIONS: [LifecycleAction; 3] = [
    LifecycleAction::Terminate,
    LifecycleAction::Shutdown,
    LifecycleAction::Erase,
];
const IOS_PERMISSION_NAMES: [&str; 15] = [
    "all",
    "calendar",
    "contacts-limited",
    "contacts",
    "location",
    "location-always",
    "photos-add",
    "photos",
    "media-library",
    "microphone",
    "motion",
    "reminders",
    "siri",
    "user-tracking",
    "pasteboard",
];
const IOS_PERMISSIONS: [IosPermission; 15] = [
    IosPermission::All,
    IosPermission::Calendar,
    IosPermission::ContactsLimited,
    IosPermission::Contacts,
    IosPermission::Location,
    IosPermission::LocationAlways,
    IosPermission::PhotosAdd,
    IosPermission::Photos,
    IosPermission::MediaLibrary,
    IosPermission::Microphone,
    IosPermission::Motion,
    IosPermission::Reminders,
    IosPermission::Siri,
    IosPermission::UserTracking,
    IosPermission::Pasteboard,
];

pub fn bind_lifecycle_configuration(value: Value<'_>) -> Result<Option<LifecycleConfiguration>> {
    value.nullable(|v| {
        let mut shutdown_unused = Field::primitive(&v, true);
        let mut on_prepare = Field::reference(&v, Some(Vec::new()));
        let mut on_dispose = Field::reference(&v, Some(Vec::new()));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "shutdownUnused" => {
                    let bound = child.boolean()?;
                    shutdown_unused.set(child, Some(bound));
                }
                "onPrepare" => {
                    let bound = set_non_null(&child, |elem| {
                        bind_enum(&elem, &LIFECYCLE_ACTION_NAMES, &LIFECYCLE_ACTIONS)
                    })?;
                    on_prepare.set(child, bound);
                }
                "onDispose" => {
                    let bound = set_non_null(&child, |elem| {
                        bind_enum(&elem, &LIFECYCLE_ACTION_NAMES, &LIFECYCLE_ACTIONS)
                    })?;
                    on_dispose.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(LifecycleConfiguration {
            shutdown_unused: shutdown_unused.required("shutdownUnused")?,
            on_prepare: on_prepare.required("onPrepare")?,
            on_dispose: on_dispose.required("onDispose")?,
        })
    })
}

pub fn bind_ios_permissions_configuration(value: Value<'_>) -> Result<Option<IosPermissionsConfiguration>> {
    value.nullable(|v| {
        let mut grant = Field::reference(&v, Some(Vec::new()));
        let mut lifecycle = Field::reference(&v, Some(GrantLifecycle::BeforeTestRun));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "grant" => {
                    let bound = set_non_null(&child, |elem| bind_enum(&elem, &IOS_PERMISSION_NAMES, &IOS_PERMISSIONS))?;
                    grant.set(child, bound);
                }
                "lifecycle" => {
                    let bound = bind_enum(
                        &child,
                        &["BEFORE_TEST_RUN", "BEFORE_EACH_BATCH"],
                        &[GrantLifecycle::BeforeTestRun, GrantLifecycle::BeforeEachBatch],
                    )?;
                    lifecycle.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(IosPermissionsConfiguration {
            grant: grant.required("grant")?,
            lifecycle: lifecycle.required("lifecycle")?,
        })
    })
}

pub fn bind_ios_screenshot_configuration(value: Value<'_>) -> Result<Option<IosScreenshotConfiguration>> {
    value.nullable(|v| {
        let mut enabled = Field::primitive(&v, true);
        let mut screenshot_type = Field::reference(&v, Some(ScreenshotType::Jpeg));
        let mut display = Field::reference(&v, Some(Display::Internal));
        let mut mask = Field::reference(&v, Some(Mask::Black));
        let mut delay = Field::reference(&v, Some(millis(500)));
        let mut width = Field::primitive(&v, 720);
        let mut height = Field::primitive(&v, 1280);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "enabled" => {
                    let bound = child.boolean()?;
                    enabled.set(child, Some(bound));
                }
                "type" => {
                    let bound = bind_enum(
                        &child,
                        &["png", "tiff", "bmp", "jpeg"],
                        &[
                            ScreenshotType::Png,
                            ScreenshotType::Tiff,
                            ScreenshotType::Bmp,
                            ScreenshotType::Jpeg,
                        ],
                    )?;
                    screenshot_type.set(child, bound);
                }
                "display" => {
                    let bound = bind_enum(&child, &DISPLAY_NAMES, &DISPLAYS)?;
                    display.set(child, bound);
                }
                "mask" => {
                    let bound = bind_enum(&child, &MASK_NAMES, &MASKS)?;
                    mask.set(child, bound);
                }
                "delay" => {
                    let bound = child.duration()?;
                    delay.set(child, bound);
                }
                "width" => {
                    let bound = child.int()?;
                    width.set(child, Some(bound));
                }
                "height" => {
                    let bound = child.int()?;
                    height.set(child, Some(bound));
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(IosScreenshotConfiguration {
            enabled: enabled.required("enabled")?,
            screenshot_type: screenshot_type.required("type")?,
            display: display.required("display")?,
            mask: mask.required("mask")?,
            delay: delay.required("delay")?,
            width: width.required("width")?,
            height: height.required("height")?,
        })
    })
}

pub fn bind_transcoding_configuration(value: Value<'_>) -> Result<Option<TranscodingConfiguration>> {
    value.nullable(|v| {
        let mut enabled = Field::primitive(&v, false);
        let mut binary = Field::reference(&v, Some(String::from("/opt/homebrew/bin/ffmpeg")));
        let mut size = Field::reference(&v, Some(TranscodingSize::Hd720));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "enabled" => {
                    let bound = child.boolean()?;
                    enabled.set(child, Some(bound));
                }
                "ffmpegPath" => {
                    let bound = child.string()?;
                    binary.set(child, bound);
                }
                "size" => {
                    let bound = bind_enum(
                        &child,
                        &["hd480", "hd720", "hd1080"],
                        &[TranscodingSize::Hd480, TranscodingSize::Hd720, TranscodingSize::Hd1080],
                    )?;
                    size.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(TranscodingConfiguration {
            enabled: enabled.required("enabled")?,
            binary: binary.required("ffmpegPath")?,
            size: size.required("size")?,
        })
    })
}

pub fn bind_ios_video_configuration(value: Value<'_>) -> Result<Option<IosVideoConfiguration>> {
    value.nullable(|v| {
        let mut enabled = Field::primitive(&v, true);
        let mut codec = Field::reference(&v, Some(Codec::H264));
        let mut display = Field::reference(&v, Some(Display::Internal));
        let mut mask = Field::reference(&v, Some(Mask::Black));
        let mut transcoding = Field::reference(
            &v,
            Some(TranscodingConfiguration {
                enabled: false,
                binary: "/opt/homebrew/bin/ffmpeg".into(),
                size: TranscodingSize::Hd720,
            }),
        );
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "enabled" => {
                    let bound = child.boolean()?;
                    enabled.set(child, Some(bound));
                }
                "codec" => {
                    let bound = bind_enum(&child, &["h264", "hevc"], &[Codec::H264, Codec::Hevc])?;
                    codec.set(child, bound);
                }
                "display" => {
                    let bound = bind_enum(&child, &DISPLAY_NAMES, &DISPLAYS)?;
                    display.set(child, bound);
                }
                "mask" => {
                    let bound = bind_enum(&child, &MASK_NAMES, &MASKS)?;
                    mask.set(child, bound);
                }
                "transcoding" => {
                    let bound = bind_transcoding_configuration(child.clone())?;
                    transcoding.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(IosVideoConfiguration {
            enabled: enabled.required("enabled")?,
            codec: codec.required("codec")?,
            display: display.required("display")?,
            mask: mask.required("mask")?,
            transcoding: transcoding.required("transcoding")?,
        })
    })
}

pub fn bind_ios_screen_record_configuration(value: Value<'_>) -> Result<Option<IosScreenRecordConfiguration>> {
    value.nullable(|v| {
        let mut recorder = Field::nullable(&v, None::<IosRecorderType>);
        let mut video = Field::reference(&v, Some(default_ios_video()));
        let mut screenshot = Field::reference(&v, Some(default_ios_screenshot()));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "preferableRecorderType" => {
                    let bound = child.enum_creator_named(
                        "com.malinskiy.marathon.config.vendor.apple.ios.RecorderType",
                        creator(&RECORDER_NAMES, &IOS_RECORDERS),
                    )?;
                    recorder.set(child, bound);
                }
                "videoConfiguration" => {
                    let bound = bind_ios_video_configuration(child.clone())?;
                    video.set(child, bound);
                }
                "screenshotConfiguration" => {
                    let bound = bind_ios_screenshot_configuration(child.clone())?;
                    screenshot.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(IosScreenRecordConfiguration {
            preferable_recorder_type: recorder.optional(),
            video_configuration: video.required("videoConfiguration")?,
            screenshot_configuration: screenshot.required("screenshotConfiguration")?,
        })
    })
}

pub fn bind_signing_configuration(value: Value<'_>) -> Result<Option<SigningConfiguration>> {
    value.nullable(|v| {
        let mut profile = Field::nullable(&v, None::<String>);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "testRunnerProvisioningProfile" => {
                    let bound = child.string()?;
                    profile.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(SigningConfiguration {
            test_runner_provisioning_profile: profile.optional(),
        })
    })
}

pub fn bind_worker_connection_configuration(value: Value<'_>) -> Result<Option<WorkerConnectionConfiguration>> {
    value.nullable(|v| {
        let mut attempts = Field::primitive(&v, 3);
        let mut backoff = Field::reference(&v, Some(secs(10)));
        let mut retry_interval = Field::reference(&v, Some(secs(60)));
        let mut late_join = Field::primitive(&v, true);
        let mut cutoff = Field::reference(&v, Some(secs(600)));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "attempts" => {
                    let bound = child.int()?;
                    attempts.set(child, Some(bound));
                }
                "backoff" => {
                    let bound = child.duration()?;
                    backoff.set(child, bound);
                }
                "retryInterval" => {
                    let bound = child.duration()?;
                    retry_interval.set(child, bound);
                }
                "lateJoin" => {
                    let bound = child.boolean()?;
                    late_join.set(child, Some(bound));
                }
                "lateJoinCutoff" => {
                    let bound = child.duration()?;
                    cutoff.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(WorkerConnectionConfiguration {
            attempts: attempts.required("attempts")?,
            backoff: backoff.required("backoff")?,
            retry_interval: retry_interval.required("retryInterval")?,
            late_join: late_join.required("lateJoin")?,
            late_join_cutoff: cutoff.required("lateJoinCutoff")?,
        })
    })
}

const LIFETIME_NAMES: [&str; 3] = ["KEEP_ALWAYS", "DELETE_ON_SUCCESS", "KEEP_NEVER"];
const LIFETIMES: [Lifetime; 3] = [Lifetime::KeepAlways, Lifetime::DeleteOnSuccess, Lifetime::KeepNever];

pub fn bind_attachments_configuration(value: Value<'_>) -> Result<Option<AttachmentsConfiguration>> {
    value.nullable(|v| {
        let mut system = Field::reference(&v, Some(Lifetime::DeleteOnSuccess));
        let mut user = Field::reference(&v, Some(Lifetime::KeepAlways));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "systemAttachmentLifetime" => {
                    let bound = bind_enum(&child, &LIFETIME_NAMES, &LIFETIMES)?;
                    system.set(child, bound);
                }
                "userAttachmentLifetime" => {
                    let bound = bind_enum(&child, &LIFETIME_NAMES, &LIFETIMES)?;
                    user.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(AttachmentsConfiguration {
            system_attachment_lifetime: system.required("systemAttachmentLifetime")?,
            user_attachment_lifetime: user.required("userAttachmentLifetime")?,
        })
    })
}

pub fn bind_xcresult_queue_configuration(value: Value<'_>) -> Result<Option<XcresultQueueConfiguration>> {
    value.nullable(|v| {
        let mut capacity = Field::primitive(&v, XcresultQueueConfiguration::DEFAULT_CAPACITY);
        let mut concurrency = Field::primitive(&v, XcresultQueueConfiguration::DEFAULT_CONCURRENCY);
        let mut worker_capacity = Field::nullable(&v, None::<i32>);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "capacity" => {
                    let bound = child.int()?;
                    capacity.set(child, Some(bound));
                }
                "concurrency" => {
                    let bound = child.int()?;
                    concurrency.set(child, Some(bound));
                }
                "workerCapacity" => {
                    let bound = child.nullable(|elem| elem.int())?;
                    worker_capacity.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(XcresultQueueConfiguration {
            capacity: capacity.required("capacity")?,
            concurrency: concurrency.required("concurrency")?,
            worker_capacity: worker_capacity.optional(),
        })
    })
}

pub fn bind_xcresult_configuration(value: Value<'_>) -> Result<Option<XcresultConfiguration>> {
    value.nullable(|v| {
        let mut pull = Field::nullable(&v, None::<bool>);
        let mut policy = Field::reference(&v, Some(PullingPolicy::Always));
        let mut remote_clean = Field::primitive(&v, true);
        let mut attachments = Field::reference(
            &v,
            Some(AttachmentsConfiguration {
                system_attachment_lifetime: Lifetime::DeleteOnSuccess,
                user_attachment_lifetime: Lifetime::KeepAlways,
            }),
        );
        let mut format = Field::nullable(&v, None::<ScreenCaptureFormat>);
        let mut queue = Field::reference(&v, Some(XcresultQueueConfiguration::default_instance()));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "pull" => {
                    let bound = child.nullable(|elem| elem.boolean())?;
                    pull.set(child, bound);
                }
                "pullingPolicy" => {
                    let bound = bind_enum(
                        &child,
                        &["NEVER", "ALWAYS", "ON_FAILURE"],
                        &[PullingPolicy::Never, PullingPolicy::Always, PullingPolicy::OnFailure],
                    )?;
                    policy.set(child, bound);
                }
                "remoteClean" => {
                    let bound = child.boolean()?;
                    remote_clean.set(child, Some(bound));
                }
                "attachments" => {
                    let bound = bind_attachments_configuration(child.clone())?;
                    attachments.set(child, bound);
                }
                "preferredScreenCaptureFormat" => {
                    let bound = bind_enum(
                        &child,
                        &["SCREENSHOTS", "SCREEN_RECORDING"],
                        &[ScreenCaptureFormat::Screenshots, ScreenCaptureFormat::ScreenRecording],
                    )?;
                    format.set(child, bound);
                }
                "queue" => {
                    let bound = bind_xcresult_queue_configuration(child.clone())?;
                    queue.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(XcresultConfiguration {
            pull: pull.optional(),
            pulling_policy: policy.required("pullingPolicy")?,
            remote_clean: remote_clean.required("remoteClean")?,
            attachments: attachments.required("attachments")?,
            preferred_screen_capture_format: format.optional(),
            queue: queue.required("queue")?,
        })
    })
}

pub fn bind_xctestrun_env_configuration(value: Value<'_>) -> Result<Option<XctestrunEnvConfiguration>> {
    value.nullable(|v| {
        let mut app = Field::reference(&v, Some(Vec::new()));
        let mut test = Field::reference(&v, Some(Vec::new()));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "app" => {
                    let bound = string_map(&child)?;
                    app.set(child, bound);
                }
                "test" => {
                    let bound = string_map(&child)?;
                    test.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(XctestrunEnvConfiguration {
            app_envs: app.required("app")?,
            test_envs: test.required("test")?,
        })
    })
}

// ---- macOS leaves ------------------------------------------------------------------

const MACOS_PERMISSION_NAMES: [&str; 2] = ["accessibility", "developer-tool"];
const MACOS_PERMISSIONS: [MacosPermission; 2] = [MacosPermission::Accessibility, MacosPermission::DeveloperTool];

pub fn bind_macos_permissions_configuration(value: Value<'_>) -> Result<Option<MacosPermissionsConfiguration>> {
    value.nullable(|v| {
        let mut grant = Field::reference(&v, Some(Vec::new()));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "grant" => {
                    let bound = child.map(|elem| {
                        let set = set_non_null(&elem, |item| {
                            bind_enum(&item, &MACOS_PERMISSION_NAMES, &MACOS_PERMISSIONS)
                        })?;
                        set.ok_or_else(|| elem.error(MISMATCHED, "D9: null element in collection"))
                    })?;
                    grant.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(MacosPermissionsConfiguration {
            grant: grant.required("grant")?,
        })
    })
}

// ---- Top-level iOS / macOS ----------------------------------------------------------

pub fn bind_ios_configuration(value: Value<'_>) -> Result<Option<IosConfiguration>> {
    value.nullable(|v| {
        let mut bundle = Field::nullable(&v, None::<AppleTestBundleConfiguration>);
        let mut device_provider = Field::reference(&v, Some(DeviceProvider::Static { path: None }));
        let mut devices_file = Field::nullable(&v, None::<JavaPath>);
        let mut ssh = Field::reference(&v, Some(default_ssh()));
        let mut worker_connection = Field::reference(&v, Some(default_worker_connection()));
        let mut xcresult = Field::reference(&v, Some(default_xcresult()));
        let mut screen_record = Field::reference(&v, Some(default_ios_screen_record()));
        let mut xctestrun_env = Field::reference(&v, Some(default_xctestrun_env()));
        let mut lifecycle = Field::reference(&v, Some(default_lifecycle()));
        let mut permissions = Field::reference(&v, Some(default_ios_permissions()));
        let mut media_files = Field::nullable(&v, None::<Vec<JavaPath>>);
        let mut timeout = Field::reference(&v, Some(apple_timeout_configuration(AppleTimeoutParameters::empty())));
        let mut threading = Field::reference(&v, Some(AppleThreadingConfiguration::default_instance()));
        let mut hide_runner = Field::primitive(&v, false);
        let mut compact = Field::primitive(&v, false);
        let mut rsync = Field::reference(&v, Some(default_rsync()));
        let mut test_args = Field::reference(&v, Some(Vec::new()));
        let mut data_clear = Field::primitive(&v, false);
        let mut test_parser = Field::reference(&v, Some(default_nm()));
        let mut device_log = Field::primitive(&v, false);
        let mut signing = Field::reference(&v, Some(default_signing()));
        let mut skip_derived = Field::primitive(&v, false);
        let mut max_batches = Field::primitive(&v, 0);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "bundle" => {
                    let bound = bind_apple_test_bundle_configuration(child.clone())?;
                    bundle.set(child, bound);
                }
                "deviceProvider" => {
                    let bound = bind_device_provider(child.clone())?;
                    device_provider.set(child, bound);
                }
                "devices" => {
                    let bound = child.file()?;
                    devices_file.set(child, bound);
                }
                "ssh" => {
                    let bound = bind_ssh_configuration(child.clone())?;
                    ssh.set(child, bound);
                }
                "workerConnection" => {
                    let bound = bind_worker_connection_configuration(child.clone())?;
                    worker_connection.set(child, bound);
                }
                "xcresult" => {
                    let bound = bind_xcresult_configuration(child.clone())?;
                    xcresult.set(child, bound);
                }
                "screenRecordConfiguration" => {
                    let bound = bind_ios_screen_record_configuration(child.clone())?;
                    screen_record.set(child, bound);
                }
                "xctestrunEnv" => {
                    let bound = bind_xctestrun_env_configuration(child.clone())?;
                    xctestrun_env.set(child, bound);
                }
                "lifecycle" => {
                    let bound = bind_lifecycle_configuration(child.clone())?;
                    lifecycle.set(child, bound);
                }
                "permissions" => {
                    let bound = bind_ios_permissions_configuration(child.clone())?;
                    permissions.set(child, bound);
                }
                "importMedia" => {
                    let bound = file_list(&child)?;
                    media_files.set(child, bound);
                }
                "timeoutConfiguration" => {
                    let bound = bind_apple_timeout_configuration(child.clone())?;
                    timeout.set(child, bound);
                }
                "threadingConfiguration" => {
                    let bound = bind_apple_threading_configuration(child.clone())?;
                    threading.set(child, bound);
                }
                "hideRunnerOutput" => {
                    let bound = child.boolean()?;
                    hide_runner.set(child, Some(bound));
                }
                "compactOutput" => {
                    let bound = child.boolean()?;
                    compact.set(child, Some(bound));
                }
                "rsync" => {
                    let bound = bind_rsync_configuration(child.clone())?;
                    rsync.set(child, bound);
                }
                "xcodebuildTestArgs" => {
                    let bound = string_map(&child)?;
                    test_args.set(child, bound);
                }
                "dataContainerClear" => {
                    let bound = child.boolean()?;
                    data_clear.set(child, Some(bound));
                }
                "testParserConfiguration" => {
                    let bound = bind_apple_test_parser_configuration(child.clone())?;
                    test_parser.set(child, bound);
                }
                "deviceLog" => {
                    let bound = child.boolean()?;
                    device_log.set(child, Some(bound));
                }
                "signing" => {
                    let bound = bind_signing_configuration(child.clone())?;
                    signing.set(child, bound);
                }
                "skipCopyingDerivedData" => {
                    let bound = child.boolean()?;
                    skip_derived.set(child, Some(bound));
                }
                "maxBatchesWithoutProgress" => {
                    let bound = child.int()?;
                    max_batches.set(child, Some(bound));
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(IosConfiguration {
            bundle: bundle.optional(),
            device_provider: device_provider.required("deviceProvider")?,
            devices_file: devices_file.optional(),
            ssh: ssh.required("ssh")?,
            worker_connection: worker_connection.required("workerConnection")?,
            xcresult: xcresult.required("xcresult")?,
            screen_record_configuration: screen_record.required("screenRecordConfiguration")?,
            xctestrun_env: xctestrun_env.required("xctestrunEnv")?,
            lifecycle_configuration: lifecycle.required("lifecycle")?,
            permissions: permissions.required("permissions")?,
            media_files: media_files.optional(),
            timeout_configuration: timeout.required("timeoutConfiguration")?,
            threading_configuration: threading.required("threadingConfiguration")?,
            hide_runner_output: hide_runner.required("hideRunnerOutput")?,
            compact_output: compact.required("compactOutput")?,
            rsync: rsync.required("rsync")?,
            xcodebuild_test_args: test_args.required("xcodebuildTestArgs")?,
            data_container_clear: data_clear.required("dataContainerClear")?,
            test_parser_configuration: test_parser.required("testParserConfiguration")?,
            device_log: device_log.required("deviceLog")?,
            signing: signing.required("signing")?,
            skip_copying_derived_data: skip_derived.required("skipCopyingDerivedData")?,
            max_batches_without_progress: max_batches.required("maxBatchesWithoutProgress")?,
        })
    })
}

pub fn bind_macos_configuration(value: Value<'_>) -> Result<Option<MacosConfiguration>> {
    value.nullable(|v| {
        let mut bundle = Field::nullable(&v, None::<AppleTestBundleConfiguration>);
        let mut device_provider = Field::reference(&v, Some(DeviceProvider::Static { path: None }));
        let mut devices_file = Field::nullable(&v, None::<JavaPath>);
        let mut ssh = Field::reference(&v, Some(default_ssh()));
        let mut xcresult = Field::reference(&v, Some(default_xcresult()));
        let mut xctestrun_env = Field::reference(&v, Some(default_xctestrun_env()));
        let mut timeout = Field::reference(&v, Some(apple_timeout_configuration(AppleTimeoutParameters::empty())));
        let mut threading = Field::reference(&v, Some(AppleThreadingConfiguration::default_instance()));
        let mut hide_runner = Field::primitive(&v, false);
        let mut compact = Field::primitive(&v, false);
        let mut rsync = Field::reference(&v, Some(default_rsync()));
        let mut test_args = Field::reference(&v, Some(Vec::new()));
        let mut test_parser = Field::reference(&v, Some(default_nm()));
        let mut permissions = Field::reference(&v, Some(MacosPermissionsConfiguration { grant: Vec::new() }));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "bundle" => {
                    let bound = bind_apple_test_bundle_configuration(child.clone())?;
                    bundle.set(child, bound);
                }
                "deviceProvider" => {
                    let bound = bind_device_provider(child.clone())?;
                    device_provider.set(child, bound);
                }
                "devices" => {
                    let bound = child.file()?;
                    devices_file.set(child, bound);
                }
                "ssh" => {
                    let bound = bind_ssh_configuration(child.clone())?;
                    ssh.set(child, bound);
                }
                "xcresult" => {
                    let bound = bind_xcresult_configuration(child.clone())?;
                    xcresult.set(child, bound);
                }
                "xctestrunEnv" => {
                    let bound = bind_xctestrun_env_configuration(child.clone())?;
                    xctestrun_env.set(child, bound);
                }
                "timeoutConfiguration" => {
                    let bound = bind_apple_timeout_configuration(child.clone())?;
                    timeout.set(child, bound);
                }
                "threadingConfiguration" => {
                    let bound = bind_apple_threading_configuration(child.clone())?;
                    threading.set(child, bound);
                }
                "hideRunnerOutput" => {
                    let bound = child.boolean()?;
                    hide_runner.set(child, Some(bound));
                }
                "compactOutput" => {
                    let bound = child.boolean()?;
                    compact.set(child, Some(bound));
                }
                "rsync" => {
                    let bound = bind_rsync_configuration(child.clone())?;
                    rsync.set(child, bound);
                }
                "xcodebuildTestArgs" => {
                    let bound = string_map(&child)?;
                    test_args.set(child, bound);
                }
                "testParserConfiguration" => {
                    let bound = bind_apple_test_parser_configuration(child.clone())?;
                    test_parser.set(child, bound);
                }
                "permissions" => {
                    let bound = bind_macos_permissions_configuration(child.clone())?;
                    permissions.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(MacosConfiguration {
            bundle: bundle.optional(),
            device_provider: device_provider.required("deviceProvider")?,
            devices_file: devices_file.optional(),
            ssh: ssh.required("ssh")?,
            xcresult: xcresult.required("xcresult")?,
            xctestrun_env: xctestrun_env.required("xctestrunEnv")?,
            timeout_configuration: timeout.required("timeoutConfiguration")?,
            threading_configuration: threading.required("threadingConfiguration")?,
            hide_runner_output: hide_runner.required("hideRunnerOutput")?,
            compact_output: compact.required("compactOutput")?,
            rsync: rsync.required("rsync")?,
            xcodebuild_test_args: test_args.required("xcodebuildTestArgs")?,
            test_parser_configuration: test_parser.required("testParserConfiguration")?,
            permissions: permissions.required("permissions")?,
        })
    })
}

// ---- Vendor dispatch (VendorConfiguration.kt:44-55) ------------------------------

/// The vendor binder for WP 2.2's `bind_configuration` vendor-binder
/// parameter (`impl FnMut(Value) -> Result<Option<VendorConfiguration>>`).
/// The dispatch can produce `Empty`; rejecting it at parse is WP 2.4's
/// loader staging, not this binder's.
pub fn bind_vendor_configuration(value: Value<'_>) -> Result<Option<VendorConfiguration>> {
    value.polymorphic(|id, v| match id {
        "Android" => Some(
            bind_android_configuration(v.clone())
                .and_then(|bound| bound.ok_or_else(|| v.error(MISMATCHED, "non-null parameter is null")))
                .map(VendorConfiguration::Android),
        ),
        "iOS" | "watchOS" | "tvOS" | "visionOS" => Some(
            bind_ios_configuration(v.clone())
                .and_then(|bound| bound.ok_or_else(|| v.error(MISMATCHED, "non-null parameter is null")))
                .map(VendorConfiguration::Ios),
        ),
        "macOS" => Some(
            bind_macos_configuration(v.clone())
                .and_then(|bound| bound.ok_or_else(|| v.error(MISMATCHED, "non-null parameter is null")))
                .map(VendorConfiguration::Macos),
        ),
        "stub" => Some(singleton(v).map(|()| VendorConfiguration::Stub)),
        "empty" => Some(singleton(v).map(|()| VendorConfiguration::Empty)),
        _ => None,
    })
}
