//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/Configuration.kt
//! WP 2.2 schema corpus plus the ported AnalyticsDefaultsTest rows.
#[path = "support/schema_bind.rs"]
mod schema_bind;

use jcompat::jtime::{parse_duration, parse_instant};
use marathon_config::{
    AnalyticsConfiguration, Defaults,
    bind::{bind_analytics_configuration, bind_configuration},
    bind_vendor::bind_vendor_configuration,
    binder::{Binder, Result, Value},
    binding::{BindingContext, ValidationStage},
    node,
};
use schema_bind::{Outcome, bind_target, outcome_matches};
use serde_json::Value as Json;

/// Approved divergences (S4/S5/S7): the JVM row stays in the corpus; each
/// row asserts its specific D9 outcome (class + exact message, R1-claude-3),
/// not just "some parse error".
const REJECT_INSTEAD: &[(&str, &str, &str)] = &[
    (
        "Configuration-null-list-elem",
        marathon_config::binder::MISMATCHED,
        "D9: null element in collection",
    ),
    (
        "FilteringConfiguration-allow-null-elem",
        marathon_config::binder::MISMATCHED,
        "D9: null element in collection",
    ),
    (
        "TestFilterConfiguration-composition-null-elem",
        marathon_config::binder::MISMATCHED,
        "D9: null element in collection",
    ),
    (
        "Pooling-combo-null-elem",
        marathon_config::binder::MISMATCHED,
        "D9: null element in collection",
    ),
    (
        "AnalyticsConfiguration-graphite-port-arabic",
        marathon_config::bind::CONFIGURATION_EXCEPTION,
        "GraphiteConfigurationDeserializer: port should be a number, e.g. 2003",
    ),
    (
        "Sorting-exec-overflow",
        marathon_config::binder::JSON_MAPPING,
        "D9: now + Duration overflows Instant",
    ),
    (
        "Batching-fixed-overflow",
        marathon_config::binder::JSON_MAPPING,
        "D9: now + Duration overflows Instant",
    ),
    (
        "Flakiness-prob-overflow",
        marathon_config::binder::JSON_MAPPING,
        "D9: now + Duration overflows Instant",
    ),
];

/// S4 narrowed (R1-claude-2): a null element in a filter `values` list is
/// dropped instead of rejected (Kotlin only contains/intersect-checks it,
/// so it could never match). The expected value is the JVM rendering with
/// the nulls under `values` removed.
const DROP_NULL_VALUES: &[&str] = &["TestFilterConfiguration-method-values-null-elem"];

fn drop_nulls_under_values(value: &mut Json) {
    match value {
        Json::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key == "values"
                    && let Json::Array(xs) = child
                {
                    xs.retain(|x| !x.is_null());
                } else {
                    drop_nulls_under_values(child);
                }
            }
        }
        Json::Array(xs) => xs.iter_mut().for_each(drop_nulls_under_values),
        _ => {}
    }
}

