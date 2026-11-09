//! Port of configuration/src/test/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactoryTest.kt
//! and configuration/src/test/kotlin/com/malinskiy/marathon/config/serialization/IosConfigurationFactoryTest.kt.
//!
//! Whole Marathonfiles through the assembled stage (a). Each test keeps the
//! Kotlin test's `marathonfileDir` choice: the parent of `fixture/config`
//! for `ConfigurationFactoryTest` (Android only, nothing is written), the
//! `ios` directory of a temp copy for `IosConfigurationFactoryTest`. Expected
//! values are the Kotlin test's, with the defaults it leaves to the Kotlin
//! constructors transcribed from them.

#[path = "support/load.rs"]
mod load;

use jcompat::{
    JavaRegex,
    jtime::{Duration, Instant, duration_of, parse_duration, parse_instant},
};
use marathon_config::{
    AnalyticsConfiguration, BatchingStrategyConfiguration, Configuration, Defaults, FlakinessStrategyConfiguration,
    InfluxDbConfiguration, PoolingStrategyConfiguration, RegexValuesFileFilter, RetentionPolicyConfiguration,
    RetryStrategyConfiguration, ScreenRecordingPolicy, ShardingStrategyConfiguration, SortingStrategyConfiguration,
    TestFilterConfiguration, TestPackageFilter, VendorConfiguration, vendor::*,
};
use std::path::{Path, PathBuf};

fn secs(s: i64) -> Duration {
    Duration::of_seconds(s)
}
fn millis(ms: i64) -> Duration {
    duration_of(0, ms * 1_000_000).unwrap()
}
fn iso(text: &str) -> Instant {
    parse_instant(text).unwrap()
}
fn re(pattern: &str) -> JavaRegex {
    JavaRegex::compile(pattern, "test").unwrap()
}
fn s(text: &str) -> String {
    text.into()
}
fn p(dir: &Path, rel: &str) -> PathBuf {
    load::java_path(&dir.join(rel))
}

/// `File(path).canonicalFile` for a path whose leaf may not exist: the
/// longest existing prefix is resolved, the rest appended.
fn canonical_file(path: &str) -> PathBuf {
    let mut existing = PathBuf::from(path);
    let mut rest = Vec::new();
    while !existing.exists() {
        rest.push(existing.file_name().unwrap().to_owned());
        existing.pop();
    }
    let mut out = existing.canonicalize().unwrap();
    out.extend(rest.iter().rev());
    load::java_path(&out)
}

/// `ConfigurationFactoryTest.mockMarathonFileDir`: the parent of `fixture/config`.
fn mock_marathonfile_dir() -> PathBuf {
    load::fixture_config().join("..").canonicalize().unwrap()
}
fn parse_sample(
    name: &str,
    sdk: Option<&str>,
) -> Result<Configuration, Box<marathon_config::binding::ConfigurationError>> {
    let dir = mock_marathonfile_dir();
    let file = dir.join("config").join(name);
    load::parse(&file, &load::context(&file, &dir, sdk))
}

fn values_filter(regex: Option<&str>, values: Option<&[&str]>, file: Option<PathBuf>) -> RegexValuesFileFilter {
    RegexValuesFileFilter {
        regex: regex.map(re),
        values: values.map(|v| v.iter().copied().map(s).collect()),
        file,
        enabled: true,
    }
}
fn package_filter(regex: &str) -> TestFilterConfiguration {
    TestFilterConfiguration::TestPackage(TestPackageFilter {
        regex: Some(re(regex)),
        values: None,
        file: None,
        subpackages: false,
        enabled: true,
    })
}

