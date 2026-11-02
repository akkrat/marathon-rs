//! Port of configuration/src/test/kotlin/com/malinskiy/marathon/config/vendor/apple/ThreadingConfigurationTest.kt,
//! configuration/src/test/kotlin/com/malinskiy/marathon/config/vendor/apple/ios/XcresultQueueConfigurationTest.kt and
//! configuration/src/test/kotlin/com/malinskiy/marathon/config/analytics/AnalyticsDefaultsTest.kt (the whole-file,
//! validator rows), plus Rust-only staging tests for WP 2.4.
//!
//! The Kotlin tests write the Marathonfile into a temp dir and pass the
//! copied `ios` fixture dir as `marathonfileDir`, so its `derivedDataDir`
//! bundle validates; nothing is extracted or written there.

#[path = "support/load.rs"]
mod load;

use jcompat::{file_manager::JavaPath, jtime::Duration};
use marathon_config::{
    AnalyticsConfiguration, Configuration, Defaults, VendorConfiguration,
    binding::{
        BindingContext, ConfigurationError, ConfigurationLoader, Profile, ValidationStage, VendorConfigurationHooks,
    },
    devices::Transport,
    loader::{
        DevicesProvider, LogicalConfigurationValidator, MarathonConfigurationLoader, MarathonVendorHooks,
        load_error_to_configuration_error, validation_error_to_configuration_error,
    },
    pipeline::LoadError,
    vendor::{
        AppleThreadingConfiguration, PullingPolicy, SshAuthentication, ValidationError, XcresultQueueConfiguration,
    },
};
use std::{fs, path::Path};

const CONFIGURATION_EXCEPTION: &str = "com.malinskiy.marathon.config.exceptions.ConfigurationException";

/// `parse(…)` of the three Kotlin test classes: `header` then `body` in a
/// temp Marathonfile, `marathonfileDir` = the fixture `ios` dir.
fn parse_with(header: &str, body: &str) -> Result<Configuration, Box<ConfigurationError>> {
    let (_temp, dir) = load::temp().unwrap();
    let file = dir.join("Marathonfile");
    fs::write(&file, format!("{header}\n{body}")).unwrap();
    let fixtures = load::fixture_config().join("ios").canonicalize().unwrap();
    load::parse(&file, &load::context(&file, &fixtures, None))
}
fn apple(name: &str, vendor_type: &str, body: &str) -> Result<Configuration, Box<ConfigurationError>> {
    parse_with(
        &format!(
            "name: \"{name}\"\noutputDir: \"./marathon\"\nvendorConfiguration:\n  type: \"{vendor_type}\"\n  bundle:\n    derivedDataDir: \"derivedDataDir\""
        ),
        body,
    )
}
fn validate(configuration: &Configuration) -> Result<(), Box<ConfigurationError>> {
    load::validate(configuration)
}
fn assert_rejected(configuration: &Configuration, message: &str) {
    let error = validate(configuration).unwrap_err();
    assert_eq!(error.stage, ValidationStage::Logical);
    assert_eq!(load::class(&error), CONFIGURATION_EXCEPTION);
    assert_eq!(load::message(&error), message);
}

// ---- ThreadingConfigurationTest ------------------------------------------------

fn threading(configuration: &Configuration) -> AppleThreadingConfiguration {
    match &configuration.vendor_configuration {
        VendorConfiguration::Ios(c) => c.threading_configuration.clone(),
        VendorConfiguration::Macos(c) => c.threading_configuration.clone(),
        other => panic!("unexpected vendor {other:?}"),
    }
}
fn threads(
    device_provider_threads: i32,
    device_threads: i32,
    provisioning_concurrency: i32,
) -> AppleThreadingConfiguration {
    AppleThreadingConfiguration {
        device_provider_threads,
        device_threads,
        provisioning_concurrency,
    }
}