#[test]
fn schema_corpus_matches_jvm() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut checked = 0;
    for line in include_str!("../../../fixtures/oracle/yaml-schema-corpus.jsonl")
        .lines()
        .skip(1)
    {
        let row: Json = serde_json::from_str(line)?;
        let id = row["id"].as_str().ok_or("id")?;
        let target = row["target"].as_str().ok_or("target")?;
        let input = row["input"].as_str().ok_or("input")?;
        match target {
            "InstantPlus" => {
                let now = parse_instant("2026-10-12T00:00:00Z").ok_or("now")?;
                let got = parse_duration(input).and_then(|d| now.checked_add(d));
                match (&row["result"]["ok"], got) {
                    (Json::String(s), Some(i)) => assert_eq!(&i.to_string(), s, "{id}"),
                    (Json::Null, None) => {}
                    _ => panic!("{id}: expected {:?} got {got:?}", row["result"]),
                }
            }
            "NodeString" => {
                let stream = node::load(input);
                let context = schema_bind::corpus_context();
                let binder = Binder::new(&context, &stream);
                let rendered = binder
                    .first(&stream, |v: Value<'_>| -> Result<String> {
                        let tree = v.read_tree()?;
                        let x = tree.get("x")?.ok_or_else(|| v.error("test", "x"))?;
                        Ok(x.node_string())
                    })
                    .map_err(|e| format!("{id}: {e:?}"))?;
                assert_eq!(rendered.as_deref(), row["result"]["ok"].as_str(), "{id}");
            }
            _ => {
                let stream = node::load(input);
                let got = bind_target(&stream, target);
                if let Some((_, class, message)) = REJECT_INSTEAD.iter().find(|(rid, _, _)| *rid == id) {
                    let Outcome::Err(e) = &got else {
                        panic!("{id}: approved simplification must reject at bind");
                    };
                    assert_eq!(e.stage, ValidationStage::Parse, "{id}");
                    assert_eq!(e.class.to_string_lossy(), *class, "{id}");
                    assert_eq!(
                        e.message.as_ref().map(|m| m.to_string_lossy()).as_deref(),
                        Some(*message),
                        "{id}"
                    );
                } else if DROP_NULL_VALUES.contains(&id) {
                    let mut expected = row["result"].clone();
                    drop_nulls_under_values(&mut expected);
                    if let Err(diff) = outcome_matches(&got, &expected) {
                        panic!("{id} ({target}) input={input:?}: {diff}");
                    }
                } else if let Err(diff) = outcome_matches(&got, &row["result"]) {
                    panic!("{id} ({target}) input={input:?}: {diff}");
                }
            }
        }
        checked += 1;
    }
    assert_eq!(checked, 206);
    Ok(())
}

// ---- Ported AnalyticsDefaultsTest rows (section level, WP 2.2 decision 1) ----