/// `VendorConfiguration.AndroidConfiguration`'s constructor defaults after
/// the four leading parameters, as the Kotlin tests pass them positionally.
fn android(sdk: PathBuf, app: Option<PathBuf>, test_app: Option<PathBuf>) -> AndroidConfiguration {
    AndroidConfiguration {
        android_sdk: Some(sdk),
        application_output: app,
        test_application_output: test_app,
        split_apks: None,
        extra_applications_output: None,
        outputs: None,
        auto_grant_permission: false,
        instrumentation_args: Vec::new(),
        application_pm_clear: false,
        test_application_pm_clear: false,
        adb_init_timeout_millis: 30_000,
        install_options: s(""),
        serial_strategy: SerialStrategy::Automatic,
        screen_record_configuration: AndroidScreenRecordConfiguration {
            preferable_recorder_type: None,
            video_configuration: AndroidVideoConfiguration {
                enabled: true,
                width: 720,
                height: 1280,
                bitrate_mbps: 1,
                time_limit: 180,
                time_limit_units: TimeUnit::Seconds,
            },
            screenshot_configuration: AndroidScreenshotConfiguration {
                enabled: true,
                width: 720,
                height: 1280,
                delay_ms: 500,
            },
        },
        wait_for_devices_timeout_millis: 30_000,
        allure_configuration: AllureConfiguration {
            enabled: false,
            relative_results_directory: s("/files/allure-results"),
            path_root: PathRoot::AppData,
        },
        timeout_configuration: AndroidTimeoutConfiguration {
            shell: secs(20),
            list_files: secs(20),
            push_file: secs(60),
            push_folder: secs(60),
            pull_file: secs(30),
            uninstall: secs(20),
            install: secs(60),
            screenrecorder: secs(200),
            screencapturer: millis(300),
            socket_idle_timeout: secs(30),
            port_forward: secs(20),
            boot: secs(30),
        },
        file_sync_configuration: FileSyncConfiguration {
            pull: Vec::new(),
            push: Vec::new(),
        },
        threading_configuration: AndroidThreadingConfiguration {
            boot_waiting_threads: 4,
            adb_io_threads: 4,
            install_threads: 8,
        },
        test_parser_configuration: AndroidTestParserConfiguration::Local,
        test_access_configuration: TestAccessConfiguration {
            adb: false,
            grpc: false,
            console: false,
            console_token: s(""),
        },
        adb_servers: vec![AdbEndpoint {
            host: s("127.0.0.1"),
            port: 5037,
        }],
        disable_window_animation: true,
        profiling_configuration: ProfilingConfiguration {
            enabled: false,
            pbtxt: None,
            url_template: s("https://cloud.marathonlabs.io/"),
        },
        mock_location: false,
    }
}

const APP: &str = "kotlin-buildscript/build/outputs/apk/debug/kotlin-buildscript-debug.apk";
const TEST_APP: &str =
    "kotlin-buildscript/build/outputs/apk/androidTest/debug/kotlin-buildscript-debug-androidTest.apk";