#[test]
fn simulators_are_provisioned_two_at_a_time_per_worker_by_default() {
    let configuration = apple("threading", "iOS", "").unwrap();
    validate(&configuration).unwrap();
    assert_eq!(threading(&configuration), threads(8, 2, 2));
}

#[test]
fn provisioning_concurrency_is_parsed() {
    let configuration = apple(
        "threading",
        "iOS",
        "  threadingConfiguration:\n    deviceProviderThreads: 32\n    deviceThreads: 4\n    provisioningConcurrency: 4",
    )
    .unwrap();
    validate(&configuration).unwrap();
    assert_eq!(threading(&configuration), threads(32, 4, 4));
}

#[test]
fn provisioning_concurrency_below_one_is_rejected_for_ios() {
    let configuration = apple(
        "threading",
        "iOS",
        "  threadingConfiguration:\n    provisioningConcurrency: 0",
    )
    .unwrap();
    assert_rejected(
        &configuration,
        "threadingConfiguration.provisioningConcurrency should be at least 1, got 0",
    );
}

#[test]
fn macos_rejects_provisioning_concurrency_since_it_provisions_no_simulators() {
    let configuration = apple(
        "threading",
        "macOS",
        "  threadingConfiguration:\n    provisioningConcurrency: 4",
    )
    .unwrap();
    assert_rejected(
        &configuration,
        "threadingConfiguration.provisioningConcurrency is supported only for iOS simulators",
    );
    let defaults = apple("threading", "macOS", "  threadingConfiguration:\n    deviceThreads: 4").unwrap();
    validate(&defaults).unwrap();
    assert_eq!(threading(&defaults), threads(8, 4, 2));
}

// ---- XcresultQueueConfigurationTest --------------------------------------------

fn queue(configuration: &Configuration) -> XcresultQueueConfiguration {
    let VendorConfiguration::Ios(ios) = &configuration.vendor_configuration else {
        panic!("expected iOS");
    };
    ios.xcresult.queue.clone()
}
fn ios_queue(body: &str) -> Configuration {
    apple("queue", "iOS", body).unwrap()
}

#[test]
fn defaults_keep_the_run_limits_and_give_each_worker_a_share_of_the_run_capacity() {
    let configuration = ios_queue("");
    validate(&configuration).unwrap();
    assert_eq!(
        queue(&configuration),
        XcresultQueueConfiguration {
            capacity: 8,
            concurrency: 4,
            worker_capacity: None,
        }
    );
    assert_eq!(queue(&configuration).effective_worker_capacity(), 2);
}

#[test]
fn overrides_are_parsed_and_validated() {
    let configuration = ios_queue(
        "  xcresult:\n    pullingPolicy: ON_FAILURE\n    queue:\n      capacity: 64\n      concurrency: 16\n      workerCapacity: 8",
    );
    validate(&configuration).unwrap();
    let VendorConfiguration::Ios(ios) = &configuration.vendor_configuration else {
        panic!("expected iOS");
    };
    assert_eq!(ios.xcresult.pulling_policy, PullingPolicy::OnFailure);
    assert_eq!(
        ios.xcresult.queue,
        XcresultQueueConfiguration {
            capacity: 64,
            concurrency: 16,
            worker_capacity: Some(8),
        }
    );
    assert_eq!(ios.xcresult.queue.effective_worker_capacity(), 8);
    // Without an explicit worker capacity the share follows the configured limits.
    let derived = ios_queue("  xcresult:\n    queue:\n      capacity: 64\n      concurrency: 16");
    validate(&derived).unwrap();
    assert_eq!(queue(&derived).effective_worker_capacity(), 4);
    // A worker may be given the whole capacity, which restores global-first admission.
    validate(&ios_queue("  xcresult:\n    queue:\n      workerCapacity: 8")).unwrap();
    // An explicit null keeps the derived share.
    let explicit_null = ios_queue("  xcresult:\n    queue:\n      workerCapacity: null");
    validate(&explicit_null).unwrap();
    assert_eq!(queue(&explicit_null), XcresultQueueConfiguration::default_instance());
}

