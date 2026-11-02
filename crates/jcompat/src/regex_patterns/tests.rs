//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/logparser/parser/TestRunProgressParser.kt (regex operation and pattern tests).

use super::*;
use serde_json::{Value, json};

fn captures_json(input: &str, c: regex::Captures<'_>) -> Value {
    let m = c.get(0).unwrap();
    json!({"range": [input[..m.start()].encode_utf16().count(), input[..m.end()].encode_utf16().count()], "groups": c.iter().map(|m| m.map(|m| m.as_str())).collect::<Vec<_>>()})
}

#[test]
fn fixed_patterns_jvm_corpus() {
    let patterns: [&JavaRegex; 23] = [
        &TEST_CASE_STARTED,
        &TEST_CASE_STARTED,
        &TEST_CASE_FINISHED,
        &TEST_CASE_FINISHED,
        &FAILING_TEST,
        &FAILING_TEST,
        &TIMEOUT_TEST,
        &TIMEOUT_TEST,
        &SESSION_RESULTS_PATH,
        &DIAGNOSTIC_LOGS_PATH,
        &ESCAPE,
        &FILTER_COMMENT,
        &GRAPHITE_WHITESPACE,
        &PLAIN_PATH,
        &RSYNC_FIRST_LINE,
        &UDID,
        &BUNDLE_NAME,
        &METRIC_ANNOTATION_V2,
        &PARAMETERIZED_METHOD,
        &LOGCAT_HEADER,
        &NM_SWIFT_TEST,
        &NM_TEXT_SYMBOL,
        &NM_OBJC_TEST,
    ];
    let mut count = 0;
    for line in include_str!("../../../../fixtures/oracle/regex-corpus.jsonl")
        .lines()
        .skip(1)
    {
        let row: Value = serde_json::from_str(line).unwrap();
        if row["family"] != "hardcoded" {
            continue;
        }
        let index = row["id"]
            .as_str()
            .unwrap()
            .strip_prefix("hardcoded-")
            .unwrap()
            .parse::<usize>()
            .unwrap()
            - 1;
        count += 1;
        for (input, expected) in row["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .zip(row["expected"].as_array().unwrap())
        {
            let input = input.as_str().unwrap();
            let actual = if index == 23 {
                json!(default_test_class_matches(input))
            } else if index == 11 {
                json!(strip_filter_comment(input))
            } else {
                let pattern = patterns[index];
                match row["op"].as_str().unwrap() {
                    "matches" => json!(pattern.matches(input)),
                    "find" => pattern
                        .find(input)
                        .map(|c| captures_json(input, c))
                        .unwrap_or(Value::Null),
                    "findAll" => Value::Array(pattern.find_all(input).map(|c| captures_json(input, c)).collect()),
                    "replace" => json!(pattern.replace(input, row["replacement"].as_str().unwrap()).unwrap()),
                    _ => unreachable!(),
                }
            };
            assert_eq!(&actual, expected, "{} {input:?}", row["id"]);
        }
    }
    assert_eq!(count, 24);
}

#[test]
fn apple_fixture_lines() {
    let cases = [
        (
            include_str!("../../../../fixtures/vendor-apple-base/fixtures/test_output/success_0.log.input"),
            &*TEST_CASE_STARTED,
        ),
        (
            include_str!("../../../../fixtures/vendor-apple-base/fixtures/test_output/success_0.log.input"),
            &*TEST_CASE_FINISHED,
        ),
        (
            include_str!("../../../../fixtures/vendor-apple-base/fixtures/test_output/failure_0.log.input"),
            &*FAILING_TEST,
        ),
        (
            include_str!("../../../../fixtures/vendor-apple-base/fixtures/test_output/timeout_0.log.input"),
            &*TIMEOUT_TEST,
        ),
    ];
    for (fixture, pattern) in cases {
        let matching: Vec<_> = fixture.lines().filter(|line| pattern.matches(line)).collect();
        assert!(!matching.is_empty(), "{pattern}");
        for line in matching {
            let captures = pattern.find(line).unwrap();
            assert_eq!(captures.get(0).unwrap().as_str(), line);
            assert!(!captures.get(1).unwrap().is_empty());
        }
    }
}

#[test]
fn logcat_parts_and_fixture_rejection() {
    assert!(LOGCAT_DATE_TIME.matches("08-18 16:39:11.760"));
    assert!(LOGCAT_PROCESS_ID.matches("2977"));
    assert!(!LOGCAT_PROCESS_ID.matches("١"));
    assert!(LOGCAT_THREAD_ID.matches("0x1a2b"));
    assert!(!LOGCAT_THREAD_ID.matches("é"));
    for priority in ["V", "D", "I", "W", "E", "A", "F"] {
        assert!(LOGCAT_PRIORITY.matches(priority));
    }
    assert!(LOGCAT_TAG.matches("Tag With Spaces"));
    assert!(!LOGCAT_TAG.matches("Tag\r"));
    // This fake-adb fixture uses a year prefix. Kotlin rejects it too: do not
    // silently broaden the actual long-header grammar to accept the fake data.
    let fixture = include_str!("../../../../fixtures/fake-adb/android-two-devices/script/logcat.txt");
    assert!(
        fixture
            .lines()
            .filter(|l| l.starts_with('['))
            .all(|l| !LOGCAT_HEADER.matches(l))
    );
}

#[test]
fn fixed_pattern_edge_cases() {
    assert!(UDID.matches("")); // known Kotlin bug A4
    assert!(!default_test_class_matches("FooAbstractTest"));
    assert!(default_test_class_matches("TestsTestsss"));
    assert!(default_test_class_matches("Test"));
    assert!(!default_test_class_matches("Test\n"));
    assert!(!default_test_class_matches("Foo\u{85}Test"));
    assert_eq!(strip_filter_comment("a #comment\r\n"), "a\r\n");
    assert_eq!(strip_filter_comment("a #comment\u{2028}"), "a\u{2028}");
    assert_eq!(strip_filter_comment("a #comment\nb"), "a #comment\nb");
    assert_eq!(
        find_artifact(&SESSION_RESULTS_PATH, " /a.xcresult\u{2028}"),
        Some(" /a.xcresult")
    );
    assert!(!PARAMETERIZED_METHOD.matches("a\r[1]"));
}
