//! Port of tools/oracle/dex/DexOracle.java expectations (WP 6.4):
//! the pinned dex-test-parser 2.3.4 JAR and the reference ApkParser on
//! the reference JDK over every fixture APK, compared exactly against
//! the Rust output — ordered test names, per-annotation ordered values
//! with wrapper kinds, Java scalar rendering and array reference equality, both ApkParser
//! results, and JVM failure classes where they surface.
//!
//! Regenerate with tools/oracle/dex/dex-oracle.sh (never hand-edit the
//! JSONL). D57 overlays the encoded-surrogate byte cases from a separate native expectation file.

use jcompat::jnum;
use marathon_dex::{DecodedValue, DexError, TestMethod};
use serde_json::Value;
use std::{collections::HashMap, path::PathBuf};

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(rel)
}

fn apk_for(name: &str) -> PathBuf {
    match name {
        "android_test_1.apk" => fixture("vendor-android/android_test_1.apk"),
        "app-debug-androidTest.apk" | "app-debug.apk" | "standalone-hdpi.apk" | "base-en.apk" => {
            fixture(&format!("vendor-android/apk/{name}"))
        }
        "probe.apk"
        | "probe-inheritance.apk"
        | "abstract-class-in-second-dex.apk"
        | "test-app-debug-androidTest.apk" => fixture(&format!("dex/{name}")),
        other => fixture(&format!("dex/mutants/{other}")),
    }
}

