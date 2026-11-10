//! Port of core/src/main/kotlin/com/malinskiy/marathon/test/Test.kt,
//! test/MetaProperty.kt, device/DeviceInfo.kt, execution/TestResult.kt,
//! execution/Attachment.kt and json/FileSerializer.kt.

pub use super::yaml::parse_results;
use crate::model::*;
use jcompat::{jnum, writers::KotlinValue};

pub fn status(status: TestStatus) -> &'static str {
    match status {
        TestStatus::Failure => "FAILURE",
        TestStatus::Passed => "PASSED",
        TestStatus::Ignored => "IGNORED",
        TestStatus::Incomplete => "INCOMPLETE",
        TestStatus::AssumptionFailure => "ASSUMPTION_FAILURE",
    }
}
pub fn kotlin_meta(value: &MetaValue) -> KotlinValue {
    use MetaValue::*;
    match value {
        Null => KotlinValue::Null,
        List(v) => KotlinValue::List(v.iter().map(kotlin_meta).collect()),
        Map(v) => KotlinValue::Map(
            v.entries()
                .iter()
                .map(|(k, v)| (KotlinValue::Text(k.clone()), kotlin_meta(v)))
                .collect(),
        ),
        Array(v) => KotlinValue::List(v.elements.iter().map(kotlin_meta).collect()),
        Decoded(v) => v.kotlin_value(),
        Boolean(v) => KotlinValue::Text(v.to_string()),
        Byte(v) => KotlinValue::Text(v.to_string()),
        Short(v) => KotlinValue::Text(v.to_string()),
        Int(v) => KotlinValue::Text(v.to_string()),
        Long(v) => KotlinValue::Text(v.to_string()),
        Char(v) => KotlinValue::Text(std::string::String::from_utf16_lossy(&[*v])),
        Float(v) => KotlinValue::Text(jnum::float_to_string(*v)),
        Double(v) => KotlinValue::Text(jnum::double_to_string(*v)),
        String(v) => KotlinValue::Text(v.clone()),
    }
}
