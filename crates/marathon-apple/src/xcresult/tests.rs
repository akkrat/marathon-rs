//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/xcresulttool/XcresulttoolTest.kt

use super::{
    decoder::{Decode, Fields, JsonNode, Path},
    *,
};
use jcompat::{jackson::JavaDate, jnum};
use serde_json::value::RawValue;
use std::collections::BTreeMap;

pub(super) trait Project {
    fn project(&self) -> JsonNode;
}
impl Project for String {
    fn project(&self) -> JsonNode {
        JsonNode::String(self.clone())
    }
}
impl Project for i32 {
    fn project(&self) -> JsonNode {
        JsonNode::Number(self.to_string())
    }
}
impl Project for f64 {
    fn project(&self) -> JsonNode {
        let text = jnum::double_to_string(*self);
        if self.is_finite() {
            JsonNode::Number(text)
        } else {
            JsonNode::String(text)
        }
    }
}
impl Project for bool {
    fn project(&self) -> JsonNode {
        JsonNode::Bool(*self)
    }
}
impl Project for JavaDate {
    fn project(&self) -> JsonNode {
        JsonNode::Number(self.0.to_string())
    }
}
impl<T: Project> Project for Option<T> {
    fn project(&self) -> JsonNode {
        self.as_ref().map_or(JsonNode::Null, Project::project)
    }
}
impl<T: Project> Project for Vec<T> {
    fn project(&self) -> JsonNode {
        JsonNode::Array(self.iter().map(Project::project).collect())
    }
}
impl<T: Project> Project for Box<T> {
    fn project(&self) -> JsonNode {
        self.as_ref().project()
    }
}

#[derive(Debug)]
struct OracleNullableLists {
    strings: Option<Vec<Option<String>>>,
    objects: Option<Vec<Option<Reference>>>,
}
impl Decode for OracleNullableLists {
    fn decode(node: &JsonNode, path: &Path) -> Result<Option<Self>, XcresultDecodeError> {
        if node.is_null() {
            return Ok(None);
        }
        let fields = Fields::new(node, path)?;
        Ok(Some(Self {
            strings: fields.optional_list("strings")?,
            objects: fields.optional_list("objects")?,
        }))
    }
}
impl Project for OracleNullableLists {
    fn project(&self) -> JsonNode {
        JsonNode::object([("strings", self.strings.project()), ("objects", self.objects.project())])
    }
}

fn project<T: LegacyDecode + Project>(input: &str) -> Result<JsonNode, XcresultDecodeError> {
    decode_legacy::<T>(input).map(|value| value.project())
}

#[test]
fn measured_apple_mapper_corpus() {
    let mut cases = 0;
    for line in include_str!("../../../../fixtures/oracle/xcresult/corpus.jsonl")
        .lines()
        .skip(1)
    {
        let row: BTreeMap<String, Box<RawValue>> = serde_json::from_str(line).unwrap();
        let id: String = serde_json::from_str(row["id"].get()).unwrap();
        let target: String = serde_json::from_str(row["target"].get()).unwrap();
        let input: String = serde_json::from_str(row["input"].get()).unwrap();
        let actual = match target.as_str() {
            "ResultMetrics" => project::<ResultMetrics>(&input),
            "ObjectID" => project::<ObjectID>(&input),
            "CodeCoverageInfo" => project::<CodeCoverageInfo>(&input),
            "ConsoleLogItem" => project::<ConsoleLogItem>(&input),
            "ActionAbstractTestSummary" => project::<ActionAbstractTestSummary>(&input),
            "ActionTestPlanRunSummaries" => project::<ActionTestPlanRunSummaries>(&input),
            "ActionTestActivitySummary" => project::<ActionTestActivitySummary>(&input),
            "ActionTestPerformanceMetricSummary" => project::<ActionTestPerformanceMetricSummary>(&input),
            "OracleNullableLists" => project::<OracleNullableLists>(&input),
            "ActionTestAttachment" => project::<ActionTestAttachment>(&input),
            "ActionTestRepetitionPolicySummary" => project::<ActionTestRepetitionPolicySummary>(&input),
            "ActionTestMetadata" => project::<ActionTestMetadata>(&input),
            "ActionsInvocationRecord" => project::<ActionsInvocationRecord>(&input),
            "ActionRecord" => project::<ActionRecord>(&input),
            "Reference" => project::<Reference>(&input),
            "ActionTestSummaryIdentifiableObject" => project::<ActionTestSummaryIdentifiableObject>(&input),
            "TypeDefinition" => project::<TypeDefinition>(&input),
            _ => panic!("unknown oracle target {target}"),
        };
        if matches!(id.as_str(), "int-11" | "double-10" | "date-9" | "date-10" | "date-13") {
            assert!(row.contains_key("proposal"), "{id}");
            assert!(actual.is_err(), "approved narrowed grammar: {id}");
        } else if let Some(expected) = row.get("value") {
            assert_eq!(
                actual.unwrap_or_else(|error| panic!("{id}: {error:?}")),
                JsonNode::parse(expected).unwrap(),
                "{id}"
            );
        } else if id.starts_with("depth-") {
            // Rust rejects while parsing, before binding, so there is no Jackson reference chain.
            let error = actual.expect_err(&id);
            assert_eq!(error.kind, DecodeErrorKind::JsonSyntax, "{id}");
            assert!(
                error
                    .failure
                    .kind
                    .legacy_class()
                    .ends_with("StreamConstraintsException")
            );
        } else {
            let error = actual.expect_err(&id);
            let expected: Vec<String> = serde_json::from_str(row["path"].get()).unwrap();
            let path: Vec<String> = error
                .path
                .iter()
                .map(|part| match part {
                    DecodePathSegment::Field(name) => name.clone(),
                    DecodePathSegment::Index(index) => format!("[{index}]"),
                })
                .collect();
            assert!(path.starts_with(&expected), "{id}: {path:?} vs {expected:?}");
        }
        cases += 1;
    }
    assert!(cases >= 120);
}