#[test]
fn invalid_limits_are_rejected() {
    let invalid = [
        "capacity: 0",
        "capacity: -1",
        "concurrency: 0",
        "concurrency: 9",
        "capacity: 2",
        "workerCapacity: 0",
        "workerCapacity: 9",
        "capacity: 16\n      workerCapacity: 17",
    ];
    for limits in invalid {
        let configuration = ios_queue(&format!("  xcresult:\n    queue:\n      {limits}"));
        let error = validate(&configuration).unwrap_err();
        assert_eq!(load::class(&error), CONFIGURATION_EXCEPTION, "{limits}");
    }
    for body in [
        "  xcresult:\n    queue:\n      capacity: many",
        "  xcresult:\n    queue:\n      transfers: 4",
    ] {
        let error = apple("queue", "iOS", body).unwrap_err();
        assert_eq!(load::class(&error), CONFIGURATION_EXCEPTION, "{body}");
    }
}

#[test]
fn macos_rejects_the_ios_queue_limits() {
    let macos = apple("queue", "macOS", "  xcresult:\n    queue:\n      capacity: 16").unwrap();
    assert!(matches!(macos.vendor_configuration, VendorConfiguration::Macos(_)));
    assert_rejected(&macos, "xcresult.queue is supported only for iOS simulators");
    validate(&apple("queue", "macOS", "  xcresult:\n    remoteClean: false").unwrap()).unwrap();
}

// ---- AnalyticsDefaultsTest -------------------------------------------------------

const BACKENDS: [&str; 3] = [
    "  type: \"influxdb\"\n  url: \"http://influx.svc.cluster.local:8086\"\n  user: \"root\"\n  password: \"root\"\n  dbName: \"marathon\"",
    "  type: \"influxdb2\"\n  url: \"http://influx2.svc.cluster.local:8086\"\n  token: \"token\"\n  organization: \"starlabs\"\n  bucket: \"marathon\"",
    "  type: \"graphite\"\n  host: \"graphite.svc.cluster.local\"",
];
fn analytics(body: &str) -> Configuration {
    parse_with(
        "name: \"analytics\"\noutputDir: \"./marathon\"\nvendorConfiguration:\n  type: \"iOS\"\n  bundle:\n    derivedDataDir: \"derivedDataDir\"\nanalyticsConfiguration:",
        body,
    )
    .unwrap()
}
fn defaults(configuration: &Configuration) -> Option<Defaults> {
    match &configuration.analytics_configuration {
        AnalyticsConfiguration::InfluxDb(c) => Some(c.defaults),
        AnalyticsConfiguration::InfluxDb2(c) => Some(c.defaults),
        AnalyticsConfiguration::Graphite(c) => Some(c.defaults),
        AnalyticsConfiguration::Disabled => None,
    }
}

/// The whole-file, validator-included form of the WP 2.2 section-level
/// test of the same name (`tests/schema.rs`), completed here (WP 2.2
/// findings, "Carried to WP 2.4").
#[test]
fn observed_duration_min_tests_is_parsed_for_every_backend() {
    for backend in BACKENDS {
        let configuration = analytics(&format!(
            "{backend}\n  defaults:\n    duration: \"PT40S\"\n    observedDurationMinTests: 20"
        ));
        validate(&configuration).unwrap();
        assert_eq!(
            defaults(&configuration),
            Some(Defaults {
                success_rate: 0.0,
                duration: Duration::of_seconds(40),
                observed_duration_min_tests: Some(20),
            })
        );
    }
}

#[test]
fn observed_duration_min_tests_below_one_is_rejected() {
    for backend in BACKENDS {
        let configuration = analytics(&format!("{backend}\n  defaults:\n    observedDurationMinTests: 0"));
        assert_rejected(
            &configuration,
            "analyticsConfiguration.defaults.observedDurationMinTests must be at least 1, got 0",
        );
    }
}

// ---- Rust-only staging tests -----------------------------------------------------