#[test]
fn on_sample_config_1_should_deserialize() {
    let dir = mock_marathonfile_dir();
    let configuration = parse_sample("sample_1.yaml", None).unwrap();

    assert_eq!(configuration.name, s("sample-app tests"));
    assert_eq!(configuration.output_dir.as_os_str(), p(&dir, "marathon").as_os_str());
    assert_eq!(
        configuration.analytics_configuration,
        AnalyticsConfiguration::InfluxDb(InfluxDbConfiguration {
            url: s("http://influx.svc.cluster.local:8086"),
            user: s("root"),
            password: s("root"),
            db_name: s("marathon"),
            retention_policy_configuration: RetentionPolicyConfiguration::default_policy(),
            defaults: Defaults {
                success_rate: 0.0,
                duration: secs(300),
                observed_duration_min_tests: None,
            },
            read_only: false,
        })
    );
    use PoolingStrategyConfiguration as P;
    assert_eq!(
        configuration.pooling_strategy,
        P::Combo(vec![
            P::Omni,
            P::Model,
            P::OperatingSystemVersion,
            P::Manufacturer,
            P::Abi
        ])
    );
    assert_eq!(
        configuration.sharding_strategy,
        ShardingStrategyConfiguration::Count { count: 5 }
    );
    assert_eq!(
        configuration.sorting_strategy,
        SortingStrategyConfiguration::SuccessRate {
            time_limit: iso("2015-03-14T09:26:53.590Z"),
            ascending: false,
        }
    );
    assert_eq!(
        configuration.batching_strategy,
        BatchingStrategyConfiguration::FixedSize {
            size: 5,
            duration_millis: None,
            percentile: None,
            time_limit: None,
            last_mile_length: 0,
        }
    );
    assert_eq!(
        configuration.flakiness_strategy,
        FlakinessStrategyConfiguration::Probability {
            min_success_rate: 0.7,
            max_count: 3,
            time_limit: iso("2015-03-14T09:26:53.590Z"),
        }
    );
    assert_eq!(
        configuration.retry_strategy,
        RetryStrategyConfiguration::FixedQuota {
            total_allowed_retry_quota: 100,
            retry_per_test_quota: 2,
        }
    );

    use TestFilterConfiguration as F;
    // Kotlin's `shouldContainSame` ignores order; the binder keeps it.
    assert_eq!(
        configuration.filtering_configuration.allowlist,
        vec![
            F::SimpleClassname(values_filter(Some(".*"), None, None)),
            F::SimpleClassname(values_filter(None, Some(&["SimpleTest"]), None)),
            F::FullyQualifiedClassname(values_filter(None, None, Some(p(&dir, "filterfile")))),
            F::FullyQualifiedClassname(values_filter(Some(".*"), None, None)),
            F::TestMethod(values_filter(Some(".*"), None, None)),
            F::Composition {
                filters: vec![
                    package_filter(".*"),
                    F::TestMethod(values_filter(Some(".*"), None, None))
                ],
                op: marathon_config::CompositionOperation::Union,
                enabled: true,
            },
        ]
    );
    assert_eq!(
        configuration.filtering_configuration.blocklist,
        vec![
            package_filter(".*"),
            F::Annotation(values_filter(Some(".*"), None, None)),
            F::AnnotationData {
                name_regex: re(".*"),
                value_regex: re(".*"),
                enabled: true,
            },
        ]
    );

    let joined = |regexes: &[JavaRegex]| regexes.iter().map(JavaRegex::pattern).collect::<String>();
    assert_eq!(joined(&configuration.include_serial_regexes), "emulator-500[2,4]");
    assert_eq!(joined(&configuration.exclude_serial_regexes), "emulator-5002");
    assert!(!configuration.ignore_failures);
    assert!(!configuration.is_code_coverage_enabled);
    assert_eq!(configuration.test_batch_timeout_millis, 20_000);
    assert_eq!(configuration.test_output_timeout_millis, 30_000);
    assert!(configuration.debug);
    assert_eq!(configuration.screen_recording_policy, ScreenRecordingPolicy::OnAny);
    assert_eq!(configuration.device_initialization_timeout_millis, 300_000);

    let mut expected = android(
        PathBuf::from("/local/android"),
        Some(p(&dir, APP)),
        Some(p(&dir, TEST_APP)),
    );
    expected.split_apks = Some(vec![p(
        &dir,
        "kotlin-buildscript/build/outputs/apk/androidTest/debug/kotlin-buildscript-split-debug.apk",
    )]);
    expected.auto_grant_permission = true;
    expected.instrumentation_args = vec![("debug".to_owned(), s("false"))];
    expected.application_pm_clear = true;
    expected.test_application_pm_clear = true;
    expected.install_options = s("-d");
    expected.screen_record_configuration = AndroidScreenRecordConfiguration {
        preferable_recorder_type: Some(AndroidRecorderType::Screenshot),
        video_configuration: AndroidVideoConfiguration {
            enabled: false,
            width: 1080,
            height: 1920,
            bitrate_mbps: 2,
            time_limit: 300,
            time_limit_units: TimeUnit::Seconds,
        },
        screenshot_configuration: AndroidScreenshotConfiguration {
            enabled: false,
            width: 1080,
            height: 1920,
            delay_ms: 200,
        },
    };
    expected.wait_for_devices_timeout_millis = 15_000;
    assert_eq!(
        configuration.vendor_configuration,
        VendorConfiguration::Android(expected)
    );
}

#[test]
fn on_sample_config_1_with_custom_retention_policy_should_deserialize() {
    let configuration = parse_sample("sample_1_rp.yaml", None).unwrap();
    assert_eq!(
        configuration.analytics_configuration,
        AnalyticsConfiguration::InfluxDb(InfluxDbConfiguration {
            url: s("http://influx.svc.cluster.local:8086"),
            user: s("root"),
            password: s("root"),
            db_name: s("marathon"),
            retention_policy_configuration: RetentionPolicyConfiguration {
                name: s("rpMarathonTest"),
                duration: s("90d"),
                shard_duration: s("1h"),
                replication_factor: 5,
                is_default: false,
            },
            defaults: Defaults {
                success_rate: 0.0,
                duration: secs(300),
                observed_duration_min_tests: None,
            },
            read_only: false,
        })
    );
}