fn context() -> BindingContext {
    schema_bind::corpus_context()
}
fn bind<T>(text: &str, schema: impl FnOnce(Value<'_>) -> Result<T>) -> Result<Option<T>> {
    let stream = node::load(text);
    let context = context();
    let binder = Binder::new(&context, &stream);
    binder.first(&stream, schema)
}
fn analytics_defaults(text: &str) -> Option<Defaults> {
    match bind(text, bind_analytics_configuration).unwrap().flatten() {
        Some(AnalyticsConfiguration::InfluxDb(c)) => Some(c.defaults),
        Some(AnalyticsConfiguration::InfluxDb2(c)) => Some(c.defaults),
        Some(AnalyticsConfiguration::Graphite(c)) => Some(c.defaults),
        Some(AnalyticsConfiguration::Disabled) | None => None,
    }
}
fn defaults(sr: f64, secs: i64, observed: Option<i32>) -> Defaults {
    Defaults {
        success_rate: sr,
        duration: jcompat::jtime::duration_of(secs, 0).unwrap(),
        observed_duration_min_tests: observed,
    }
}
const BACKENDS: [&str; 3] = [
    "type: \"influxdb\"\nurl: \"http://influx.svc.cluster.local:8086\"\nuser: \"root\"\npassword: \"root\"\ndbName: \"marathon\"",
    "type: \"influxdb2\"\nurl: \"http://influx2.svc.cluster.local:8086\"\ntoken: \"token\"\norganization: \"starlabs\"\nbucket: \"marathon\"",
    "type: \"graphite\"\nhost: \"graphite.svc.cluster.local\"",
];

#[test]
fn defaults_without_observed_duration_min_tests_parse_as_before() {
    for backend in BACKENDS {
        assert_eq!(analytics_defaults(backend), Some(defaults(0.0, 300, None)));
        let with = format!("{backend}\ndefaults:\n  successRate: 0.1\n  duration: \"PT40S\"");
        assert_eq!(analytics_defaults(&with), Some(defaults(0.1, 40, None)));
    }
}

// R1-sol-1: Kotlin `Int?` binds with Jackson boxed semantics, while a
// primitive `Int` keeps the zero coercion for the same spellings.
#[test]
fn nullable_int_uses_boxed_semantics_primitive_int_keeps_zero() {
    use marathon_config::bind::bind_output_configuration;
    for spelling in ["\"\"", "\" \"", "\"null\""] {
        for backend in BACKENDS {
            let with = format!("{backend}\ndefaults:\n  observedDurationMinTests: {spelling}");
            assert_eq!(analytics_defaults(&with), Some(defaults(0.0, 300, None)), "{spelling}");
        }
        let primitive = bind(&format!("maxPath: {spelling}"), bind_output_configuration)
            .unwrap()
            .flatten()
            .unwrap();
        assert_eq!(primitive.max_path, 0, "{spelling}");
    }
}

// Section-level half. The Kotlin original also asserts the validator does
// not throw for the whole file; WP 2.4 completed that in
// `tests/validation.rs::observed_duration_min_tests_is_parsed_for_every_backend`.
#[test]
fn observed_duration_min_tests_is_parsed_for_every_backend() {
    for backend in BACKENDS {
        let with = format!("{backend}\ndefaults:\n  duration: \"PT40S\"\n  observedDurationMinTests: 20");
        assert_eq!(analytics_defaults(&with), Some(defaults(0.0, 40, Some(20))));
    }
}

// Whole-file shape of the same rows (validator included) is WP 2.4; the
// section-level Configuration binding with an absent vendor also parses.
#[test]
fn configuration_builder_defaults_and_vendor_seam() {
    let got = bind("name: analytics\noutputDir: ./marathon", |v| {
        bind_configuration(v, bind_vendor_configuration)
    })
    .unwrap()
    .flatten()
    .unwrap();
    assert_eq!(got.name.to_string_lossy(), "analytics");
    assert!(got.debug);
    assert_eq!(got.test_batch_timeout_millis, 1_800_000);
    assert_eq!(got.vendor_configuration, marathon_config::VendorConfiguration::Empty);
    assert_eq!(got.ci_configuration, marathon_config::CiConfiguration::None);
    assert!(matches!(got.analytics_configuration, AnalyticsConfiguration::Disabled));
    let with_empty = bind("name: n\noutputDir: ./o\nvendorConfiguration: {type: empty}", |v| {
        bind_configuration(v, bind_vendor_configuration)
    })
    .unwrap()
    .flatten()
    .unwrap();
    assert_eq!(
        with_empty.vendor_configuration,
        marathon_config::VendorConfiguration::Empty
    );
}

/// WP 2.3a integration: a whole Marathonfile binds its vendor section
/// through `bind_vendor_configuration` plugged into `bind_configuration`
/// (WP 2.2's empty-only stub is deleted).
#[test]
fn whole_marathonfile_binds_ios_and_android_vendor_sections() {
    let android = bind(
        "name: android-app\noutputDir: ./marathon\nvendorConfiguration:\n  type: Android\n  serialStrategy: AUTOMATIC\n  waitForDevicesTimeoutMillis: 15000",
        |v| bind_configuration(v, bind_vendor_configuration),
    )
    .unwrap()
    .flatten()
    .unwrap();
    assert_eq!(android.name.to_string_lossy(), "android-app");
    let marathon_config::VendorConfiguration::Android(android_vendor) = &android.vendor_configuration else {
        panic!("expected Android vendor, got {:?}", android.vendor_configuration)
    };
    assert_eq!(
        android_vendor.serial_strategy,
        marathon_config::SerialStrategy::Automatic
    );
    assert_eq!(android_vendor.wait_for_devices_timeout_millis, 15_000);
    assert_eq!(android_vendor.adb_init_timeout_millis, 30_000);

    let ios = bind(
        "name: ios-app\noutputDir: ./marathon\nvendorConfiguration:\n  type: iOS\n  hideRunnerOutput: true",
        |v| bind_configuration(v, bind_vendor_configuration),
    )
    .unwrap()
    .flatten()
    .unwrap();
    assert_eq!(ios.name.to_string_lossy(), "ios-app");
    let marathon_config::VendorConfiguration::Ios(ios_vendor) = &ios.vendor_configuration else {
        panic!("expected iOS vendor, got {:?}", ios.vendor_configuration)
    };
    assert!(ios_vendor.hide_runner_output);
    assert!(ios_vendor.bundle.is_none());
    assert!(matches!(
        ios_vendor.test_parser_configuration,
        marathon_config::AppleTestParserConfiguration::Nm { .. }
    ));
}

// ---- Focused schema rules -----------------------------------------------------

use marathon_config::{
    CompositionOperation, ExceptionClass, RegexValuesFileFilter, TestFilterConfiguration, ValidationError,
    bind::{bind_filtering_configuration, bind_flakiness_strategy, bind_pooling_strategy, bind_sorting_strategy},
    binder,
};

fn rvf(regex: Option<&str>) -> RegexValuesFileFilter {
    RegexValuesFileFilter {
        regex: regex.map(|p| jcompat::JavaRegex::compile(p, "test").unwrap()),
        values: None,
        file: None,
        enabled: true,
    }
}

#[test]
fn filter_validate_messages_keep_kotlin_simple_names() {
    let err = |f: TestFilterConfiguration| f.validate().unwrap_err();
    assert_eq!(
        err(TestFilterConfiguration::TestMethod(rvf(None))),
        ValidationError {
            class: ExceptionClass::Configuration,
            message: "At least one of [regex,values,file] should be specified for TestMethodFilterConfiguration".into()
        }
    );
    let mut both = rvf(Some(".*"));
    both.values = Some(vec!["x".into()]);
    assert_eq!(
        err(TestFilterConfiguration::SimpleTestname(both)).message,
        "Only one of [regex,values,file] can be specified for SimpleTestnameFilterConfiguration"
    );
    assert_eq!(
        err(TestFilterConfiguration::Fragmentation {
            index: -1,
            count: 2,
            enabled: true
        })
        .message,
        "Fragment index [-1] should be >= 0"
    );
    assert_eq!(
        err(TestFilterConfiguration::Fragmentation {
            index: 2,
            count: 2,
            enabled: true
        })
        .message,
        "Fragment index [2] should be less than count [2]"
    );
    let nested = TestFilterConfiguration::Composition {
        filters: vec![TestFilterConfiguration::Annotation(rvf(None))],
        op: CompositionOperation::Union,
        enabled: true,
    };
    assert!(err(nested).message.ends_with("for AnnotationFilterConfiguration"));
    assert!(TestFilterConfiguration::Allure { enabled: false }.validate().is_ok());
}

#[test]
fn f7_pattern_equality_is_consistent_and_full_field() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let a = TestFilterConfiguration::TestMethod(rvf(Some("a+")));
    let b = TestFilterConfiguration::TestMethod(rvf(Some("a+")));
    assert_eq!(a, b);
    let hash = |r: &jcompat::JavaRegex| {
        let mut h = DefaultHasher::new();
        r.hash(&mut h);
        h.finish()
    };
    assert_eq!(
        hash(&rvf(Some("x")).regex.unwrap()),
        hash(&rvf(Some("x")).regex.unwrap())
    );
    // S1: enabled participates, unlike the Kotlin hand-written equals.
    let mut disabled = rvf(Some("a+"));
    disabled.enabled = false;
    assert_ne!(a, TestFilterConfiguration::TestMethod(disabled));
    let c1 = bind("name: n\noutputDir: ./o\nincludeSerialRegexes: [a+]", |v| {
        bind_configuration(v, bind_vendor_configuration)
    })
    .unwrap()
    .flatten()
    .unwrap();
    let c2 = bind("name: n\noutputDir: ./o\nincludeSerialRegexes: [a+]", |v| {
        bind_configuration(v, bind_vendor_configuration)
    })
    .unwrap()
    .flatten()
    .unwrap();
    assert_eq!(c1, c2);
}