/// `ConfigurationFactory.kt:192-199`: each flag is false when the CLI or the
/// file says false, independently of the other flag.
#[test]
fn tracking_flags_and_merge_cli_and_file_independently() {
    for (cli, file, expected) in [
        (true, true, true),
        (true, false, false),
        (false, true, false),
        (false, false, false),
    ] {
        let (_temp, dir) = load::temp().unwrap();
        let path = dir.join("Marathonfile");
        fs::write(
            &path,
            format!("name: t\noutputDir: out\nanalyticsTracking: {file}\nbugsnagReporting: true\nvendorConfiguration:\n  type: stub"),
        )
        .unwrap();
        let mut context = load::context(&path, &dir, None);
        context.analytics_tracking = cli;
        let configuration = load::parse(&path, &context).unwrap();
        assert_eq!(configuration.analytics_tracking, expected, "cli {cli}, file {file}");
        assert!(configuration.bugsnag_reporting, "cli {cli}, file {file}");
    }
}

/// The merge runs after the vendor branch, so an `Empty` rejection wins
/// even when the tracking flags are off.
#[test]
fn empty_vendor_rejection_precedes_the_tracking_merge() {
    let (_temp, dir) = load::temp().unwrap();
    let path = dir.join("Marathonfile");
    fs::write(&path, "name: t\noutputDir: out\nanalyticsTracking: false").unwrap();
    let error = load::parse(&path, &load::context(&path, &dir, None)).unwrap_err();
    assert_eq!(error.stage, ValidationStage::Parse);
    assert_eq!(load::class(&error), CONFIGURATION_EXCEPTION);
    assert_eq!(load::message(&error), "No vendor configuration specified");
    assert!(error.causes.is_empty(), "unwrapped");
}

/// The factory's own exceptions escape its `JsonProcessingException`
/// catch, so they carry no "Error parsing config file" wrapper; binding
/// failures do.
#[test]
fn stage_a_vendor_errors_are_unwrapped_and_binding_errors_wrapped() {
    let (_temp, dir) = load::temp().unwrap();
    let path = dir.join("Marathonfile");
    let parse = |text: &str| {
        fs::write(&path, text).unwrap();
        load::parse(&path, &load::context(&path, &dir, None)).unwrap_err()
    };
    let sdk = parse("name: t\noutputDir: out\nvendorConfiguration:\n  type: Android");
    assert_eq!(
        (load::class(&sdk), load::message(&sdk)),
        (CONFIGURATION_EXCEPTION.into(), "No android SDK path specified".into())
    );
    assert!(sdk.causes.is_empty());
    let bundle = parse(
        "name: t\noutputDir: out\nvendorConfiguration:\n  type: macOS\n  bundle:\n    application: missing.ipa\n    testApplication: missing.zip",
    );
    assert_eq!(bundle.stage, ValidationStage::Parse);
    assert_eq!(
        load::message(&bundle),
        "application should be .ipa/.zip archive or a .app folder"
    );
    assert!(bundle.causes.is_empty());
    let binding = parse("name: t\noutputDir: out\nvendorConfiguration:\n  type: Android\n  bogus: 1");
    assert_eq!(load::class(&binding), CONFIGURATION_EXCEPTION);
    assert_eq!(
        load::message(&binding),
        format!("Error parsing config file {}", path.to_string_lossy())
    );
    assert_eq!(binding.causes.len(), 1);
}

/// One converter for both stages: FQCN class (F18's `FileNotFoundException`
/// intact), verbatim message, no wrapper, no node context (P4).
#[test]
fn validation_errors_convert_with_their_class_at_both_stages() {
    let error = ValidationError {
        class: marathon_config::vendor::ExceptionClass::FileNotFound,
        message: "Private key not found at /k".into(),
    };
    for stage in [ValidationStage::Parse, ValidationStage::Logical] {
        let converted = validation_error_to_configuration_error(error.clone(), stage);
        assert_eq!(converted.stage, stage);
        assert_eq!(converted.class.to_string_lossy(), "java.io.FileNotFoundException");
        assert_eq!(load::message(&converted), "Private key not found at /k");
        assert!(converted.causes.is_empty() && converted.path.is_empty() && converted.span.is_none());
    }
}