#[test]
fn on_sample_config_2_with_minimal_configuration_should_deserialize() {
    let dir = mock_marathonfile_dir();
    let configuration = parse_sample("sample_2.yaml", None).unwrap();

    assert_eq!(configuration.name, s("sample-app tests"));
    assert_eq!(configuration.output_dir.as_os_str(), p(&dir, "marathon").as_os_str());
    assert_eq!(configuration.analytics_configuration, AnalyticsConfiguration::Disabled);
    assert_eq!(configuration.pooling_strategy, PoolingStrategyConfiguration::Omni);
    assert_eq!(configuration.sharding_strategy, ShardingStrategyConfiguration::Parallel);
    assert_eq!(configuration.sorting_strategy, SortingStrategyConfiguration::NoSorting);
    assert_eq!(configuration.batching_strategy, BatchingStrategyConfiguration::Isolate);
    assert_eq!(configuration.flakiness_strategy, FlakinessStrategyConfiguration::Ignore);
    assert_eq!(configuration.retry_strategy, RetryStrategyConfiguration::NoRetry);
    assert!(configuration.filtering_configuration.allowlist.is_empty());
    assert!(configuration.filtering_configuration.blocklist.is_empty());
    assert!(configuration.include_serial_regexes.is_empty());
    assert!(configuration.exclude_serial_regexes.is_empty());
    assert!(!configuration.ignore_failures);
    assert!(!configuration.is_code_coverage_enabled);
    assert_eq!(configuration.test_batch_timeout_millis, 1_800_000);
    assert_eq!(configuration.test_output_timeout_millis, 300_000);
    assert!(configuration.debug);
    assert_eq!(configuration.screen_recording_policy, ScreenRecordingPolicy::OnFailure);
    assert_eq!(
        configuration.vendor_configuration,
        VendorConfiguration::Android(android(
            PathBuf::from("/local/android"),
            Some(p(&dir, APP)),
            Some(p(&dir, TEST_APP)),
        ))
    );
}

#[test]
fn on_configuration_without_an_explicit_xctestrun_path_should_throw_an_exception() {
    // The pre-bundle iOS keys are unknown properties: a binding failure,
    // wrapped by the factory.
    let error = parse_sample("sample_5.yaml", None).unwrap_err();
    assert_eq!(load::class(&error), "configuration rejected");
}

#[test]
fn on_configuration_without_android_sdk_value_should_use_value_provided_by_environment() {
    let dir = mock_marathonfile_dir();
    let configuration = parse_sample("sample_6.yaml", Some("/android/home")).unwrap();
    let mut expected = android(
        PathBuf::from("/android/home"),
        Some(p(&dir, APP)),
        Some(p(&dir, TEST_APP)),
    );
    expected.serial_strategy = SerialStrategy::Hostname;
    assert_eq!(
        configuration.vendor_configuration,
        VendorConfiguration::Android(expected)
    );
}

#[test]
fn on_configuration_without_android_sdk_value_should_throw_an_exception_when_android_home_is_not_set() {
    let error = parse_sample("sample_7.yaml", None).unwrap_err();
    assert_eq!(load::class(&error), "configuration rejected");
    assert_eq!(load::message(&error), "No android SDK path specified");
}

#[test]
fn on_configuration_with_allowlist_but_no_blocklist_should_initialize_an_empty_blocklist() {
    let configuration = parse_sample("sample_8.yaml", None).unwrap();
    assert_eq!(
        configuration.filtering_configuration.allowlist,
        vec![TestFilterConfiguration::SimpleClassname(values_filter(
            Some(".*"),
            None,
            None
        ))]
    );
    assert!(configuration.filtering_configuration.blocklist.is_empty());
}

#[test]
fn on_configuration_with_blocklist_but_no_allowlist_should_initialize_an_empty_allowlist() {
    let configuration = parse_sample("sample_9.yaml", None).unwrap();
    assert!(configuration.filtering_configuration.allowlist.is_empty());
    assert_eq!(
        configuration.filtering_configuration.blocklist,
        vec![TestFilterConfiguration::SimpleClassname(values_filter(
            Some(".*"),
            None,
            None
        ))]
    );
}