#[test]
fn unpaired_surrogates_decode_lossily() {
    let value = decode_legacy::<ObjectID>(r#"{"hash":{"_value":"\ud800x\udfff"}}"#)
        .unwrap()
        .unwrap();
    assert_eq!(value.hash, "\u{fffd}x\u{fffd}");
}

#[test]
fn decode_failures_are_structured_and_resource_free() {
    assert!(decode_legacy::<ResultMetrics>("null").unwrap().is_none());
    assert_eq!(
        decode_legacy::<ResultMetrics>("{").unwrap_err().kind,
        DecodeErrorKind::JsonSyntax
    );
    let error = decode_legacy::<ActionTestPlanRunSummaries>(r#"{"summaries":{"_values":[{"testableSummaries":{"_values":[{"failureSummaries":{"_values":[{"lineNumber":{"_value":"oops"}}]}}]}}]}}"#).unwrap_err();
    assert_eq!(error.kind, DecodeErrorKind::Mapping);
    assert_eq!(
        error.path,
        [
            DecodePathSegment::Field("summaries".into()),
            DecodePathSegment::Index(0),
            DecodePathSegment::Field("testableSummaries".into()),
            DecodePathSegment::Index(0),
            DecodePathSegment::Field("failureSummaries".into()),
            DecodePathSegment::Index(0),
            DecodePathSegment::Field("lineNumber".into())
        ]
    );
    let failure: marathon_core::error::Failure = error.into();
    assert_eq!(
        failure.kind,
        marathon_error::FailureKind::Config(marathon_error::ConfigKind::InvalidFormat)
    );
}

#[test]
fn nesting_beyond_jacksons_limit_is_an_error_not_a_stack_overflow() {
    let deep = |depth: usize| "[".repeat(depth) + &"]".repeat(depth);
    for input in [deep(1001), deep(5000), "{\"u\":".repeat(5000) + "1" + &"}".repeat(5000)] {
        let error = decode_legacy::<ResultMetrics>(&input).unwrap_err();
        assert_eq!(error.kind, DecodeErrorKind::JsonSyntax);
        assert!(
            error
                .failure
                .kind
                .legacy_class()
                .ends_with("StreamConstraintsException")
        );
    }
    assert!(decode_legacy::<ResultMetrics>(&format!("{{\"u\":{}}}", deep(999))).is_ok());
}

#[test]
fn recursive_models_decode_at_the_nesting_limit_on_a_default_stack() {
    let chain = |depth: usize| "{\"supertype\":".repeat(depth - 1) + "{}" + &"}".repeat(depth - 1);
    let mut value = decode_legacy::<TypeDefinition>(&chain(1000)).unwrap().unwrap();
    let mut levels = 1;
    while let Some(supertype) = value.supertype {
        value = *supertype;
        levels += 1;
    }
    assert_eq!(levels, 1000);
    assert!(decode_legacy::<TypeDefinition>(&chain(1001)).is_err());
}

#[test]
fn approved_key_simplifications_p4_p5() {
    let metrics = decode_legacy::<ResultMetrics>(r#"{"testsCount":{"_value":"oops"},"testsCount":{"_value":1}}"#);
    assert_eq!(metrics.unwrap().unwrap().tests_count, 1);
    // D55 normalizes object keys before unknown-field handling.
    let metrics = decode_legacy::<ResultMetrics>(r#"{"\ud800":1}"#).unwrap().unwrap();
    assert_eq!(metrics.tests_count, 0);
}