/// Orchestrator decision 3: the `LoadError`s without a `ConfigurationError`.
#[test]
fn load_errors_map_to_their_kotlin_classes_at_parse() {
    let worker = load_error_to_configuration_error(LoadError::Worker(std::io::Error::other("no threads")));
    assert_eq!(worker.stage, ValidationStage::Parse);
    assert_eq!(worker.class.to_string_lossy(), "java.lang.IllegalStateException");
    let null = load_error_to_configuration_error(LoadError::NullDocument);
    assert_eq!(null.class.to_string_lossy(), "java.lang.NullPointerException");
    assert!(
        null.causes.is_empty(),
        "the NPE escapes the ConfigurationException wrapper"
    );
}

/// Orchestrator decision 2: `devices` rebuilds a P2 context, so a caller's
/// P1 context cannot re-enable Marathonfile-relative `File` resolution (F4).
#[test]
fn devices_binds_with_p2_whatever_the_callers_profile() {
    let context = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: JavaPath::from_text("/elsewhere/Marathonfile"),
        analytics_tracking: true,
        bugsnag_reporting: true,
        marathonfile_dir: JavaPath::from_text("/elsewhere"),
        process_dir: JavaPath::from_text("/elsewhere"),
        environment: Vec::new(),
        now: load::reference_now(),
    };
    let yaml = "workers: [{transport: {type: ssh, addr: node, authentication: {type: publicKey, username: u, key: \"relative/key.pem\"}}, devices: []}]";
    let devices = load::ready(ConfigurationLoader::devices(
        &MarathonConfigurationLoader,
        yaml.as_bytes(),
        &context,
    ))
    .unwrap();
    let Transport::Ssh {
        authentication: Some(SshAuthentication::PublicKey { key, .. }),
        ..
    } = &devices.workers[0].transport
    else {
        panic!("expected ssh publicKey");
    };
    assert_eq!(String::from_utf16_lossy(&key.0), "relative/key.pem");
    // P2 binding failures keep the Provider stage and their own class.
    let error = load::ready(ConfigurationLoader::devices(
        &MarathonConfigurationLoader,
        b"workers: [{bogus: 1}]",
        &context,
    ))
    .unwrap_err();
    assert_eq!(error.stage, ValidationStage::Provider);
    assert_ne!(load::class(&error), CONFIGURATION_EXCEPTION);
}

/// R1-sol-1: a null Marathondevices document is the provider's NPE on
/// `.workers` at stage `Provider`, not the Marathonfile's P1 NPE. JVM
/// witnesses: a Kotlin probe of `readValue<Marathondevices>("null")` then
/// `marathonfile.workers` (iOS) and `tools/oracle/null-devices-probe` (macOS).
#[test]
fn null_marathondevices_is_the_providers_npe() {
    let (_tmp, dir) = load::temp().unwrap();
    let context = load::context(&dir.join("Marathonfile"), &dir, None);
    let ios = load::ready(ConfigurationLoader::devices(
        &MarathonConfigurationLoader,
        b"null",
        &context,
    ))
    .unwrap_err();
    assert_eq!(ios.stage, ValidationStage::Provider);
    assert_eq!(load::class(&ios), "java.lang.NullPointerException");
    assert_eq!(
        load::message(&ios),
        "Cannot invoke \"com.malinskiy.marathon.apple.configuration.Marathondevices.getWorkers()\" because \"marathonfile\" is null"
    );
    assert!(ios.causes.is_empty());
    let macos = MarathonConfigurationLoader
        .devices_sync_for(DevicesProvider::Macos, b"null", &context)
        .unwrap_err();
    assert_eq!(macos.stage, ValidationStage::Provider);
    assert_eq!(load::class(&macos), "java.lang.NullPointerException");
    assert_eq!(
        load::message(&macos),
        "Cannot invoke \"com.malinskiy.marathon.apple.configuration.Marathondevices.getWorkers()\" because the return value of \"com.fasterxml.jackson.databind.ObjectMapper.readValue(String, com.fasterxml.jackson.core.type.TypeReference)\" is null"
    );
}