#[test]
fn analytics_debug_masks_secrets_like_kotlin_tostring() {
    let text = "type: influxdb\nurl: http://secret-host\nuser: root\npassword: hunter2\ndbName: marathon";
    let got = bind(text, bind_analytics_configuration).unwrap().flatten().unwrap();
    let debug = format!("{got:?}");
    assert!(
        !debug.contains("secret-host") && !debug.contains("hunter2") && !debug.contains("root"),
        "{debug}"
    );
    assert!(debug.contains("dbName='marathon'"), "{debug}");
    // influxdb2 masks url and token, shows organization/bucket.
    let got = bind(
        "type: influxdb2\nurl: http://secret2-host\ntoken: sekrit-token\norganization: org\nbucket: marathon",
        bind_analytics_configuration,
    )
    .unwrap()
    .flatten()
    .unwrap();
    let debug = format!("{got:?}");
    assert!(
        !debug.contains("sekrit-token") && !debug.contains("secret2-host"),
        "{debug}"
    );
    assert!(debug.contains("token='*****'"), "{debug}");
    let got = bind("type: graphite\nhost: graphite-host", bind_analytics_configuration)
        .unwrap()
        .flatten()
        .unwrap();
    // Graphite masks nothing in Kotlin (no hand-written toString / Debug).
    assert!(!format!("{got:?}").contains("*****"));
}

