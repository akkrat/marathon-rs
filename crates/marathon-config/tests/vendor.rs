//! Port of configuration/src/test/kotlin/com/malinskiy/marathon/config/environment/SystemEnvironmentReaderTest.kt,
//! config/vendor/AndroidConfigurationTest.kt, config/vendor/apple/SshConfigurationTest.kt,
//! config/vendor/apple/TimeoutConfigurationTest.kt,
//! config/vendor/IOSConfigurationTest.kt (the worker-connection back-off row
//! only; the rest is WP 2.3b) and
//! vendor/vendor-android/src/test/kotlin/com/malinskiy/marathon/android/configuration/AllureConfigurationTest.kt.
//!
//! Whole-file `ConfigurationFactory.parse` rows are ported at section level
//! (WP 2.3a decision 1): the fixture's `vendorConfiguration` subtree binds
//! through `bind_vendor_configuration` and this WP's post-processing pieces
//! run with a stubbed environment. The assembled forms belong to WP 2.4.

use jcompat::jtime::{Duration, instant_of};
use marathon_config::{
    AndroidConfiguration, AppleTimeoutParameters, SshAuthentication, SshConfiguration, TestClassRegex,
    VendorConfiguration, WorkerConnectionConfiguration, XcresultQueueConfiguration,
    bind_vendor::{
        bind_allure_configuration, bind_android_timeout_configuration, bind_ssh_configuration,
        bind_vendor_configuration,
    },
    binder::{Binder, Result, Value},
    binding::{BindingContext, Profile},
    node,
    vendor::{apply_xcresult_pull, fill_android_sdk, post_process_ios, read_environment},
};
use std::path::PathBuf;

fn context(dir: &str, environment: Vec<(String, String)>) -> BindingContext {
    BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: PathBuf::from("/Marathonfile"),
        marathonfile_dir: PathBuf::from(dir),
        process_dir: PathBuf::from("/"),
        environment,
        now: instant_of(0, 0).unwrap(),
        analytics_tracking: false,
        bugsnag_reporting: false,
    }
}
fn bind<T>(text: &str, ctx: &BindingContext, schema: impl FnOnce(Value<'_>) -> Result<T>) -> Result<Option<T>> {
    let stream = node::load(text);
    let binder = Binder::new(ctx, &stream);
    binder.first(&stream, schema)
}
fn bind_vendor(text: &str, ctx: &BindingContext) -> VendorConfiguration {
    bind(text, ctx, bind_vendor_configuration)
        .unwrap()
        .flatten()
        .expect("vendor binds")
}
fn path(text: &str) -> PathBuf {
    PathBuf::from(text)
}
fn secs(seconds: i64) -> Duration {
    Duration::of_seconds(seconds)
}

// ---- SystemEnvironmentReaderTest -----------------------------------------------

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect()
}

#[test]
fn environment_configuration_should_point_to_android_home() {
    let config = read_environment(&env(&[("ANDROID_HOME", "/android")]));
    assert_eq!(
        config.android_sdk.as_deref().map(std::path::Path::as_os_str),
        Some(std::ffi::OsStr::new("/android"))
    );
}

#[test]
fn environment_configuration_should_point_to_android_sdk_root() {
    let config = read_environment(&env(&[("ANDROID_SDK_ROOT", "/android")]));
    assert_eq!(
        config.android_sdk.as_deref().map(std::path::Path::as_os_str),
        Some(std::ffi::OsStr::new("/android"))
    );
}

#[test]
fn environment_configuration_should_prefer_android_home_over_android_sdk_root() {
    let config = read_environment(&env(&[
        ("ANDROID_HOME", "/android"),
        ("ANDROID_SDK_ROOT", "/android_sdk"),
    ]));
    assert_eq!(
        config.android_sdk.as_deref().map(std::path::Path::as_os_str),
        Some(std::ffi::OsStr::new("/android"))
    );
}

// ---- AndroidConfigurationTest (section level) -----------------------------------
// Kotlin uses marathonfileDir = /some/folder/with/marathonfile/ and a mocked
// environment reader returning a temp file; here the environment carries
// ANDROID_HOME=/env-sdk and `fill_android_sdk` plays the factory's role.

const ANDROID_DIR: &str = "/some/folder/with/marathonfile";