/// F6 through the hooks: iOS runs `IOSConfiguration.validate()` at stage
/// (b), macOS never runs `MacosConfiguration.validate()`.
#[test]
fn vendor_hooks_validate_ios_but_not_macos() {
    let ios = apple("hooks", "iOS", "  maxBatchesWithoutProgress: -1").unwrap();
    let error = load::ready(VendorConfigurationHooks::validate(
        &MarathonVendorHooks,
        &ios.vendor_configuration,
    ))
    .unwrap_err();
    assert_eq!(error.stage, ValidationStage::Logical);
    assert_eq!(
        load::message(&error),
        "maxBatchesWithoutProgress should not be negative, got -1"
    );
    // No bundle and a missing SSH key: both fail MacosConfiguration.validate().
    let macos = parse_with(
        "name: hooks\noutputDir: out\nvendorConfiguration:\n  type: macOS\n  ssh:\n    authentication:\n      type: publicKey\n      username: u\n      key: nokey",
        "",
    )
    .unwrap();
    let VendorConfiguration::Macos(m) = &macos.vendor_configuration else {
        panic!("expected macOS");
    };
    assert!(m.validate().is_err(), "the method exists and would reject");
    load::ready(VendorConfigurationHooks::validate(
        &MarathonVendorHooks,
        &macos.vendor_configuration,
    ))
    .unwrap();
    load::ready(marathon_config::binding::ConfigurationValidator::validate(
        &LogicalConfigurationValidator,
        &macos,
    ))
    .unwrap();
}

/// Stage (b) re-runs bundle extraction on a clone (WP 2.4 P2 rejected):
/// the extraction directory is rebuilt by the validator, after stage (a)
/// built it once.
#[test]
fn stage_b_re_extracts_the_bundle() {
    let (_tmp, root) = load::fixture_copy().unwrap();
    let dir = root.join("ios");
    let path = dir.join("Marathonfile");
    fs::write(
        &path,
        "name: t\noutputDir: out\nvendorConfiguration:\n  type: iOS\n  bundle:\n    application: ipa/app.ipa\n    testApplication: ipa/testApp.ipa",
    )
    .unwrap();
    let configuration = load::parse(&path, &load::context(&path, &dir, None)).unwrap();
    let extracted = dir.join("ipa/testApp");
    assert!(extracted.is_dir(), "stage (a) extracted");
    let marker = extracted.join("marker");
    fs::write(&marker, "").unwrap();
    // The fixture bundles have no SSH key or devices to trip on first.
    let result = validate(&configuration);
    assert!(!marker.exists(), "stage (b) deleted and re-extracted: {result:?}");
    assert!(extracted.is_dir());
    assert!(
        !Path::new(&load::fixture_config().join("ios/ipa/testApp")).exists(),
        "fixtures untouched"
    );
}

