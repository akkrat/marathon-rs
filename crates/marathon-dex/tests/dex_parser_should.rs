//! Port of dex-test-parser 2.3.4 parser/src/test/kotlin/com/linkedin/dex/DexParserShould.kt
//! (BSD-2-Clause). Test names are the Kotlin names in snake_case, typos
//! included. The APK is fixtures/dex/test-app-debug-androidTest.apk,
//! built from the same tag's test-app/ (fixtures/PROVENANCE.md); every
//! assertion was first verified against the JVM parser's dump of that
//! APK (fixtures/oracle/dex/differential.jsonl). findTestNames calls map
//! over testName (WP 6.4 P6).

use marathon_dex::{DecodedValue as DV, TestAnnotation, TestMethod, parser::DecodedValue};
use std::path::PathBuf;

fn apk_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dex/test-app-debug-androidTest.apk")
}

fn js(text: &str) -> String {
    text.into()
}

fn custom() -> Vec<String> {
    vec![js("")] // listOf("") in every upstream test
}

fn find() -> Vec<TestMethod> {
    marathon_dex::find_test_methods(&apk_path(), &custom()).unwrap()
}

fn name(method: &TestMethod) -> String {
    method.test_name.to_owned()
}

fn with_annotation<'a>(methods: &'a [TestMethod], needle: &str) -> Vec<&'a TestMethod> {
    methods
        .iter()
        .filter(|m| m.annotations.iter().any(|a| a.name.to_owned().contains(needle)))
        .collect()
}

fn annotations_named<'a>(method: &'a TestMethod, needle: &str) -> Vec<&'a TestAnnotation> {
    method
        .annotations
        .iter()
        .filter(|a| a.name.to_owned().contains(needle))
        .collect()
}

fn basic(test_name: &str) -> TestMethod {
    let methods = find();
    let found: Vec<&TestMethod> = with_annotation(&methods, "TestValueAnnotation")
        .into_iter()
        .filter(|m| name(m) == test_name)
        .collect();
    assert_eq!(found.len(), 1);
    assert_eq!(name(found[0]), test_name);
    found[0].clone()
}

fn get_basic_junit4_test_method() -> TestMethod {
    basic("com.linkedin.parser.test.junit4.java.BasicJUnit4#basicJUnit4")
}

fn get_second_basic_junit4_test_method() -> TestMethod {
    basic("com.linkedin.parser.test.junit4.java.BasicJUnit4#basicJUnit4Second")
}

fn assert_matches_string(value: Option<&DV>, expected: &str) {
    match value {
        Some(DV::String(v) | DV::Enum(v) | DV::Type(v)) => assert_eq!(v.to_owned(), expected),
        other => panic!("Value was not a string type: {other:?}"),
    }
}

fn assert_matches_int(value: Option<&DV>, expected: i32) {
    match value {
        Some(DV::Int(v)) => assert_eq!(*v, expected),
        other => panic!("Value was not an int type: {other:?}"),
    }
}

fn assert_matches_bool(value: Option<&DV>, expected: bool) {
    match value {
        Some(DV::Boolean(v)) => assert_eq!(*v, expected),
        other => panic!("Value was not a boolean type: {other:?}"),
    }
}

fn assert_matches_long(value: Option<&DV>, expected: i64) {
    match value {
        Some(DV::Long(v)) => assert_eq!(*v, expected),
        other => panic!("Value was not a long type: {other:?}"),
    }
}

fn assert_matches_float(value: Option<&DV>, expected: f32) {
    match value {
        Some(DV::Float(v)) => assert_eq!(*v, expected),
        other => panic!("Value was not a float type: {other:?}"),
    }
}

fn assert_matches_double(value: Option<&DV>, expected: f64) {
    match value {
        Some(DV::Double(v)) => assert_eq!(*v, expected),
        other => panic!("Value was not a double type: {other:?}"),
    }
}

fn assert_matches_byte(value: Option<&DV>, expected: i8) {
    match value {
        Some(DV::Byte(v)) => assert_eq!(*v, expected),
        other => panic!("Value was not a byte type: {other:?}"),
    }
}

fn assert_matches_char(value: Option<&DV>, expected: char) {
    match value {
        Some(DV::Char(v)) => assert_eq!(*v, expected as u16),
        other => panic!("Value was not a char type: {other:?}"),
    }
}