#[test]
fn on_configuration_time_limits_specified_as_duration_should_be_used_as_relative_values() {
    let configuration = parse_sample("sample_10.yaml", None).unwrap();
    let now = load::reference_now();
    let SortingStrategyConfiguration::ExecutionTime { time_limit, .. } = configuration.sorting_strategy else {
        panic!("expected execution-time sorting");
    };
    assert_eq!(Some(time_limit), now.checked_add(parse_duration("-PT1H").unwrap()));
    let FlakinessStrategyConfiguration::Probability { time_limit, .. } = configuration.flakiness_strategy else {
        panic!("expected probability flakiness");
    };
    assert_eq!(Some(time_limit), now.checked_add(parse_duration("-P30D").unwrap()));
}

#[test]
fn on_configuration_with_timeout_configuration_in_android() {
    let configuration = parse_sample("sample_11.yaml", None).unwrap();
    let VendorConfiguration::Android(android) = configuration.vendor_configuration else {
        panic!("expected Android");
    };
    // Kotlin's TimeoutConfiguration(...) leaves pushFolder, portForward and
    // boot at their constructor defaults; portForward's is the given shell.
    assert_eq!(
        android.timeout_configuration,
        AndroidTimeoutConfiguration {
            shell: secs(30),
            list_files: secs(60),
            push_file: secs(3600),
            push_folder: secs(60),
            pull_file: secs(86_400),
            uninstall: secs(1),
            install: parse_duration("P1DT12H30M5S").unwrap(),
            screenrecorder: secs(3600),
            screencapturer: secs(1),
            socket_idle_timeout: secs(45),
            port_forward: secs(30),
            boot: secs(30),
        }
    );
}

#[test]
fn on_configuration_with_multi_module_testing_in_android() {
    let dir = mock_marathonfile_dir();
    let configuration = parse_sample("sample_12.yaml", None).unwrap();
    let VendorConfiguration::Android(android) = configuration.vendor_configuration else {
        panic!("expected Android");
    };
    let bundle = |name: &str| AndroidTestBundleConfiguration {
        application: Some(p(
            &dir,
            &format!("kotlin-buildscript/build/outputs/apk/debug/{name}-app-debug.apk"),
        )),
        test_application: p(
            &dir,
            &format!("kotlin-buildscript/build/outputs/apk/androidTest/debug/{name}-app-debug-androidTest.apk"),
        ),
        extra_applications: None,
        split_apks: Some(vec![p(
            &dir,
            &format!("kotlin-buildscript/build/outputs/apk/androidTest/debug/{name}-app-split-debug.apk"),
        )]),
    };
    assert_eq!(android.outputs, Some(vec![bundle("first"), bundle("second")]));
}

/// `ConfigurationFactory(file.parentFile, …)`: the factory's own defaults,
/// tracking flags as given (`None` = the CLI has no opinion).
fn parse_tracking(name: &str, cli: bool) -> Configuration {
    let file = load::fixture_config().canonicalize().unwrap().join(name);
    let mut context = load::context(&file, file.parent().unwrap(), None);
    context.analytics_tracking = cli;
    context.bugsnag_reporting = cli;
    load::parse(&file, &context).unwrap()
}

#[test]
fn on_configuration_with_tracking_enabled_by_default() {
    let configuration = parse_tracking("sample_1.yaml", true);
    assert!(configuration.analytics_tracking);
    assert!(configuration.bugsnag_reporting);
}

#[test]
fn on_configuration_with_tracking_disabled_in_cli() {
    let configuration = parse_tracking("sample_1.yaml", false);
    assert!(!configuration.analytics_tracking);
    assert!(!configuration.bugsnag_reporting);
}

#[test]
fn on_configuration_with_tracking_disabled_in_config_file() {
    let configuration = parse_tracking("sample_1_no_tracking.yaml", true);
    assert!(!configuration.analytics_tracking);
    assert!(!configuration.bugsnag_reporting);
}

// ---- IosConfigurationFactoryTest ----------------------------------------------