/// JVM witnesses: WP 2.6 `u49-tab`, `u49-eof`, `r1-wrapper-*`, `r2-reader-control*`
/// and `r2-code-point-*`, recorded by ConfigDump.kt.
#[test]
fn yaml_syntax_errors_keep_the_wrapper_and_original_parser_cause() -> Result<(), Box<dyn std::error::Error>> {
    const MARKED: &str = "com.fasterxml.jackson.dataformat.yaml.snakeyaml.error.MarkedYAMLException";
    const JACKSON: &str = "com.fasterxml.jackson.dataformat.yaml.JacksonYAMLParseException";
    let (_tmp, dir) = load::temp()?;
    let file = dir.join("Marathonfile");
    let context = load::context(&file, &dir, None);
    let long = "a".repeat(3 * 1024 * 1024 + 10);
    for (input, wrapper, original) in [
        (
            "name: t\noutputDir: out\nvendorConfiguration:\n\ttype: stub\n".to_string(),
            MARKED,
            "org.yaml.snakeyaml.scanner.ScannerException",
        ),
        (
            "name: t\noutputDir: out\nvendorConfiguration: [".to_string(),
            MARKED,
            "org.yaml.snakeyaml.parser.ParserException",
        ),
        (
            "name: t\noutputDir: out\nvendorConfiguration: [stub,".to_string(),
            MARKED,
            "org.yaml.snakeyaml.parser.ParserException",
        ),
        (
            "name: t\noutputDir: out\nvendorConfiguration: [stub".to_string(),
            MARKED,
            "org.yaml.snakeyaml.parser.ParserException",
        ),
        (
            "name: \x01t\noutputDir: out\nvendorConfiguration: {type: stub}\n".to_string(),
            JACKSON,
            "org.yaml.snakeyaml.reader.ReaderException",
        ),
        (
            "name: t\noutputDir: out\nvendorConfiguration: {type: stub}\n# \x01".to_string(),
            JACKSON,
            "org.yaml.snakeyaml.reader.ReaderException",
        ),
        (
            format!("name: \"{long}\"\noutputDir: out\nvendorConfiguration: {{type: stub}}\n"),
            JACKSON,
            "org.yaml.snakeyaml.error.YAMLException",
        ),
        // The buffered unknown property's value is read before the creator reports it.
        (
            format!("name: t\noutputDir: out\nvendorConfiguration: {{type: stub}}\nx: \"{long}\""),
            JACKSON,
            "org.yaml.snakeyaml.error.YAMLException",
        ),
    ] {
        let error = MarathonConfigurationLoader
            .parse_sync(input.as_bytes(), &context)
            .err()
            .ok_or("the malformed input must fail at parse")?;
        assert_eq!(error.stage, ValidationStage::Parse);
        assert_eq!(error.class.to_string_lossy(), CONFIGURATION_EXCEPTION);
        let causes: Vec<_> = error.causes.iter().map(|c| c.class.to_string_lossy()).collect();
        assert_eq!(causes, [wrapper, original]);
    }
    Ok(())
}

/// JVM witnesses: WP 2.6 `r2-regex-*`. RegexDeserializer names the tree's JsonNodeType.
#[test]
fn regex_scalar_kinds_use_json_node_type_names() -> Result<(), Box<dyn std::error::Error>> {
    let (_tmp, dir) = load::temp()?;
    let file = dir.join("Marathonfile");
    let context = load::context(&file, &dir, None);
    let base = "name: t\noutputDir: out\nvendorConfiguration: {type: stub}\n";
    for (tail, kind) in [
        ("includeSerialRegexes: [1]", "NUMBER"),
        ("includeSerialRegexes: [1.5]", "NUMBER"),
        ("includeSerialRegexes: [yes]", "BOOLEAN"),
        ("includeSerialRegexes: [false]", "BOOLEAN"),
        ("includeSerialRegexes: [!!binary aGk=]", "BINARY"),
        ("includeSerialRegexes: [[]]", "ARRAY"),
        (
            "filteringConfiguration: {allowlist: [{type: simple-class-name, regex: 5}]}",
            "NUMBER",
        ),
    ] {
        let error = MarathonConfigurationLoader
            .parse_sync(format!("{base}{tail}\n").as_bytes(), &context)
            .err()
            .ok_or("a non-string regex must fail at parse")?;
        let message = format!("Expected a string or an object to deserialize a Regex, but type was {kind}");
        let causes: Vec<_> = error
            .causes
            .iter()
            .map(|c| {
                (
                    c.class.to_string_lossy(),
                    c.message.as_ref().map(|m| m.to_string_lossy()),
                )
            })
            .collect();
        assert_eq!(
            causes,
            [
                (
                    "com.fasterxml.jackson.databind.JsonMappingException".into(),
                    Some(message.clone())
                ),
                ("java.lang.IllegalStateException".into(), Some(message)),
            ],
            "{tail}"
        );
    }
    Ok(())
}