fn assert_matches_short(value: Option<&DV>, expected: i16) {
    match value {
        Some(DV::Short(v)) => assert_eq!(*v, expected),
        other => panic!("Value was not a short type: {other:?}"),
    }
}

fn assert_matches_strings(value: Option<&DV>, expected: &[&str]) {
    match value {
        Some(DV::Array(values)) => {
            let actual: Vec<Option<String>> = values
                .iter()
                .map(|v| match v {
                    DV::Type(s) => Some(s.to_owned()),
                    _ => None,
                })
                .collect();
            let expected: Vec<Option<String>> = expected.iter().map(|s| Some(s.to_string())).collect();
            assert_eq!(actual, expected);
        }
        other => panic!("Value was not an array value: {other:?}"),
    }
}

#[test]
fn parse_correct_number_of_test_methods() {
    let test_methods: Vec<String> = find().iter().map(name).collect();
    assert_eq!(test_methods.len(), 23);
}

#[test]
fn parse_method_with_multiple_method_annotations() {
    let methods = find();
    let test_methods = with_annotation(&methods, "TestValueAnnotation");
    assert_eq!(test_methods.len(), 5);

    let method = test_methods[1];
    assert_eq!(
        name(method),
        "com.linkedin.parser.test.junit4.java.BasicJUnit4#basicJUnit4"
    );
    // TestValueAnnotation at the class level, Test annotation at the method level, and TestValueAnnotation at the method level
    assert_eq!(method.annotations.len(), 3);
}

#[test]
fn parse_method_with_childclass_annotation() {
    let methods = find();
    let test_methods = with_annotation(&methods, "TestValueAnnotation");
    let method = test_methods[0];
    assert_eq!(
        name(method),
        "com.linkedin.parser.test.junit4.java.BasicJUnit4#abstractTest"
    );
    assert_eq!(
        method.annotations[2].value("stringValue"),
        Some(&DecodedValue::String(js("Hello world!")))
    );
}

#[test]
fn parse_inherited_method_annotation() {
    let methods = find();
    let test_methods = with_annotation(&methods, "InheritedAnnotation");
    let method = test_methods[0];
    assert_eq!(
        name(method),
        "com.linkedin.parser.test.junit4.java.BasicJUnit4#concreteTest"
    );
    assert_eq!(
        method.annotations[2].value("stringValue"),
        Some(&DecodedValue::String(js("Hello world!")))
    );
}

#[test]
fn pars_non_inherited_method_annotation() {
    let methods = find();
    let test_methods = with_annotation(&methods, "InheritedAnnotation");
    let method = test_methods
        .iter()
        .find(|m| name(m).contains("BasicJUnit4#concreteTest"))
        .expect("method");
    assert_eq!(
        name(method),
        "com.linkedin.parser.test.junit4.java.BasicJUnit4#concreteTest"
    );
    assert!(
        !method
            .annotations
            .iter()
            .any(|a| a.name.to_owned().contains("NonInheritedAnnotation"))
    );
}

#[test]
fn parse_inherited_class_annotation() {
    let methods = find();
    let test_methods = with_annotation(&methods, "InheritedAnnotation");
    let method = test_methods[0];
    assert_eq!(
        name(method),
        "com.linkedin.parser.test.junit4.java.BasicJUnit4#concreteTest"
    );
    assert!(
        method
            .annotations
            .iter()
            .any(|a| a.name == js("com.linkedin.parser.test.junit4.java.InheritedClassAnnotation"))
    );
}

#[test]
fn parse_string_annotation_values() {
    let method = get_basic_junit4_test_method();
    let value_annotations = annotations_named(&method, "TestValueAnnotation");

    let class_annotation = value_annotations[0];
    let string_value = class_annotation.value("stringValue");
    assert!(string_value.is_some());
    assert_matches_string(string_value, "Hello world!");

    let method_annotation = value_annotations[1];
    assert_matches_string(method_annotation.value("stringValue"), "On a method");
}

#[test]
fn parse_int_annotation_values() {
    let method = get_basic_junit4_test_method();
    let value_annotations = annotations_named(&method, "TestValueAnnotation");
    assert_matches_int(value_annotations[1].value("intValue"), 12345);
}

