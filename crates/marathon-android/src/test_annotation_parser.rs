//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/event/TestAnnotationParser.kt
//!
//! All length arithmetic is in UTF-16 code units, as in Kotlin. An overrun
//! length stops extraction and returns what was collected (P4, approved
//! deviation; Kotlin throws `StringIndexOutOfBoundsException` there).

use jcompat::{jnum, jstring, regex_patterns::METRIC_ANNOTATION_V2};
use marathon_adb::testrunner::MetricMap;
use marathon_core::model::{MetaMap, MetaProperty, MetaValue};

const METRIC_ANNOTATION_V2_KEY: &str = "com.malinskiy.adam.junit4.android.listener.TestAnnotationProducer.v2";
const METRIC_ANNOTATION_V3_KEY: &str = "com.malinskiy.adam.junit4.android.listener.TestAnnotationProducer.v3";
const METRIC_ANNOTATION_V4_KEY: &str = "com.malinskiy.adam.junit4.android.listener.TestAnnotationProducer.v4";

#[derive(Clone, Copy, Debug, Default)]
pub struct TestAnnotationParser;

impl TestAnnotationParser {
    pub fn new() -> Self {
        Self
    }

    /// Kotlin `extractAnnotations(TestEnded)`; the Rust event model is an
    /// enum, so this takes the `TestEnded` variant's metrics directly.
    /// Version selection is by key presence, newest first; a present but
    /// malformed newer version never falls back to an older one.
    pub fn extract_annotations(&self, metrics: &MetricMap) -> Vec<MetaProperty> {
        if let Some(v4) = metrics.get_str(METRIC_ANNOTATION_V4_KEY) {
            extract_v4(v4)
        } else if let Some(v3) = metrics.get_str(METRIC_ANNOTATION_V3_KEY) {
            extract_v3(v3)
        } else if let Some(v2) = metrics.get_str(METRIC_ANNOTATION_V2_KEY) {
            extract_v2(v2)
        } else {
            Vec::new()
        }
    }
}

fn extract_v4(value: &str) -> Vec<MetaProperty> {
    let mut result = Vec::new();
    for annotation in length_framed_annotations(remove_surrounding(
        &value.encode_utf16().collect::<Vec<_>>(),
        b'[',
        b']',
    )) {
        let Some(split) = annotation.iter().position(|&c| c == u16::from(b'(')) else {
            continue;
        };
        result.push(meta_property(
            String::from_utf16_lossy(&annotation[..split]),
            length_framed_parameters(&annotation[split + 1..]),
        ));
    }
    result
}

fn extract_v3(value: &str) -> Vec<MetaProperty> {
    serialized_annotations(value)
        .into_iter()
        .map(|annotation| {
            let index = annotation
                .iter()
                .position(|&c| c == u16::from(b'('))
                .unwrap_or(annotation.len());
            let parameters = remove_surrounding(&annotation[index..], b'(', b')');
            meta_property(
                String::from_utf16_lossy(&annotation[..index]),
                length_framed_parameters(parameters),
            )
        })
        .collect()
}

fn extract_v2(value: &str) -> Vec<MetaProperty> {
    serialized_annotations(value)
        .into_iter()
        .map(|annotation| {
            let index = annotation
                .iter()
                .position(|&c| c == u16::from(b'('))
                .unwrap_or(annotation.len());
            let parameters = remove_surrounding(&annotation[index..], b'(', b')');
            let values = split_unit(parameters, u16::from(b':'))
                .into_iter()
                .filter_map(|parameter| {
                    let split = split_unit(parameter, u16::from(b'='));
                    (split.len() == 2).then(|| (String::from_utf16_lossy(split[0]), String::from_utf16_lossy(split[1])))
                })
                .collect();
            meta_property(String::from_utf16_lossy(&annotation[..index]), values)
        })
        .collect()
}