/// JVM witnesses: WP 2.6 `r2-creator-*`. A property-based creator buffers unknown
/// properties: later field errors and missing creator parameters are reported first.
#[test]
fn unknown_properties_wait_for_the_creator() -> Result<(), Box<dyn std::error::Error>> {
    const MISSING: &str = "com.fasterxml.jackson.module.kotlin.MissingKotlinParameterException";
    const MISMATCHED: &str = "com.fasterxml.jackson.databind.exc.MismatchedInputException";
    const UNKNOWN: &str = "com.fasterxml.jackson.databind.exc.UnrecognizedPropertyException";
    let (_tmp, dir) = load::temp()?;
    let file = dir.join("Marathonfile");
    let context = load::context(&file, &dir, None);
    let stub = "vendorConfiguration: {type: stub}\n";
    let filter = format!("name: t\noutputDir: out\n{stub}filteringConfiguration: {{allowlist: [");
    for (input, class) in [
        (format!("outputDir: out\nunknownDifferentialKey: 1\n{stub}"), MISSING),
        (format!("name: null\nunknownDifferentialKey: 1\noutputDir: out\n{stub}"), MISSING),
        (format!("outputDir: out\n{stub}unknownDifferentialKey: 1\n"), MISSING),
        (format!("unknownDifferentialKey: 1\nname: t\noutputDir: out\ndebug: []\n{stub}"), MISMATCHED),
        (format!("{filter}{{type: composition, filters: [], unknownDifferentialKey: 1}}]}}\n"), MISSING),
        (format!("{filter}{{type: composition, unknownDifferentialKey: 1}}]}}\n"), MISSING),
        (format!("{filter}{{type: composition, unknownDifferentialKey: 1, filters: [], op: []}}]}}\n"), MISMATCHED),
        (
            format!("name: t\noutputDir: out\n{stub}outputConfiguration: {{unknownDifferentialKey: 1}}\ndebug: []\n"),
            UNKNOWN,
        ),
        (
            "name: t\noutputDir: out\nvendorConfiguration: [Android, {androidSdk: /s, unknownDifferentialKey: 1, autoGrantPermission: []}]\n".into(),
            MISMATCHED,
        ),
        (format!("name: t\noutputDir: out\nunknownDifferentialKey: 1\n{stub}"), UNKNOWN),
    ] {
        let error = MarathonConfigurationLoader
            .parse_sync(input.as_bytes(), &context)
            .err()
            .ok_or("the input must fail at parse")?;
        let causes: Vec<_> = error.causes.iter().map(|c| c.class.to_string_lossy()).collect();
        assert_eq!(causes, [class], "{input}");
    }
    Ok(())
}

/// JVM witness: WP 2.6 `r1-wrapper-end`; END_ARRAY binds a null subtype.
#[test]
fn wrapper_array_without_a_body_keeps_the_missing_parameter_error() -> Result<(), Box<dyn std::error::Error>> {
    let (_tmp, dir) = load::temp()?;
    let file = dir.join("Marathonfile");
    let context = load::context(&file, &dir, None);
    let error = MarathonConfigurationLoader
        .parse_sync(b"name: t\noutputDir: out\nvendorConfiguration: [stub]\n", &context)
        .err()
        .ok_or("the absent vendor must fail at parse")?;
    assert_eq!(error.stage, ValidationStage::Parse);
    assert_eq!(error.class.to_string_lossy(), CONFIGURATION_EXCEPTION);
    assert_eq!(error.causes.len(), 1);
    assert_eq!(
        error.causes[0].class.to_string_lossy(),
        "com.fasterxml.jackson.module.kotlin.MissingKotlinParameterException"
    );
    Ok(())
}