fn rows(text: &str) -> Vec<Value> {
    text.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

/// Array ids follow DexOracle.java: backing arrays of one APK's result
/// numbered in order of first visit, outer before nested. The keys are
/// addresses of arrays the result keeps alive while it is rendered.
type ArrayIds = HashMap<usize, usize>;

fn render_value(value: &DecodedValue, ids: &mut ArrayIds) -> Value {
    let (kind, rendered): (&str, Value) = match value {
        DecodedValue::Byte(v) => ("byte", v.to_string().into()),
        DecodedValue::Short(v) => ("short", v.to_string().into()),
        DecodedValue::Char(v) => ("char", v.to_string().into()),
        DecodedValue::Int(v) => ("int", v.to_string().into()),
        DecodedValue::Long(v) => ("long", v.to_string().into()),
        DecodedValue::Float(v) => ("float", jnum::float_to_string(*v).into()),
        DecodedValue::Double(v) => ("double", jnum::double_to_string(*v).into()),
        DecodedValue::String(v) => ("string", v.to_owned().into()),
        DecodedValue::Type(v) => ("type", v.to_owned().into()),
        DecodedValue::Enum(v) => ("enum", v.to_owned().into()),
        DecodedValue::Boolean(v) => ("boolean", v.to_string().into()),
        DecodedValue::Null => ("null", "null".into()),
        DecodedValue::Array(values) => {
            let next = ids.len();
            let id = *ids.entry(values.as_ptr() as usize).or_insert(next);
            let elements = values.iter().map(|v| render_value(v, ids)).collect();
            return Value::Array(vec!["array".into(), Value::Array(elements), id.into()]);
        }
    };
    Value::Array(vec![kind.into(), rendered])
}

fn render_method(method: &TestMethod, ids: &mut ArrayIds) -> Value {
    let annotations: Vec<Value> = method
        .annotations
        .iter()
        .map(|a| {
            let values: Vec<Value> = a
                .values
                .iter()
                .map(|(k, v)| Value::Array(vec![k.to_owned().into(), render_value(v, ids)]))
                .collect();
            serde_json::json!({"name": a.name.to_owned(), "inherited": a.inherited, "values": values})
        })
        .collect();
    serde_json::json!({"testName": method.test_name.to_owned(), "annotations": annotations})
}

fn render_methods(methods: &[TestMethod]) -> Value {
    let mut ids = ArrayIds::new();
    Value::Array(methods.iter().map(|m| render_method(m, &mut ids)).collect())
}

fn render_error(err: &DexError) -> Value {
    let mut object = serde_json::json!({"class": err.jvm_class.to_owned()});
    if let Some(message) = &err.message {
        object["message"] = message.to_owned().into();
    }
    if let Some(cause) = &err.cause {
        object["cause"] = render_error(cause);
    }
    object
}

fn custom_of(row: &Value) -> Vec<String> {
    row["customAnnotations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().into())
        .collect()
}

fn check_row(row: &Value) {
    let mut row = row.clone();
    let overrides: Value = serde_json::from_str(include_str!("../../../fixtures/phase8/d57-dex-probes.json")).unwrap();
    if let Some(changes) = overrides.get(row["apk"].as_str().unwrap()).and_then(Value::as_array) {
        for change in changes {
            let value = row.pointer_mut(change["pointer"].as_str().unwrap()).unwrap();
            assert_eq!(*value, change["before"]);
            *value = change["after"].clone();
        }
    }
    let row = &row;
    let apk = row["apk"].as_str().unwrap();
    let path = apk_for(apk);
    // D26: the JVM parser rejects DEX 040, as this row keeps witnessing (N10). This mutant is `probe.apk` with only its
    // version bytes changed, so it now gives the tests of `probe.apk`. Exactly this row is treated this way.
    if apk == "probe-dex040.apk" {
        assert_eq!(row["testsError"]["class"], "com.linkedin.dex.spec.DexException");
        let probe = marathon_dex::find_test_methods(&apk_for("probe.apk"), &custom_of(row)).unwrap();
        let mutant = marathon_dex::find_test_methods(&path, &custom_of(row)).unwrap();
        assert_eq!(render_methods(&mutant), render_methods(&probe));
        return;
    }
    match marathon_dex::find_test_methods(&path, &custom_of(row)) {
        Ok(methods) => {
            assert_eq!(render_methods(&methods), row["tests"], "tests for {apk}");
        }
        Err(err) => {
            assert!(row.get("testsError").is_some(), "unexpected error for {apk}: {err}");
            // P3 (approved): JVM crash taxonomy for malformed input
            // collapses to DexException (class compared); every other
            // error, DexException itself (DEX 040) included, must match
            // exactly.
            let expected = row["testsError"]["class"].as_str().unwrap();
            let expected = if expected == "java.lang.StackOverflowError"
                || expected.starts_with("java.lang.ArrayIndexOutOfBounds")
            {
                "com.linkedin.dex.spec.DexException"
            } else {
                expected
            };
            if expected == row["testsError"]["class"] {
                assert_eq!(render_error(&err), row["testsError"], "error for {apk}");
            } else {
                assert_eq!(render_error(&err)["class"], expected, "error class for {apk}");
            }
        }
    }
    match marathon_dex::parse_instrumentation_info(&path) {
        Ok(info) => {
            assert_eq!(
                serde_json::json!({
                    "applicationPackage": info.application_package.to_owned(),
                    "instrumentationPackage": info.instrumentation_package.to_owned(),
                    "testRunnerClass": info.test_runner_class.to_owned(),
                }),
                row["instrumentationInfo"],
                "instrumentationInfo for {apk}"
            );
        }
        Err(err) => assert_eq!(
            render_error(&err),
            row["instrumentationInfoError"],
            "instrumentationInfo error for {apk}"
        ),
    }
    match marathon_dex::parse_app_package_name(&path) {
        Ok(name) => assert_eq!(
            name.to_owned(),
            row["appPackageName"].as_str().unwrap(),
            "appPackageName for {apk}"
        ),
        Err(err) => assert_eq!(
            render_error(&err),
            row["appPackageNameError"],
            "appPackageName error for {apk}"
        ),
    }
}

#[test]
fn differential_over_fixture_apks() {
    for row in rows(include_str!("../../../fixtures/oracle/dex/differential.jsonl")) {
        check_row(&row);
    }
}

#[test]
fn differential_over_probe_apks() {
    for row in rows(include_str!("../../../fixtures/oracle/dex/probes.jsonl")) {
        let apk = row["apk"].as_str().unwrap();
        if apk == "no-such-file.apk" {
            continue; // asserted separately below
        }
        check_row(&row);
    }
}

/// The supplementary-plane value written by d8 as a CESU pair reads as
/// six U+FFFD with Rust's replacement granularity (D57); the original
/// JVM probe remains the witness for the old two-character result.
#[test]
fn supplementary_string_reads_as_six_replacement_characters_d57() {
    let methods = marathon_dex::find_test_methods(&apk_for("probe.apk"), &[]).unwrap();
    let negatives = methods[0]
        .annotations
        .iter()
        .find(|a| a.name == "probe.Negatives")
        .expect("probe.Negatives");
    let Some(DecodedValue::String(supplementary)) = negatives.value("supplementary") else {
        panic!("supplementary must be a string");
    };
    assert_eq!(supplementary.encode_utf16().collect::<Vec<_>>(), [0xfffd; 6]);
}

/// Missing file: recorded by the oracle with a relative path; driven
/// with one here too (relative to a fresh, empty temp dir). The other
/// tests of this binary use absolute paths, so the cwd switch is safe. DexParser propagates FileNotFoundException
/// unwrapped; ApkParser wraps the NoSuchFileException in
/// RuntimeException("Unable to parse test app AndroidManifest.xml.").
#[test]
fn missing_apk_errors_match_jvm() {
    let text = include_str!("../../../fixtures/oracle/dex/probes.jsonl");
    let row = rows(text).into_iter().find(|r| r["apk"] == "no-such-file.apk").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();
    let tests_err = marathon_dex::find_test_methods(std::path::Path::new("no-such-file.apk"), &[]).unwrap_err();
    let info_err = marathon_dex::parse_instrumentation_info(std::path::Path::new("no-such-file.apk")).unwrap_err();
    let pkg_err = marathon_dex::parse_app_package_name(std::path::Path::new("no-such-file.apk")).unwrap_err();
    std::env::set_current_dir(previous).unwrap();
    assert_eq!(render_error(&tests_err), row["testsError"]);
    assert_eq!(render_error(&info_err), row["instrumentationInfoError"]);
    assert_eq!(render_error(&pkg_err), row["appPackageNameError"]);
}