/// v3/v2 annotation discovery: `removeSurrounding("[", "]")`, then the N6
/// regex over the result, each match trimmed, empties dropped.
fn serialized_annotations(value: &str) -> Vec<Vec<u16>> {
    let body = value
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(value);
    METRIC_ANNOTATION_V2
        .find_all(body)
        .filter_map(|captures| captures.get(0))
        .map(|m| jstring::trim(m.as_str()).encode_utf16().collect::<Vec<_>>())
        .filter(|m| !m.is_empty())
        .collect()
}

/// v4 annotation sequence: `<len>L<annotation>` repeated, one `,` and leading
/// whitespace dropped after each. Stops silently at the first bad length.
fn length_framed_annotations(mut body: &[u16]) -> Vec<Vec<u16>> {
    let mut annotations = Vec::new();
    while let Some((length, rest)) = read_length_frame(body) {
        let Some(annotation) = rest.get(..length) else { break };
        annotations.push(annotation.to_vec());
        body = &rest[length..];
        body = body.strip_prefix(&[u16::from(b',')]).unwrap_or(body);
        body = &body[body
            .iter()
            .position(|&u| !char::from_u32(u32::from(u)).is_some_and(jstring::is_whitespace))
            .unwrap_or(body.len())..];
    }
    annotations
}

/// The v4/v3 parameter framing. A parameter survives only when it is longer
/// than one unit and contains `=`; it is split at the first `=`.
fn length_framed_parameters(mut rest: &[u16]) -> Vec<(String, String)> {
    let mut values = Vec::new();
    while let Some((length, after)) = read_length_frame(rest) {
        let Some(parameter) = after.get(..length) else { break };
        if parameter.len() > 1
            && let Some(equal) = parameter.iter().position(|&c| c == u16::from(b'='))
        {
            values.push((
                String::from_utf16_lossy(&parameter[..equal]),
                String::from_utf16_lossy(&parameter[equal + 1..]),
            ));
        }
        rest = &after[length..];
    }
    values
}

/// Parses the `<len>L` header at the start of `units`: the digits up to the
/// first `L` via `toIntOrNull`. Negative lengths are malformed (Kotlin's
/// substring throws on them) and stop extraction like any bad length (P4).
fn read_length_frame(units: &[u16]) -> Option<(usize, &[u16])> {
    let split = units.iter().position(|&c| c == u16::from(b'L'))?;
    let length = String::from_utf16(&units[..split])
        .ok()
        .and_then(|s| jnum::parse_int(&s))?;
    usize::try_from(length).ok().map(|length| (length, &units[split + 1..]))
}

/// Kotlin `removeSurrounding(prefix, suffix)` for single-unit affixes.
fn remove_surrounding(units: &[u16], prefix: u8, suffix: u8) -> &[u16] {
    if units.len() >= 2 && units[0] == u16::from(prefix) && units[units.len() - 1] == u16::from(suffix) {
        &units[1..units.len() - 1]
    } else {
        units
    }
}

/// Kotlin `split(Char)`: every piece kept, including trailing empties.
fn split_unit(units: &[u16], delimiter: u16) -> Vec<&[u16]> {
    let mut pieces = Vec::new();
    let mut start = 0;
    for (i, &c) in units.iter().enumerate() {
        if c == delimiter {
            pieces.push(&units[start..i]);
            start = i + 1;
        }
    }
    pieces.push(&units[start..]);
    pieces
}

