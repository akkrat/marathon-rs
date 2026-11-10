//! Port of core/src/main/kotlin/com/malinskiy/marathon/test/Test.kt,
//! test/MetaProperty.kt, device/DeviceInfo.kt, execution/TestResult.kt,
//! execution/Attachment.kt and json/FileSerializer.kt.

pub use super::yaml::parse_results;
use crate::model::*;

pub fn status(status: TestStatus) -> &'static str {
    match status {
        TestStatus::Failure => "FAILURE",
        TestStatus::Passed => "PASSED",
        TestStatus::Ignored => "IGNORED",
        TestStatus::Incomplete => "INCOMPLETE",
        TestStatus::AssumptionFailure => "ASSUMPTION_FAILURE",
    }
}
/// Raw metadata retains the annotation text contract; the raw reporter handles
/// its top-level unboxed-null-to-empty-string exception separately.
pub fn metadata_text(value: &MetaValue) -> String {
    crate::filter::annotation_text(value)
}