/// A temp copy of the fixtures with `marathonfileDir` = its `ios` directory.
fn parse_ios(name: &str) -> (tempfile::TempDir, PathBuf, IosConfiguration) {
    let (tmp, root) = load::fixture_copy().unwrap();
    let dir = root.join("ios");
    let file = dir.join(name);
    let configuration = load::parse(&file, &load::context(&file, &dir, None)).unwrap();
    let VendorConfiguration::Ios(ios) = configuration.vendor_configuration else {
        panic!("expected iOS");
    };
    (tmp, dir, ios)
}

/// `IOSConfiguration()`'s constructor defaults.
fn ios_defaults() -> IosConfiguration {
    IosConfiguration {
        bundle: None,
        device_provider: DeviceProvider::Static { path: None },
        devices_file: None,
        ssh: SshConfiguration {
            authentication: None,
            known_hosts_path: None,
            keep_alive_interval: secs(15),
            keep_alive_max_count: 4,
            debug: false,
            share_worker_connection: false,
        },
        worker_connection: WorkerConnectionConfiguration {
            attempts: 3,
            backoff: secs(10),
            retry_interval: secs(60),
            late_join: true,
            late_join_cutoff: secs(600),
        },
        xcresult: XcresultConfiguration {
            pull: None,
            pulling_policy: PullingPolicy::Always,
            remote_clean: true,
            attachments: AttachmentsConfiguration {
                system_attachment_lifetime: Lifetime::DeleteOnSuccess,
                user_attachment_lifetime: Lifetime::KeepAlways,
            },
            preferred_screen_capture_format: None,
            queue: XcresultQueueConfiguration {
                capacity: 8,
                concurrency: 4,
                worker_capacity: None,
            },
        },
        screen_record_configuration: IosScreenRecordConfiguration {
            preferable_recorder_type: None,
            video_configuration: IosVideoConfiguration {
                enabled: true,
                codec: Codec::H264,
                display: Display::Internal,
                mask: Mask::Black,
                transcoding: TranscodingConfiguration {
                    enabled: false,
                    binary: s("/opt/homebrew/bin/ffmpeg"),
                    size: TranscodingSize::Hd720,
                },
            },
            screenshot_configuration: IosScreenshotConfiguration {
                enabled: true,
                screenshot_type: ScreenshotType::Jpeg,
                display: Display::Internal,
                mask: Mask::Black,
                delay: millis(500),
                width: 720,
                height: 1280,
            },
        },
        xctestrun_env: XctestrunEnvConfiguration {
            app_envs: Vec::new(),
            test_envs: Vec::new(),
        },
        lifecycle_configuration: LifecycleConfiguration {
            shutdown_unused: true,
            on_prepare: Vec::new(),
            on_dispose: Vec::new(),
        },
        permissions: IosPermissionsConfiguration {
            grant: Vec::new(),
            lifecycle: GrantLifecycle::BeforeTestRun,
        },
        media_files: None,
        timeout_configuration: AppleTimeoutConfiguration {
            shell: secs(30),
            shell_idle: secs(30),
            reachability: secs(5),
            screenshot: secs(10),
            video: secs(300),
            erase: secs(30),
            shutdown: secs(30),
            delete: secs(30),
            create: secs(30),
            boot: secs(30),
            install: secs(30),
            uninstall: secs(30),
            import_media: secs(30),
            test_destination: secs(30),
            xcresulttool: secs(120),
            boot_wait: secs(180),
        },
        threading_configuration: AppleThreadingConfiguration {
            device_provider_threads: 8,
            device_threads: 2,
            provisioning_concurrency: 2,
        },
        hide_runner_output: false,
        compact_output: false,
        rsync: RsyncConfiguration {
            remote_path: s("/usr/bin/rsync"),
        },
        xcodebuild_test_args: Vec::new(),
        data_container_clear: false,
        test_parser_configuration: AppleTestParserConfiguration::Nm {
            test_class_regexes: vec![TestClassRegex::Default],
        },
        device_log: false,
        signing: SigningConfiguration {
            test_runner_provisioning_profile: None,
        },
        skip_copying_derived_data: false,
        max_batches_without_progress: 0,
    }
}