// R1-claude-2: null filter `values` elements drop; null elements Kotlin
// later dereferences (allowlist here) still reject with the D9 message.
#[test]
fn s4_narrowed_values_drop_nulls_allowlist_still_rejects() {
    use marathon_config::bind::bind_test_filter_configuration;
    let got = bind("{type: method, values: [~, a, ~]}", bind_test_filter_configuration)
        .unwrap()
        .flatten()
        .unwrap();
    let TestFilterConfiguration::TestMethod(f) = got else {
        panic!("method")
    };
    assert_eq!(
        f.values
            .unwrap()
            .iter()
            .map(|v| v.to_string_lossy())
            .collect::<Vec<_>>(),
        ["a"]
    );
    let e = bind("allowlist: [~]", bind_filtering_configuration).unwrap_err();
    assert_eq!(e.class.to_string_lossy(), binder::MISMATCHED);
    assert_eq!(
        e.message.as_ref().map(|m| m.to_string_lossy()).as_deref(),
        Some("D9: null element in collection")
    );
}

#[test]
fn container_time_limit_is_mismatched_not_invalid_format() {
    let e = bind(
        "{type: execution-time, percentile: 80, timeLimit: [1]}",
        bind_sorting_strategy,
    )
    .unwrap_err();
    assert_eq!(e.class.to_string_lossy(), binder::MISMATCHED);
    let e = bind(
        "{type: probability, minSuccessRate: 0.5, maxCount: 1, timeLimit: {a: 1}}",
        bind_flakiness_strategy,
    )
    .unwrap_err();
    assert_eq!(e.class.to_string_lossy(), binder::MISMATCHED);
}

#[test]
fn copied_fixtures_bind_at_section_level() {
    fn section<T>(file: &str, key: &str, f: impl FnOnce(Value<'_>) -> Result<Option<T>>) -> T {
        let text = std::fs::read_to_string(format!(
            "{}/../../fixtures/configuration/fixture/config/{file}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        bind(&text, |v| {
            let child = v.get(key)?.ok_or_else(|| v.error("test", "section"))?;
            f(child)
        })
        .unwrap()
        .flatten()
        .unwrap()
    }
    let sorting = section("sample_10.yaml", "sortingStrategy", bind_sorting_strategy);
    assert_eq!(
        sorting,
        marathon_config::SortingStrategyConfiguration::ExecutionTime {
            percentile: 80.0,
            time_limit: parse_instant("2026-10-11T23:00:00Z").unwrap(),
        }
    );
    let flakiness = section("sample_10.yaml", "flakinessStrategy", bind_flakiness_strategy);
    assert_eq!(
        flakiness,
        marathon_config::FlakinessStrategyConfiguration::Probability {
            min_success_rate: 0.7,
            max_count: 3,
            time_limit: parse_instant("2026-09-12T00:00:00Z").unwrap(),
        }
    );
    let pooling = section("sample_1.yaml", "poolingStrategy", bind_pooling_strategy);
    assert!(matches!(pooling, marathon_config::PoolingStrategyConfiguration::Combo(ref l) if l.len() == 5));
    let filtering = section("sample_1.yaml", "filteringConfiguration", bind_filtering_configuration);
    assert_eq!((filtering.allowlist.len(), filtering.blocklist.len()), (6, 3));
    filtering.validate().unwrap();
    let filtering = section("sample_9.yaml", "filteringConfiguration", bind_filtering_configuration);
    assert_eq!((filtering.allowlist.len(), filtering.blocklist.len()), (0, 1));
    let analytics = section(
        "sample_1_rp.yaml",
        "analyticsConfiguration",
        bind_analytics_configuration,
    );
    let AnalyticsConfiguration::InfluxDb(c) = analytics else {
        panic!("influx")
    };
    assert_eq!(
        c.retention_policy_configuration.name.to_string_lossy(),
        "rpMarathonTest"
    );
    assert_eq!(c.retention_policy_configuration.replication_factor, 5);
    assert!(!c.read_only);
}
