//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/extensions/StringExtensions.kt

pub fn bash_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