/// Kotlin `values.toMap()` into a `MetaProperty`: last value wins in place.
fn meta_property(name: String, values: Vec<(String, String)>) -> MetaProperty {
    let mut map = MetaMap::default();
    for (key, value) in values {
        map.insert(key, MetaValue::String(value));
    }
    MetaProperty { name, values: map }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Port of marathon's TestAnnotationParserTest (WP 6.3 parity rows), then
    // new tests for the untested-upstream v3 path and the brief's edge rules.

    fn metrics(key: &str, value: &str) -> MetricMap {
        let mut map = MetricMap::new();
        map.insert(key.into(), value.into());
        map
    }

    fn property(name: &str, values: &[(&str, &str)]) -> MetaProperty {
        meta_property(
            name.into(),
            values.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect(),
        )
    }

    fn extract(key: &str, value: &str) -> Vec<MetaProperty> {
        TestAnnotationParser::new().extract_annotations(&metrics(key, value))
    }

    #[test]
    fn test_extraction_v2_1() {
        assert_eq!(extract(METRIC_ANNOTATION_V2_KEY, "[]"), Vec::new());
    }

    #[test]
    fn test_extraction_v2_2() {
        assert_eq!(
            extract(
                METRIC_ANNOTATION_V2_KEY,
                "[io.qameta.allure.kotlin.Description(useJavaDoc=false:value=Some description, but with quotes,), io.qameta.allure.kotlin.TmsLink(value=TEST-666)]"
            ),
            vec![
                property(
                    "io.qameta.allure.kotlin.Description",
                    &[("useJavaDoc", "false"), ("value", "Some description, but with quotes,")]
                ),
                property("io.qameta.allure.kotlin.TmsLink", &[("value", "TEST-666")]),
            ]
        );
    }

    #[test]
    fn test_extraction_v4_1() {
        assert_eq!(
            extract(
                METRIC_ANNOTATION_V4_KEY,
                "[64Lorg.junit.Test(34Lexpected=class org.junit.Test$None9Ltimeout=0)]"
            ),
            vec![property(
                "org.junit.Test",
                &[("expected", "class org.junit.Test$None"), ("timeout", "0")]
            )]
        );
    }

    #[test]
    fn test_extraction_v4_2() {
        assert_eq!(
            extract(
                METRIC_ANNOTATION_V4_KEY,
                "[98Lorg.junit.Test(68Lvalue=something is\nbound to happen (when you're dealing with parsin)9Ltimeout=0)]"
            ),
            vec![property(
                "org.junit.Test",
                &[
                    (
                        "value",
                        "something is\nbound to happen (when you're dealing with parsin)"
                    ),
                    ("timeout", "0")
                ]
            )]
        );
    }

    #[test]
    fn test_extraction_v4_3() {
        assert_eq!(
            extract(
                METRIC_ANNOTATION_V4_KEY,
                "[64Lorg.junit.Test(34Lexpected=class org.junit.Test$None9Ltimeout=0), 46Lio.qameta.allure.kotlin.Epic(13Lvalue=General)]"
            ),
            vec![
                property(
                    "org.junit.Test",
                    &[("expected", "class org.junit.Test$None"), ("timeout", "0")]
                ),
                property("io.qameta.allure.kotlin.Epic", &[("value", "General")]),
            ]
        );
    }

    // New tests below; none of these inputs exist upstream.

    #[test]
    fn v3_uses_the_v2_regex_with_v4_parameter_framing() {
        // Pins TestAnnotationParser.kt:55-80 (no upstream v3 test exists).
        assert_eq!(
            extract(
                METRIC_ANNOTATION_V3_KEY,
                "[io.qameta.allure.kotlin.Epic(13Lvalue=General)]"
            ),
            vec![property("io.qameta.allure.kotlin.Epic", &[("value", "General")])]
        );
        assert_eq!(
            extract(
                METRIC_ANNOTATION_V3_KEY,
                "[org.junit.Test(9Ltimeout=0), io.qameta.allure.kotlin.Epic(13Lvalue=General)]"
            ),
            vec![
                property("org.junit.Test", &[("timeout", "0")]),
                property("io.qameta.allure.kotlin.Epic", &[("value", "General")]),
            ]
        );
    }

    #[test]
    fn newest_version_wins_and_never_falls_back() {
        let mut map = metrics(METRIC_ANNOTATION_V2_KEY, "[a.B(x=1)]");
        map.insert(METRIC_ANNOTATION_V4_KEY.into(), "garbage".into());
        // v4 is present but malformed: no fallback to the parseable v2.
        assert_eq!(TestAnnotationParser::new().extract_annotations(&map), Vec::new());

        let mut map = metrics(METRIC_ANNOTATION_V2_KEY, "[a.B(x=1)]");
        map.insert(METRIC_ANNOTATION_V3_KEY.into(), "[a.B(3Lx=1)]".into());
        assert_eq!(
            TestAnnotationParser::new().extract_annotations(&map),
            vec![property("a.B", &[("x", "1")])]
        );
    }

    #[test]
    fn no_annotation_metrics_extract_nothing() {
        assert_eq!(
            TestAnnotationParser::new().extract_annotations(&MetricMap::new()),
            Vec::new()
        );
    }

    #[test]
    fn v4_lengths_count_utf16_units_not_chars() {
        let parameter = "value=𝔘x"; // U+1D518 is a surrogate pair: 9 UTF-16 units
        let annotation = format!("org.junit.Test({}L{parameter})", parameter.encode_utf16().count());
        let value = format!("[{}L{annotation}]", annotation.encode_utf16().count());
        assert_eq!(
            extract(METRIC_ANNOTATION_V4_KEY, &value),
            vec![property("org.junit.Test", &[("value", "𝔘x")])]
        );
    }

    #[test]
    fn v4_length_overrun_returns_what_was_collected() {
        // P4: Kotlin throws on the second annotation's overrun length.
        let value = "[46Lio.qameta.allure.kotlin.Epic(13Lvalue=General), 999Lorg.junit.Test()]";
        assert_eq!(
            extract(METRIC_ANNOTATION_V4_KEY, value),
            vec![property("io.qameta.allure.kotlin.Epic", &[("value", "General")])]
        );
        // A negative length is malformed the same way.
        assert_eq!(extract(METRIC_ANNOTATION_V4_KEY, "[-5Lorg.junit.Test()]"), Vec::new());
    }

    #[test]
    fn v4_annotation_without_parenthesis_is_skipped() {
        // "justAName" is 9 units and carries no '(' (TestAnnotationParser.kt:30-32).
        assert_eq!(extract(METRIC_ANNOTATION_V4_KEY, "[9LjustAName]"), Vec::new());
    }

    #[test]
    fn duplicate_parameter_keys_keep_the_last_value() {
        // Kotlin values.toMap(): the second "x" replaces the first in place.
        let value = "[15La.B(3Lx=13Lx=2)]";
        assert_eq!(
            extract(METRIC_ANNOTATION_V4_KEY, value),
            vec![property("a.B", &[("x", "2")])]
        );
    }

    #[test]
    fn v2_parameter_with_equals_in_value_is_dropped() {
        // split("=") must yield exactly 2 parts (TestAnnotationParser.kt:92-99).
        assert_eq!(
            extract(METRIC_ANNOTATION_V2_KEY, "[a.B(expr=x=1:ok=2)]"),
            vec![property("a.B", &[("ok", "2")])]
        );
    }

    #[test]
    fn v2_regex_is_not_balanced() {
        // N6 (docs/known-kotlin-bugs.md): a(b(c)) extracts "a(b(c)", so the
        // parameter text is "b(c" and nothing survives as a parameter.
        assert_eq!(extract(METRIC_ANNOTATION_V2_KEY, "[a(b(c))]"), vec![property("a", &[])]);
        assert_eq!(extract(METRIC_ANNOTATION_V3_KEY, "[a(b(c))]"), vec![property("a", &[])]);
    }

    #[test]
    fn v2_regex_non_ascii_name_extracts_nothing() {
        // N6: default Java \p{Graph} is ASCII, so ж(x=1) has no match.
        assert_eq!(extract(METRIC_ANNOTATION_V2_KEY, "[ж(x=1)]"), Vec::new());
    }
}
