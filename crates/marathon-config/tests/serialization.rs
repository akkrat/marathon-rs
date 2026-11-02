//! Port of configuration/src/test/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationSerializationTest.kt,
//! the round-trip tests of configuration/src/test/kotlin/com/malinskiy/marathon/config/vendor/apple/TimeoutConfigurationTest.kt
//! and of configuration/src/test/kotlin/com/malinskiy/marathon/config/vendor/apple/ios/XcresultQueueConfigurationTest.kt.
//!
//! parse → `serialize` → write elsewhere → parse again through the full
//! loader → equal under the ordered full-field equality (WP 2.2 S1). The
//! second parse re-runs all of stage (a): bundle validation against the
//! resolved paths, the SDK lookup, `pull` → `pullingPolicy`.

#[path = "support/load.rs"]
mod load;

use jcompat::writers::JacksonYamlWriter;
use marathon_config::{
    WriterProjection,
    bind_vendor::bind_xcresult_configuration,
    pipeline::bind_bytes,
    serialize,
    vendor::{AttachmentsConfiguration, Lifetime, PullingPolicy, XcresultConfiguration, XcresultQueueConfiguration},
};
use std::fs;

/// `testSimple(path)`: the fixture's parent is `marathonfileDir`, the
/// environment reader returns `/android-sdk`, the copy is re-read from a
/// different (temp) directory.
fn test_simple(path: &str) {
    let (_fixtures, root) = load::fixture_copy().unwrap();
    let file = root.join(path);
    let context = load::context(&file, file.parent().unwrap(), Some("/android-sdk"));
    let parsed = load::parse(&file, &context).unwrap();

    let serialized = serialize(&parsed);
    let (_temp, temp_dir) = load::temp().unwrap();
    let temp_file = temp_dir.join("temp");
    fs::write(&temp_file, serialized.to_utf8_bytes()).unwrap();

    let mut recreated_context = load::context(&temp_file, file.parent().unwrap(), Some("/android-sdk"));
    recreated_context.now = context.now;
    let recreated = load::parse(&temp_file, &recreated_context).unwrap();
    assert_eq!(parsed, recreated, "{path}");
}

#[test]
fn test_simple_1() {
    test_simple("sample_1.yaml");
}
#[test]
fn test_simple_2() {
    test_simple("sample_1_rp.yaml");
}
#[test]
fn test_simple_3() {
    test_simple("sample_2.yaml");
}
#[test]
fn test_simple_4() {
    test_simple("sample_6.yaml");
}
#[test]
fn test_simple_5() {
    test_simple("sample_7.yaml");
}
#[test]
fn test_simple_6() {
    test_simple("sample_8.yaml");
}
#[test]
fn test_simple_7() {
    test_simple("sample_9.yaml");
}
#[test]
fn test_simple_8() {
    test_simple("sample_10.yaml");
}
#[test]
fn test_simple_9() {
    test_simple("sample_11.yaml");
}
#[test]
fn test_simple_10() {
    test_simple("android/sample_1.yaml");
}
#[test]
fn test_simple_11() {
    test_simple("android/sample_2.yaml");
}
#[test]
fn test_simple_12() {
    test_simple("android/sample_3.yaml");
}
#[test]
fn test_simple_13() {
    test_simple("android/sample_4.yaml");
}
#[test]
fn test_simple_14() {
    test_simple("android/sample_5.yaml");
}
#[test]
fn test_simple_15() {
    test_simple("ios/sample_1.yaml");
}
#[test]
fn test_simple_16() {
    test_simple("ios/sample_2.yaml");
}
#[test]
fn test_simple_17() {
    test_simple("ios/sample_3.yaml");
}

/// `TimeoutConfigurationTest`: the canonical temp dir is `marathonfileDir`
/// and every parse rewrites its `Marathonfile`.
fn timeout_round_trip(vendor_type: &str, timeout: &str) {
    let (_temp, dir) = load::temp().unwrap();
    let file = dir.join("Marathonfile");
    let context = load::context(&file, &dir, None);
    let parse = |yaml: &[u8]| {
        fs::write(&file, yaml).unwrap();
        load::parse(&file, &context).unwrap()
    };
    let parsed = parse(
        format!("name: test\noutputDir: out\nvendorConfiguration:\n  type: {vendor_type}\n  timeoutConfiguration:\n    {timeout}\n")
            .as_bytes(),
    );
    let serialized = serialize(&parsed);
    assert_eq!(parse(&serialized.to_utf8_bytes()), parsed);
}