#[test]
fn parse_bool_annotation_values() {
    let method = get_basic_junit4_test_method();
    let value_annotations = annotations_named(&method, "TestValueAnnotation");
    assert_matches_bool(value_annotations[1].value("boolValue"), true);
}

#[test]
fn parse_long_annotation_values() {
    let method = get_basic_junit4_test_method();
    let value_annotations = annotations_named(&method, "TestValueAnnotation");
    assert_matches_long(value_annotations[1].value("longValue"), 56789);
}

#[test]
fn parse_float_annotation_values() {
    let method = get_second_basic_junit4_test_method();
    let value_annotations = annotations_named(&method, "TestValueAnnotation");
    assert_matches_float(value_annotations[1].value("floatValue"), 0.25);
}

#[test]
fn parse_double_annotation_values() {
    let method = get_second_basic_junit4_test_method();
    let value_annotations = annotations_named(&method, "TestValueAnnotation");
    assert_matches_double(value_annotations[1].value("doubleValue"), 0.5);
}

#[test]
fn parse_byte_annotation_values() {
    let method = get_second_basic_junit4_test_method();
    let value_annotations = annotations_named(&method, "TestValueAnnotation");
    assert_matches_byte(value_annotations[1].value("byteValue"), 0x0f);
}

#[test]
fn parse_float_max_values_in_double_fields() {
    let method = get_second_basic_junit4_test_method();
    let value_annotations = annotations_named(&method, "FloatRange");
    let method_annotation = value_annotations[0];
    assert_matches_double(method_annotation.value("from"), 0.0);
    assert_matches_double(method_annotation.value("to"), f64::from(f32::MAX));
}

#[test]
fn parse_char_annotation_values() {
    let method = get_second_basic_junit4_test_method();
    let value_annotations = annotations_named(&method, "TestValueAnnotation");
    assert_matches_char(value_annotations[1].value("charValue"), '?');
}

#[test]
fn parse_short_annotation_values() {
    let method = get_second_basic_junit4_test_method();
    let value_annotations = annotations_named(&method, "TestValueAnnotation");
    assert_matches_short(value_annotations[1].value("shortValue"), 3);
}

#[test]
fn parse_class_array_annotationn_values() {
    let method = get_second_basic_junit4_test_method();
    let value_annotations = annotations_named(&method, "TestValueAnnotation");
    // We have to use a string as opposed to a class reference in this assertion, since the way
    // that its actually stored on disk is their special class format and not what class.name will give
    assert_matches_strings(
        value_annotations[1].value("arrayTypeValue"),
        &["Ljava/util/function/Function;", "Ljava/lang/Integer;"],
    );
}

#[test]
fn parse_multiple_values_in_a_single_annotation() {
    let method = get_basic_junit4_test_method();
    let value_annotations = annotations_named(&method, "TestValueAnnotation");
    let method_annotation = value_annotations[1];
    assert_matches_string(method_annotation.value("stringValue"), "On a method");
    assert_matches_int(method_annotation.value("intValue"), 12345);
    assert_matches_bool(method_annotation.value("boolValue"), true);
    assert_matches_long(method_annotation.value("longValue"), 56789);
}

#[test]
fn parse_enum_annotation() {
    let method = get_second_basic_junit4_test_method();
    let value_annotations = annotations_named(&method, "TestValueAnnotation");
    assert_matches_string(value_annotations[1].value("enumValue"), "FAIL");
}

#[test]
fn parse_type_annotation() {
    let method = get_second_basic_junit4_test_method();
    let value_annotations = annotations_named(&method, "TestValueAnnotation");
    assert_matches_string(value_annotations[1].value("typeValue"), "Lorg/junit/Test;");
}

#[test]
fn parse_abstract_class_test_method_correctly() {
    let methods = find();
    let found: Vec<&TestMethod> = methods
        .iter()
        .filter(|m| name(m) == "com.linkedin.parser.test.junit4.java.ConcreteTest#abstractTest")
        .collect();
    assert_eq!(found.len(), 1);
    let method = found[0];
    assert_eq!(
        name(method),
        "com.linkedin.parser.test.junit4.java.ConcreteTest#abstractTest"
    );
    assert_eq!(method.annotations.len(), 2);
    assert!(method.annotations.iter().any(|a| a.name == js("org.junit.Test")));
}
