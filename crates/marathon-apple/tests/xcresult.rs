//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/xcresulttool/XcresulttoolTest.kt

use marathon_apple::xcresult::*;

const SAMPLE1: &str = include_str!("../../../fixtures/vendor-apple-base/fixtures/xctesttool/sample_1.json");
const SAMPLE2: &str = include_str!("../../../fixtures/vendor-apple-base/fixtures/xctesttool/sample_2.json");

#[test]
fn test_sample1() {
    let record = decode_legacy::<ActionsInvocationRecord>(SAMPLE1).unwrap().unwrap();
    assert!(!record.issues.test_failure_summaries.is_empty());
}

#[test]
fn test_sample2() {
    let record = decode_legacy::<ActionsInvocationRecord>(SAMPLE2).unwrap().unwrap();
    assert_eq!(record.issues.test_failure_summaries.len(), 0);
}

#[test]
fn real_tool_fixture_enrichment_path_and_dates() {
    for input in [
        SAMPLE1,
        SAMPLE2,
        include_str!("../../../fixtures/vendor-apple-base/fixtures/xctesttool/xcode27_failure.json"),
    ] {
        let record = decode_legacy::<ActionsInvocationRecord>(input).unwrap().unwrap();
        assert!(!record.actions.is_empty());
        for action in record.actions.iter().flatten() {
            assert!(action.started_time.0 > 0);
            assert!(action.ended_time.0 >= action.started_time.0);
            assert!(!action.run_destination.target_device_record.name.is_empty());
            if let Some(reference) = &action.action_result.tests_ref {
                assert!(!reference.id.is_empty());
            }
        }
        for failure in record.issues.test_failure_summaries.iter().flatten() {
            assert!(!failure.test_case_name.is_empty());
            assert!(!failure.message.is_empty());
            if let Some(location) = &failure.document_location_in_creating_workspace {
                assert!(location.url.starts_with("file://"));
            }
        }
    }
    for input in [
        include_str!("../../../fixtures/vendor-apple-base/fixtures/xctesttool/testplanrunsummary_1.json"),
        include_str!("../../../fixtures/vendor-apple-base/fixtures/xctesttool/xcode27_testplanrunsummaries.json"),
    ] {
        let summaries = decode_legacy::<ActionTestPlanRunSummaries>(input).unwrap().unwrap();
        let targets: Vec<_> = summaries
            .summaries
            .iter()
            .flatten()
            .flat_map(|s| s.testable_summaries.iter().flatten())
            .collect();
        assert!(!targets.is_empty());
        assert!(
            targets
                .iter()
                .all(|target| target.target_name.as_ref().is_some_and(|name| !name.is_empty()))
        );
    }
}

#[test]
fn flattened_inheritance_does_not_dispatch_on_type_metadata() {
    let input = r#"{"_type":{"_name":"ActionTestSummary"},"identifier":{"_value":"C/test"},"name":{"_value":"test"},"duration":{"_value":"bad"}}"#;
    let value = decode_legacy::<ActionTestSummaryIdentifiableObject>(input)
        .unwrap()
        .unwrap();
    assert_eq!(value.name, Some(String::from("test")));
    assert_eq!(value.identifier, Some(String::from("C/test")));
    assert!(value.identifier_url.is_none());
    assert_eq!(OBJECT_TYPE_COUNT, 57);
}