#[test]
fn on_config_with_ios_vendor_configuration_should_initialize_a_specific_vendor_configuration() {
    let (_tmp, dir, ios) = parse_ios("sample_2.yaml");
    let mut expected = ios_defaults();
    expected.bundle = Some(AppleTestBundleConfiguration::unresolved(
        None,
        None,
        None,
        Some(p(&dir, "derivedDataDir")),
        None,
    ));
    expected.ssh = SshConfiguration {
        authentication: Some(SshAuthentication::PublicKey {
            username: s("testuser"),
            key: canonical_file("/home/testuser/.ssh/id_rsa"),
        }),
        known_hosts_path: Some(p(&dir, "known_hosts")),
        keep_alive_interval: secs(300),
        keep_alive_max_count: 3,
        debug: true,
        share_worker_connection: false,
    };
    expected.rsync.remote_path = s("/usr/local/bin/rsync");
    expected.xcresult.pulling_policy = PullingPolicy::OnFailure;
    expected.xcresult.remote_clean = false;
    expected.hide_runner_output = true;
    expected.compact_output = true;
    expected.devices_file = Some(p(&dir, "Testdevices"));
    assert_eq!(ios, expected);
}

#[test]
fn on_configuration_without_an_explicit_remote_rsync_path_should_initialize_a_default_one() {
    let (_tmp, _dir, ios) = parse_ios("sample_3.yaml");
    assert_eq!(ios.rsync.remote_path, s("/usr/bin/rsync"));
    assert_eq!(ios.bundle.and_then(|b| b.test_type), Some(TestType::Xcuitest));
}

#[test]
fn on_configuration_with_xctest_type_specified() {
    let (_tmp, _dir, ios) = parse_ios("sample_3.yaml");
    assert_eq!(ios.bundle.and_then(|b| b.test_type), Some(TestType::Xcuitest));
}

#[test]
fn on_configuration_with_import_media_files() {
    let (_tmp, dir, ios) = parse_ios("sample_4.yaml");
    let media = ios.media_files.unwrap();
    assert_eq!(media.len(), 2);
    assert!(media.contains(&p(&dir, "media/empty.jpg")));
    assert!(media.contains(&p(&dir, "media/empty.mp4")));
}

#[test]
fn on_configuration_with_timeout_configuration_in_ios() {
    let (_tmp, _dir, ios) = parse_ios("sample_5.yaml");
    assert_eq!(
        ios.timeout_configuration,
        AppleTimeoutConfiguration {
            shell: secs(30),
            shell_idle: secs(31),
            reachability: secs(60),
            screenshot: secs(120),
            video: secs(3600),
            erase: secs(7200),
            shutdown: secs(86_400),
            delete: secs(3),
            create: parse_duration("P1DT12H30M5S").unwrap(),
            boot: secs(80),
            install: secs(10_800),
            uninstall: secs(62),
            import_media: secs(6),
            test_destination: secs(34),
            xcresulttool: secs(300),
            // Not set in the Kotlin test either: max(boot, 180 s).
            boot_wait: secs(180),
        }
    );
}

#[test]
fn should_set_pulling_policy_as_always_when_pull_is_true() {
    let (_tmp, _dir, ios) = parse_ios("sample_6.yaml");
    assert_eq!(ios.xcresult.pulling_policy, PullingPolicy::Always);
    assert!(!ios.xcresult.remote_clean);
}

#[test]
fn should_set_pulling_policy_as_never_when_pull_is_false() {
    let (_tmp, _dir, ios) = parse_ios("sample_7.yaml");
    assert_eq!(ios.xcresult.pulling_policy, PullingPolicy::Never);
    assert!(!ios.xcresult.remote_clean);
}

#[test]
fn on_configuration_without_code_coverage_should_ignore_a_missing_xctestrun() {
    let (_tmp, _dir, ios) = parse_ios("sample_8.yaml");
    assert_eq!(ios.bundle.and_then(|b| b.xctestrun), None);
}

#[test]
fn on_configuration_with_code_coverage_should_reject_a_missing_xctestrun() {
    let (_tmp, root) = load::fixture_copy().unwrap();
    let dir = root.join("ios");
    let file = dir.join("sample_9.yaml");
    let error = load::parse(&file, &load::context(&file, &dir, None)).unwrap_err();
    assert_eq!(error.kind, marathon_config::CONFIGURATION_EXCEPTION_CLASS);
    assert!(error.message.as_ref().unwrap().to_owned().contains("is not a file"));
}