#[test]
fn a_derived_xcresulttool_timeout_survives_a_serialization_round_trip_1() {
    timeout_round_trip("iOS", "shell: PT5M");
}
#[test]
fn a_derived_xcresulttool_timeout_survives_a_serialization_round_trip_2() {
    timeout_round_trip("macOS", "shell: PT5M");
}
#[test]
fn a_derived_boot_wait_survives_a_serialization_round_trip_1() {
    timeout_round_trip("iOS", "boot: PT5M");
}
#[test]
fn a_derived_boot_wait_survives_a_serialization_round_trip_2() {
    timeout_round_trip("macOS", "boot: PT5M");
}

#[test]
fn overrides_survive_a_serialization_round_trip_without_derived_values() {
    // A lone section through a bare mapper: the projection works below the
    // whole `Configuration`.
    let xcresult = XcresultConfiguration {
        pull: None,
        pulling_policy: PullingPolicy::Always,
        remote_clean: true,
        attachments: AttachmentsConfiguration {
            system_attachment_lifetime: Lifetime::DeleteOnSuccess,
            user_attachment_lifetime: Lifetime::KeepAlways,
        },
        preferred_screen_capture_format: None,
        queue: XcresultQueueConfiguration {
            capacity: 64,
            concurrency: 16,
            worker_capacity: Some(8),
        },
    };
    let yaml = JacksonYamlWriter.write(&xcresult.writer_value()).to_string_lossy();
    assert!(yaml.contains("workerCapacity: 8"), "{yaml}");
    assert!(!yaml.contains("effectiveWorkerCapacity"), "{yaml}");
    let (_temp, dir) = load::temp().unwrap();
    let file = dir.join("xcresult.yaml");
    let context = load::context(&file, &dir, None);
    let bound = bind_bytes(yaml.as_bytes(), &context, bind_xcresult_configuration).unwrap();
    assert_eq!(bound, Some(xcresult));
}

/// R1-sol-2: Kotlin data-class equality compares a `Double` property as
/// `Double.equals`, so a NaN `successRate` survives the round trip equal
/// (JVM witness: a Kotlin probe of `ConfigurationFactory` parse →
/// `serialize` → parse printed `NAN_ROUNDTRIP_EQUAL true`), while 0.0 and
/// -0.0 differ.
#[test]
fn a_nan_success_rate_survives_a_serialization_round_trip() {
    let (_temp, dir) = load::temp().unwrap();
    let file = dir.join("Marathonfile");
    fs::write(
        &file,
        "name: test\noutputDir: out\nvendorConfiguration: {type: stub}\nanalyticsConfiguration:\n  type: graphite\n  host: localhost\n  defaults:\n    successRate: \"NaN\"\n",
    )
    .unwrap();
    let context = load::context(&file, &dir, None);
    let parsed = load::parse(&file, &context).unwrap();
    let marathon_config::AnalyticsConfiguration::Graphite(graphite) = &parsed.analytics_configuration else {
        panic!("expected graphite");
    };
    assert!(graphite.defaults.success_rate.is_nan());
    let serialized = serialize(&parsed);
    // Jackson's text (same probe): a plain scalar, not a quoted string.
    assert!(serialized.to_string_lossy().contains("\n    successRate: NaN\n"));
    let again = dir.join("again");
    fs::write(&again, serialized.to_utf8_bytes()).unwrap();
    let mut again_context = load::context(&again, &dir, None);
    again_context.now = context.now;
    assert_eq!(parsed, load::parse(&again, &again_context).unwrap());

    let mut negative_zero = graphite.defaults;
    negative_zero.success_rate = -0.0;
    let mut zero = graphite.defaults;
    zero.success_rate = 0.0;
    assert_ne!(zero, negative_zero);
}