fn android_from_fixture(name: &str, environment: Vec<(String, String)>) -> AndroidConfiguration {
    let text = std::fs::read_to_string(format!(
        "{}/../../fixtures/configuration/fixture/config/android/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let ctx = context(ANDROID_DIR, environment);
    let vendor = bind(&text, &ctx, |v: Value<'_>| -> Result<Option<VendorConfiguration>> {
        let child = v.get("vendorConfiguration")?.expect("fixture has vendor");
        bind_vendor_configuration(child)
    })
    .unwrap()
    .flatten()
    .expect("vendor binds");
    let VendorConfiguration::Android(mut android) = vendor else {
        panic!("expected Android")
    };
    fill_android_sdk(&mut android, &ctx).unwrap();
    android
}
fn android_with_env(name: &str) -> AndroidConfiguration {
    android_from_fixture(name, env(&[("ANDROID_HOME", "/env-sdk")]))
}

#[test]
fn if_android_sdk_is_null_should_throw_exception_if_env_android_sdk_also_is_null() {
    let text = std::fs::read_to_string(format!(
        "{}/../../fixtures/configuration/fixture/config/android/sample_1.yaml",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let ctx = context(ANDROID_DIR, Vec::new());
    let vendor = bind(&text, &ctx, |v: Value<'_>| -> Result<Option<VendorConfiguration>> {
        let child = v.get("vendorConfiguration")?.expect("fixture has vendor");
        bind_vendor_configuration(child)
    })
    .unwrap()
    .flatten()
    .expect("vendor binds");
    let VendorConfiguration::Android(mut android) = vendor else {
        panic!("expected Android")
    };
    let error = fill_android_sdk(&mut android, &ctx).unwrap_err();
    assert_eq!(
        error.kind.legacy_class().to_owned(),
        "com.malinskiy.marathon.config.exceptions.ConfigurationException"
    );
    assert_eq!(
        error.message.as_ref().map(|m| m.to_owned()),
        Some("No android SDK path specified".to_owned())
    );
}

#[test]
fn if_android_sdk_is_null_should_use_env_android_sdk_if_it_is_not_null() {
    let android = android_with_env("sample_1.yaml");
    assert_eq!(
        android.android_sdk.as_deref().map(std::path::Path::as_os_str),
        Some(std::ffi::OsStr::new("/env-sdk"))
    );
}

#[test]
fn if_android_sdk_is_not_null_should_use_android_sdk_instead_of_env_if_both_exists() {
    let android = android_with_env("sample_2.yaml");
    assert_eq!(
        android.android_sdk.as_deref().map(std::path::Path::as_os_str),
        Some(std::ffi::OsStr::new("/opt/android-sdk"))
    );
}

#[test]
fn if_android_sdk_is_not_null_test_application_output_should_be_null_by_default() {
    // Kotlin asserts applicationOutput despite the test name.
    let android = android_with_env("sample_1.yaml");
    assert_eq!(android.application_output, None);
}

#[test]
fn if_android_sdk_is_not_null_test_application_output_should_be_null_if_provided() {
    // Kotlin asserts the provided applicationOutput despite the test name.
    let android = android_with_env("sample_3.yaml");
    assert_eq!(
        android.application_output.as_deref().map(std::path::Path::as_os_str),
        Some(std::ffi::OsStr::new("/some/folder/with/marathonfile/debug.apk"))
    );
}

#[test]
fn if_android_sdk_is_not_null_test_application_apk_should_be_equal() {
    let android = android_with_env("sample_2.yaml");
    assert_eq!(
        android
            .test_application_output
            .as_deref()
            .map(std::path::Path::as_os_str),
        Some(std::ffi::OsStr::new("/some/folder/with/marathonfile/foo/bar"))
    );
}

#[test]
fn if_android_sdk_is_not_null_auto_grant_permissions_should_be_false_by_default() {
    let android = android_with_env("sample_1.yaml");
    assert!(!android.auto_grant_permission);
}

#[test]
fn if_android_sdk_is_not_null_auto_grant_permissions_should_be_equal() {
    let android = android_with_env("sample_1.yaml");
    assert!(!android.auto_grant_permission);
}

#[test]
fn if_android_sdk_is_not_null_adb_init_timeout_millis_should_be_30_000_by_default() {
    let android = android_with_env("sample_1.yaml");
    assert_eq!(android.adb_init_timeout_millis, 30_000);
}

#[test]
fn if_android_sdk_is_not_null_adb_init_timeout_millis_should_be_equal() {
    let android = android_with_env("sample_4.yaml");
    assert_eq!(android.adb_init_timeout_millis, 500_000);
}

#[test]
fn if_android_sdk_is_not_null_install_options_should_be_empty_string_by_default() {
    let android = android_with_env("sample_1.yaml");
    assert_eq!(android.install_options.to_owned(), "");
}

#[test]
fn if_android_sdk_is_not_null_install_options_should_be_equal_if_provided() {
    let android = android_with_env("sample_5.yaml");
    assert_eq!(android.install_options.to_owned(), "-d");
}

#[test]
fn if_android_sdk_is_not_null_extra_applications_output_should_be_equal_if_provided() {
    let android = android_with_env("sample_6.yaml");
    assert_eq!(
        android
            .extra_applications_output
            .as_ref()
            .map(|paths| paths.iter().map(|p| p.as_os_str()).collect::<Vec<_>>()),
        Some(vec![std::ffi::OsStr::new("/some/folder/with/marathonfile/foo/bar")])
    );
}

#[test]
fn if_android_sdk_is_not_null_extra_applications_output_should_be_null_by_default() {
    let android = android_with_env("sample_1.yaml");
    assert_eq!(android.extra_applications_output, None);
}

// ---- SshConfigurationTest ---------------------------------------------------------

fn ssh_with(count: i32) -> SshConfiguration {
    SshConfiguration {
        authentication: None,
        known_hosts_path: None,
        keep_alive_interval: secs(15),
        keep_alive_max_count: count,
        debug: false,
        share_worker_connection: false,
    }
}

#[test]
fn keep_alive_max_count_below_one_is_rejected() {
    let error = ssh_with(0).validate().unwrap_err();
    assert_eq!(
        error.fqcn(),
        "com.malinskiy.marathon.config.exceptions.ConfigurationException"
    );
    assert_eq!(error.message, "ssh.keepAliveMaxCount should be at least 1, got 0");
}

#[test]
fn keep_alive_max_count_of_one_is_accepted() {
    ssh_with(1).validate().unwrap();
}

#[test]
fn by_default_a_worker_that_stopped_responding_is_detected_after_a_minute() {
    let ctx = context("/", Vec::new());
    let ssh = bind("debug: false", &ctx, bind_ssh_configuration)
        .unwrap()
        .flatten()
        .expect("ssh binds");
    assert_eq!(ssh.keep_alive_interval, secs(15));
    assert_eq!(ssh.keep_alive_max_count, 4);
}

#[test]
fn configured_keep_alive_is_kept() {
    let ctx = context("/", Vec::new());
    let ssh = bind(
        "keepAliveInterval: PT60S\nkeepAliveMaxCount: 3",
        &ctx,
        bind_ssh_configuration,
    )
    .unwrap()
    .flatten()
    .expect("ssh binds");
    assert_eq!(ssh.keep_alive_interval, secs(60));
    assert_eq!(ssh.keep_alive_max_count, 3);
}

// ---- AllureConfigurationTest -------------------------------------------------------

#[test]
fn test_default() {
    let ctx = context("/", Vec::new());
    let allure = bind("{}", &ctx, bind_allure_configuration)
        .unwrap()
        .flatten()
        .expect("allure binds");
    assert_eq!(allure.relative_results_directory.to_owned(), "/files/allure-results");
}

// ---- TimeoutConfigurationTest (apple, section level) --------------------------------
// Parameterized over iOS [1] and macOS [2]; the serialization round-trip rows
// move to WP 2.4.

fn timeouts(vendor_type: &str, lines: &[&str]) -> marathon_config::AppleTimeoutConfiguration {
    let section = if lines.is_empty() {
        String::new()
    } else {
        let mut section = "timeoutConfiguration:\n".to_owned();
        for line in lines {
            section.push_str(&format!("  {line}\n"));
        }
        section
    };
    let text = format!("type: {vendor_type}\n{section}");
    let ctx = context("/", Vec::new());
    match bind_vendor(&text, &ctx) {
        VendorConfiguration::Ios(config) => config.timeout_configuration,
        VendorConfiguration::Macos(config) => config.timeout_configuration,
        _ => panic!("expected Apple vendor"),
    }
}

fn check_without_timeout_xcresulttool(vendor_type: &str) {
    let timeouts = timeouts(vendor_type, &[]);
    assert_eq!(timeouts.shell, secs(30));
    assert_eq!(timeouts.xcresulttool, secs(120));
}
#[test]
fn without_a_timeout_configuration_xcresulttool_gets_120_seconds_1() {
    check_without_timeout_xcresulttool("iOS");
}
#[test]
fn without_a_timeout_configuration_xcresulttool_gets_120_seconds_2() {
    check_without_timeout_xcresulttool("macOS");
}

fn check_raised_shell(vendor_type: &str) {
    let timeouts = timeouts(vendor_type, &["shell: PT5M"]);
    assert_eq!(timeouts.xcresulttool, secs(300));
    for field in [
        timeouts.erase,
        timeouts.shutdown,
        timeouts.delete,
        timeouts.boot,
        timeouts.install,
        timeouts.uninstall,
        timeouts.import_media,
    ] {
        assert_eq!(field, secs(300));
    }
}
#[test]
fn a_raised_shell_timeout_applies_to_xcresulttool_and_to_the_timeouts_that_default_to_shell_1() {
    check_raised_shell("iOS");
}
#[test]
fn a_raised_shell_timeout_applies_to_xcresulttool_and_to_the_timeouts_that_default_to_shell_2() {
    check_raised_shell("macOS");
}

fn check_low_shell(vendor_type: &str) {
    assert_eq!(timeouts(vendor_type, &["shell: PT10S"]).xcresulttool, secs(120));
}
#[test]
fn a_shell_timeout_below_120_seconds_leaves_xcresulttool_at_120_seconds_1() {
    check_low_shell("iOS");
}
#[test]
fn a_shell_timeout_below_120_seconds_leaves_xcresulttool_at_120_seconds_2() {
    check_low_shell("macOS");
}

fn check_explicit_xcresulttool(vendor_type: &str) {
    assert_eq!(
        timeouts(vendor_type, &["shell: PT5M", "xcresulttool: PT1M"]).xcresulttool,
        secs(60)
    );
    assert_eq!(
        timeouts(vendor_type, &["shell: PT10S", "xcresulttool: PT20S"]).xcresulttool,
        secs(20)
    );
    assert_eq!(timeouts(vendor_type, &["xcresulttool: PT10M"]).xcresulttool, secs(600));
}
#[test]
fn an_explicit_xcresulttool_timeout_wins_over_shell_and_the_120_second_default_1() {
    check_explicit_xcresulttool("iOS");
}
#[test]
fn an_explicit_xcresulttool_timeout_wins_over_shell_and_the_120_second_default_2() {
    check_explicit_xcresulttool("macOS");
}

fn check_boot_default(vendor_type: &str) {
    let timeouts = timeouts(vendor_type, &[]);
    assert_eq!(timeouts.boot, secs(30));
    assert_eq!(timeouts.boot_wait, secs(180));
}
#[test]
fn without_a_timeout_configuration_a_simulator_gets_180_seconds_to_boot_1() {
    check_boot_default("iOS");
}
#[test]
fn without_a_timeout_configuration_a_simulator_gets_180_seconds_to_boot_2() {
    check_boot_default("macOS");
}

fn check_boot_wait(vendor_type: &str) {
    assert_eq!(timeouts(vendor_type, &["boot: PT5M"]).boot_wait, secs(300));
    assert_eq!(timeouts(vendor_type, &["shell: PT4M"]).boot_wait, secs(240));
    assert_eq!(timeouts(vendor_type, &["boot: PT10S"]).boot_wait, secs(180));
    assert_eq!(
        timeouts(vendor_type, &["boot: PT5M", "bootWait: PT1M"]).boot_wait,
        secs(60)
    );
    assert_eq!(timeouts(vendor_type, &["bootWait: PT10M"]).boot_wait, secs(600));
}
#[test]
fn a_raised_boot_or_shell_timeout_applies_to_boot_wait_an_explicit_one_wins_1() {
    check_boot_wait("iOS");
}
#[test]
fn a_raised_boot_or_shell_timeout_applies_to_boot_wait_an_explicit_one_wins_2() {
    check_boot_wait("macOS");
}

#[test]
fn the_constructor_applies_the_same_default() {
    use marathon_config::apple_timeout_configuration;
    assert_eq!(
        apple_timeout_configuration(AppleTimeoutParameters::empty()).boot_wait,
        secs(180)
    );
    assert_eq!(
        apple_timeout_configuration(AppleTimeoutParameters {
            boot: Some(secs(300)),
            ..AppleTimeoutParameters::empty()
        })
        .boot_wait,
        secs(300)
    );
    assert_eq!(
        apple_timeout_configuration(AppleTimeoutParameters::empty()).xcresulttool,
        secs(120)
    );
    assert_eq!(
        apple_timeout_configuration(AppleTimeoutParameters {
            shell: Some(secs(300)),
            ..AppleTimeoutParameters::empty()
        })
        .xcresulttool,
        secs(300)
    );
    assert_eq!(
        apple_timeout_configuration(AppleTimeoutParameters {
            shell: Some(secs(300)),
            xcresulttool: Some(secs(60)),
            ..AppleTimeoutParameters::empty()
        })
        .xcresulttool,
        secs(60)
    );
}

// ---- WP 2.3a schema unit tests (not Kotlin parity rows) ------------------------------

#[test]
fn vendor_dispatch_names() {
    let ctx = context("/", Vec::new());
    for name in ["iOS", "watchOS", "tvOS", "visionOS"] {
        assert!(
            matches!(bind_vendor(&format!("type: {name}"), &ctx), VendorConfiguration::Ios(_)),
            "{name}"
        );
    }
    assert!(matches!(
        bind_vendor("type: Android", &ctx),
        VendorConfiguration::Android(_)
    ));
    assert!(matches!(
        bind_vendor("type: macOS", &ctx),
        VendorConfiguration::Macos(_)
    ));
    assert!(matches!(bind_vendor("type: stub", &ctx), VendorConfiguration::Stub));
    assert!(matches!(bind_vendor("type: empty", &ctx), VendorConfiguration::Empty));
    // Late `type` is buffered by the polymorphic binder.
    assert!(matches!(
        bind_vendor("devices: x\ntype: macOS", &ctx),
        VendorConfiguration::Macos(_)
    ));
    // Unknown and wrong-case ids are rejected.
    assert!(bind("type: android", &ctx, bind_vendor_configuration).is_err());
    assert!(bind("type: iPadOS", &ctx, bind_vendor_configuration).is_err());
}

#[test]
fn derived_android_timeouts_follow_bound_shell_in_any_order() {
    let ctx = context("/", Vec::new());
    let android_timeouts = bind("listFiles: PT1S\nshell: PT9S", &ctx, bind_android_timeout_configuration)
        .unwrap()
        .flatten()
        .expect("timeouts bind");
    assert_eq!(android_timeouts.shell, secs(9));
    assert_eq!(android_timeouts.list_files, secs(1));
    assert_eq!(android_timeouts.uninstall, secs(9));
    assert_eq!(android_timeouts.port_forward, secs(9));
    // Apple: shutdown alone moves delete but not erase; explicit values win
    // even before shell in the document.
    let apple = timeouts("iOS", &["shutdown: PT5M"]);
    assert_eq!(apple.delete, secs(300));
    assert_eq!(apple.erase, secs(30));
    let apple = timeouts("iOS", &["xcresulttool: PT20S", "shell: PT5M"]);
    assert_eq!(apple.xcresulttool, secs(20));
}

#[test]
fn creator_enums_fold_case_and_explicit_nulls() {
    let ctx = context("/", Vec::new());
    let VendorConfiguration::Android(android) = bind_vendor("type: Android\nserialStrategy: boot_property", &ctx)
    else {
        panic!("expected Android")
    };
    assert_eq!(android.serial_strategy, marathon_config::SerialStrategy::BootProperty);
    // Primitive with a default takes zero on explicit null (2.1 rules).
    let VendorConfiguration::Android(android) = bind_vendor("type: Android\nadbInitTimeoutMillis: null", &ctx) else {
        panic!("expected Android")
    };
    assert_eq!(android.adb_init_timeout_millis, 0);
    assert!(bind("type: Android\nserialStrategy: nope", &ctx, bind_vendor_configuration).is_err());
}

// Kotlin sets are `HashSet`s whose iteration order is unspecified; keeping
// first-seen order here is the approved P7 simplification, so the ordered
// assertions below pin the Rust representation, not a Kotlin order.
#[test]
fn sets_collapse_duplicates_and_enum_values_diverge_from_yaml_spellings() {
    let ctx = context("/", Vec::new());
    let VendorConfiguration::Ios(ios) = bind_vendor(
        "type: iOS\npermissions:\n  grant: [calendar, user-tracking, calendar]\nlifecycle:\n  onPrepare: [ERASE, ERASE, TERMINATE]",
        &ctx,
    ) else {
        panic!("expected iOS")
    };
    assert_eq!(
        ios.permissions.grant,
        vec![
            marathon_config::IosPermission::Calendar,
            marathon_config::IosPermission::UserTracking
        ]
    );
    assert_eq!(ios.permissions.grant[1].value(), "kTCCServiceUserTracking");
    assert_eq!(
        ios.lifecycle_configuration.on_prepare,
        vec![
            marathon_config::LifecycleAction::Erase,
            marathon_config::LifecycleAction::Terminate
        ]
    );
}

#[test]
fn default_test_class_regex_survives_and_explicit_lookahead_is_rejected() {
    let ctx = context("/", Vec::new());
    let VendorConfiguration::Ios(ios) = bind_vendor("type: iOS", &ctx) else {
        panic!("expected iOS")
    };
    let marathon_config::AppleTestParserConfiguration::Nm { test_class_regexes } = &ios.test_parser_configuration
    else {
        panic!("expected nm parser")
    };
    assert_eq!(test_class_regexes, &vec![TestClassRegex::Default]);
    assert_eq!(test_class_regexes[0].pattern(), "^((?!Abstract).)*Test[s]*$");
    assert!(test_class_regexes[0].matches("FooTest"));
    // An explicitly supplied copy of the default pattern is D6-rejected.
    let explicit =
        "type: iOS\ntestParserConfiguration:\n  type: nm\n  testClassRegexes: [\"^((?!Abstract).)*Test[s]*$\"]";
    assert!(bind(explicit, &ctx, bind_vendor_configuration).is_err());
    // Pattern-based equality is consistent (0.6 D1).
    let VendorConfiguration::Ios(other) = bind_vendor("type: iOS", &ctx) else {
        panic!("expected iOS")
    };
    assert_eq!(ios.test_parser_configuration, other.test_parser_configuration);
}

#[test]
fn worker_connection_pause_after_and_queue_capacity() {
    let defaults = WorkerConnectionConfiguration {
        attempts: 3,
        backoff: secs(10),
        retry_interval: secs(60),
        late_join: true,
        late_join_cutoff: secs(600),
    };
    let pauses: Vec<Option<Duration>> = (1..=5).map(|a| defaults.pause_after(a)).collect();
    assert_eq!(
        pauses,
        vec![
            Some(secs(10)),
            Some(secs(20)),
            Some(secs(60)),
            Some(secs(60)),
            Some(secs(60))
        ]
    );
    let no_late = WorkerConnectionConfiguration {
        late_join: false,
        ..defaults.clone()
    };
    assert_eq!(no_late.pause_after(3), None);
    assert_eq!(
        XcresultQueueConfiguration::default_instance().effective_worker_capacity(),
        2
    );
    assert_eq!(
        XcresultQueueConfiguration {
            capacity: 8,
            concurrency: 4,
            worker_capacity: Some(3)
        }
        .effective_worker_capacity(),
        3
    );
}

#[test]
fn validate_messages_are_verbatim() {
    let ctx = context("/", Vec::new());
    let VendorConfiguration::Ios(mut ios) = bind_vendor("type: iOS\nmaxBatchesWithoutProgress: -1", &ctx) else {
        panic!("expected iOS")
    };
    let error = ios.validate().unwrap_err();
    assert_eq!(
        error.message,
        "maxBatchesWithoutProgress should not be negative, got -1"
    );
    ios.max_batches_without_progress = 0;
    // Bundle presence is checked after ssh/worker/queue/threading pass.
    let error = ios.validate().unwrap_err();
    assert_eq!(error.message, "bundles must contain at least one valid entry");
    // Android file validation.
    let VendorConfiguration::Android(android) =
        bind_vendor("type: Android\ntestApplicationApk: \"/nope/missing.apk\"", &ctx)
    else {
        panic!("expected Android")
    };
    let error = android.validate().unwrap_err();
    assert_eq!(error.message, "/nope/missing.apk does not exist");
    assert_eq!(
        android.safe_android_sdk().unwrap_err().message,
        "No android SDK path specified"
    );
    // SSH missing key keeps FileNotFoundException (decision 8).
    let ssh = SshConfiguration {
        authentication: Some(SshAuthentication::PublicKey {
            username: "user".into(),
            key: path("/nope/key"),
        }),
        ..ssh_with(4)
    };
    let error = ssh.validate().unwrap_err();
    assert_eq!(error.fqcn(), "java.io.FileNotFoundException");
    assert_eq!(error.kind, marathon_config::ValidationKind::FileNotFound);
    assert_eq!(error.message, "Private key not found at /nope/key");
}

#[test]
fn sdk_lookup_empty_counts_as_unset_and_relative_stays_relative() {
    // P2: empty ANDROID_HOME falls through; Kotlin accepts File("").
    let config = read_environment(&env(&[("ANDROID_HOME", ""), ("ANDROID_SDK_ROOT", "rel/sdk")]));
    assert_eq!(
        config.android_sdk.as_deref().map(std::path::Path::as_os_str),
        Some(std::ffi::OsStr::new("rel/sdk"))
    );
    let config = read_environment(&env(&[("ANDROID_HOME", "")]));
    assert_eq!(config.android_sdk, None);
}

#[test]
fn post_processing_paths_and_xcresult_pull() {
    let dir = tempfile::tempdir().unwrap();
    let dir_text = dir.path().to_string_lossy().into_owned();
    let ctx = context(&dir_text, Vec::new());
    let VendorConfiguration::Ios(mut ios) = bind_vendor(
        "type: iOS\nssh:\n  knownHostsPath: \"sub/../known_hosts\"\nxcresult:\n  pull: false\n  pullingPolicy: ON_FAILURE",
        &ctx,
    ) else {
        panic!("expected iOS")
    };
    post_process_ios(&mut ios, &ctx).unwrap();
    // Missing devices defaults to <dir>/Marathondevices, not canonicalised.
    assert_eq!(
        ios.devices_file.as_deref().map(std::path::Path::as_os_str),
        Some(std::ffi::OsStr::new(&format!("{dir_text}/Marathondevices")))
    );
    // knownHostsPath goes through resolve(...).canonicalFile: /../ is gone.
    let known = ios.ssh.known_hosts_path.as_ref().expect("known hosts");
    assert_eq!(
        known.to_string_lossy().into_owned(),
        format!("{}/known_hosts", dir.path().canonicalize().unwrap().to_string_lossy())
    );
    // pull=false rewrites the policy; the bound pull stays (P6 rejected).
    assert_eq!(ios.xcresult.pulling_policy, marathon_config::PullingPolicy::Never);
    assert_eq!(ios.xcresult.pull, Some(false));
    let mut xcresult = ios.xcresult.clone();
    xcresult.pull = Some(true);
    apply_xcresult_pull(&mut xcresult);
    assert_eq!(xcresult.pulling_policy, marathon_config::PullingPolicy::Always);
}

#[test]
fn apple_accessors_project_vendor_fields() {
    let ctx = context("/", Vec::new());
    let ios = bind_vendor("type: iOS\nskipCopyingDerivedData: true", &ctx);
    assert!(ios.skip_copying_derived_data());
    assert!(ios.xctestrun_env().is_some());
    assert!(ios.xcresult_configuration().is_some());
    assert!(ios.bundle_configuration().is_none());
    let macos = bind_vendor("type: macOS", &ctx);
    assert!(!macos.skip_copying_derived_data());
    let android = bind_vendor("type: Android", &ctx);
    assert!(android.xctestrun_env().is_none());
    assert!(android.xcresult_configuration().is_none());
    assert!(!android.skip_copying_derived_data());
}

#[test]
fn debug_never_prints_secrets() {
    let auth = SshAuthentication::Password {
        username: "user".into(),
        password: "hunter2".into(),
    };
    assert!(!format!("{auth:?}").contains("hunter2"));
    let ctx = context("/", Vec::new());
    let vendor = bind_vendor(
        "type: Android\ntestAccessConfiguration:\n  consoleToken: \"hunter2\"",
        &ctx,
    );
    assert!(!format!("{vendor:?}").contains("hunter2"));
}

#[test]
fn jvm_probe_pins() {
    // Pinned by a JVM probe through a plain Jackson mapper (see findings):
    // numeric creator input is rejected, the one required File is required,
    // and TimeUnit binds by JDK ordinal.
    let ctx = context("/", Vec::new());
    assert!(bind("type: Android\nserialStrategy: 1", &ctx, bind_vendor_configuration).is_err());
    assert!(bind("type: Android\noutputs: [{}]", &ctx, bind_vendor_configuration).is_err());
    let VendorConfiguration::Android(android) = bind_vendor(
        "type: Android\nscreenRecordConfiguration:\n  videoConfiguration:\n    timeLimitUnits: 3",
        &ctx,
    ) else {
        panic!("expected Android")
    };
    assert_eq!(
        android.screen_record_configuration.video_configuration.time_limit_units,
        marathon_config::TimeUnit::Seconds
    );
}

#[test]
fn null_collection_elements_are_rejected_at_bind() {
    let ctx = context("/", Vec::new());
    assert!(bind("type: Android\nsplitApks: [null]", &ctx, bind_vendor_configuration).is_err());
    assert!(bind("type: Android\nadbServers: [null]", &ctx, bind_vendor_configuration).is_err());
}

// ---- IOSConfigurationTest (the one row carved back to WP 2.3a) ----------------

#[test]
fn worker_connection_back_off_doubles_up_to_the_retry_interval_which_late_join_keeps() {
    // Kotlin `WorkerConnectionConfiguration()` defaults.
    let defaults = WorkerConnectionConfiguration {
        attempts: 3,
        backoff: secs(10),
        retry_interval: secs(60),
        late_join: true,
        late_join_cutoff: secs(600),
    };
    let pauses: Vec<Option<Duration>> = (1..=5).map(|a| defaults.pause_after(a)).collect();
    assert_eq!(
        pauses,
        vec![
            Some(secs(10)),
            Some(secs(20)),
            Some(secs(60)),
            Some(secs(60)),
            Some(secs(60))
        ]
    );
    let longer = WorkerConnectionConfiguration {
        attempts: 6,
        ..defaults.clone()
    };
    let pauses: Vec<Option<Duration>> = (1..=6).map(|a| longer.pause_after(a)).collect();
    assert_eq!(
        pauses,
        vec![
            Some(secs(10)),
            Some(secs(20)),
            Some(secs(40)),
            Some(secs(60)),
            Some(secs(60)),
            Some(secs(60))
        ]
    );
    let without_late_join = WorkerConnectionConfiguration {
        late_join: false,
        ..defaults.clone()
    };
    let pauses: Vec<Option<Duration>> = (1..=3).map(|a| without_late_join.pause_after(a)).collect();
    assert_eq!(pauses, vec![Some(secs(10)), Some(secs(20)), None]);
    assert_eq!(
        WorkerConnectionConfiguration {
            attempts: 1,
            late_join: false,
            ..defaults.clone()
        }
        .pause_after(1),
        None
    );
    assert_eq!(
        WorkerConnectionConfiguration {
            attempts: 4,
            backoff: secs(0),
            ..defaults
        }
        .pause_after(3),
        Some(secs(0))
    );
}

#[test]
fn schema_level_validate_messages_for_worker_queue_threading_and_knownhosts() {
    let ctx = context("/", Vec::new());
    let worker_message = |yaml: &str| {
        let VendorConfiguration::Ios(ios) = bind_vendor(yaml, &ctx) else {
            panic!("expected iOS")
        };
        ios.worker_connection.validate().unwrap_err().message
    };
    assert_eq!(
        worker_message("type: iOS\nworkerConnection:\n  attempts: 0"),
        "workerConnection.attempts should be at least 1, got 0"
    );
    assert_eq!(
        worker_message("type: iOS\nworkerConnection:\n  backoff: PT-1S"),
        "workerConnection.backoff should not be negative, got PT-1S"
    );
    assert_eq!(
        worker_message("type: iOS\nworkerConnection:\n  retryInterval: PT0S"),
        "workerConnection.retryInterval should be positive, got PT0S"
    );
    assert_eq!(
        worker_message("type: iOS\nworkerConnection:\n  lateJoinCutoff: PT-1M"),
        "workerConnection.lateJoinCutoff should not be negative, got PT-1M"
    );

    let queue_message = |yaml: &str| {
        let VendorConfiguration::Ios(ios) = bind_vendor(yaml, &ctx) else {
            panic!("expected iOS")
        };
        ios.xcresult.queue.validate().unwrap_err().message
    };
    assert_eq!(
        queue_message("type: iOS\nxcresult:\n  queue:\n    capacity: 0"),
        "xcresult.queue.capacity should be at least 1, got 0"
    );
    assert_eq!(
        queue_message("type: iOS\nxcresult:\n  queue:\n    concurrency: 0"),
        "xcresult.queue.concurrency should be between 1 and xcresult.queue.capacity (8), got 0"
    );
    assert_eq!(
        queue_message("type: iOS\nxcresult:\n  queue:\n    concurrency: 9"),
        "xcresult.queue.concurrency should be between 1 and xcresult.queue.capacity (8), got 9"
    );
    assert_eq!(
        queue_message("type: iOS\nxcresult:\n  queue:\n    workerCapacity: 0"),
        "xcresult.queue.workerCapacity should be between 1 and xcresult.queue.capacity (8), got 0"
    );
    assert_eq!(
        queue_message("type: iOS\nxcresult:\n  queue:\n    workerCapacity: 9"),
        "xcresult.queue.workerCapacity should be between 1 and xcresult.queue.capacity (8), got 9"
    );

    let VendorConfiguration::Ios(ios) =
        bind_vendor("type: iOS\nthreadingConfiguration:\n  provisioningConcurrency: 0", &ctx)
    else {
        panic!("expected iOS")
    };
    let error = ios.threading_configuration.validate().unwrap_err();
    assert_eq!(
        error.fqcn(),
        "com.malinskiy.marathon.config.exceptions.ConfigurationException"
    );
    assert_eq!(
        error.message,
        "threadingConfiguration.provisioningConcurrency should be at least 1, got 0"
    );

    let ssh = SshConfiguration {
        known_hosts_path: Some(path("/nope/known_hosts")),
        ..ssh_with(4)
    };
    let error = ssh.validate().unwrap_err();
    assert_eq!(error.fqcn(), "java.io.FileNotFoundException");
    assert_eq!(error.message, "knownhosts file not found at /nope/known_hosts");
}

#[test]
fn wrapping_and_saturating_edges_never_panic() {
    // Kotlin `Int` arithmetic wraps: MIN / -1 == MIN, then max(1, MIN) == 1.
    assert_eq!(XcresultQueueConfiguration::default_worker_capacity(i32::MIN, -1), 1);
    // Approved simplification: concurrency 0 returns 1 (Kotlin throws).
    assert_eq!(XcresultQueueConfiguration::default_worker_capacity(8, 0), 1);
    // `attempt - 1` wraps for attempt == MIN; the loop is skipped.
    let worker = WorkerConnectionConfiguration {
        attempts: 3,
        backoff: secs(10),
        retry_interval: secs(60),
        late_join: true,
        late_join_cutoff: secs(600),
    };
    // `MIN - 1` wraps to `MAX` doublings, capped at the retry interval.
    assert_eq!(worker.pause_after(i32::MIN), Some(secs(60)));
    // Approved simplification: an overflowing doubling saturates to the cap.
    let huge = WorkerConnectionConfiguration {
        attempts: 5,
        backoff: secs(i64::MAX / 2 + 1),
        retry_interval: secs(60),
        ..worker
    };
    assert_eq!(huge.pause_after(3), Some(secs(60)));
}

#[test]
fn duplicate_environment_names_resolve_to_the_first_occurrence() {
    let config = read_environment(&env(&[("ANDROID_HOME", "/first"), ("ANDROID_HOME", "/second")]));
    assert_eq!(
        config.android_sdk.as_deref().map(std::path::Path::as_os_str),
        Some(std::ffi::OsStr::new("/first"))
    );
    // P2: an empty FIRST occurrence counts as unset; no fallback to a later one.
    let config = read_environment(&env(&[("ANDROID_HOME", ""), ("ANDROID_HOME", "/second")]));
    assert_eq!(config.android_sdk, None);
}

#[test]
fn regex_object_form_is_rejected_with_a_named_field() {
    let ctx = context("/", Vec::new());
    let error = bind(
        "type: iOS\ntestParserConfiguration:\n  type: nm\n  testClassRegexes: [{pattern: \"^foo\", options: [IGNORE_CASE]}]",
        &ctx,
        bind_vendor_configuration,
    )
    .unwrap_err();
    let message = error.message.as_ref().map(|m| m.to_owned()).unwrap_or_default();
    assert!(
        message.contains("testClassRegexes"),
        "message names the field: {message}"
    );
    assert!(
        message.contains("only string patterns are supported"),
        "message says only strings: {message}"
    );
}
