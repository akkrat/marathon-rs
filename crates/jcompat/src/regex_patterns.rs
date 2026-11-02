//! Port of core/src/main/kotlin/com/malinskiy/marathon/extension/StringExtensions.kt
//! and the fixed regex sites inventoried in docs/wp/1.4.md.

use crate::JavaRegex;
use std::sync::LazyLock;

macro_rules! fixed {
    ($($name:ident = $pattern:expr;)+) => {$(
        pub static $name: LazyLock<JavaRegex> = LazyLock::new(||
            JavaRegex::compile($pattern, stringify!($name)).expect("tested fixed marathon pattern"));
    )+};
}

// Java's default classes are ASCII, and its dot excludes five line terminators.
// Spell those explicitly here; user patterns retain the crate's D6 defaults.
fixed! {
    ESCAPE = r"[^a-zA-Z0-9.#]";
    FILTER_COMMENT = r"[ \t\n\x0B\f\r]+#[^\n\r\x{85}\x{2028}\x{2029}]*";
    GRAPHITE_WHITESPACE = r"[ \t\n\x0B\f\r]";
    PLAIN_PATH = r"/[A-Za-z0-9_./@+\-]+";
    RSYNC_FIRST_LINE = r"(?s)\n.*\z";
    SESSION_RESULTS_PATH = r"(^[ \t\n\x0B\f\r]*|[ \t\n\x0B\f\r]+)/[^\n\r\x{85}\x{2028}\x{2029}]+\.xcresult[ \t\n\x0B\f\r]*$";
    DIAGNOSTIC_LOGS_PATH = r"(^[ \t\n\x0B\f\r]*|[ \t\n\x0B\f\r]+)/[^\n\r\x{85}\x{2028}\x{2029}]+\.log[ \t\n\x0B\f\r]*$";
    TEST_CASE_STARTED = r"Test Case '-\[([^\n\r\x{85}\x{2028}\x{2029}]+) ([^\n\r\x{85}\x{2028}\x{2029}]+)\]' started\.";
    TEST_CASE_FINISHED = r"Test Case '-\[([^\n\r\x{85}\x{2028}\x{2029}]+) ([^\n\r\x{85}\x{2028}\x{2029}]+)\]' (passed|failed|skipped) \(([0-9.]+) seconds\)\.";
    FAILING_TEST = r"(/[^\n\r\x{85}\x{2028}\x{2029}]+|<unknown>:[0-9]+):[ \t\n\x0B\f\r]error:[ \t\n\x0B\f\r][+\-]\[([^\n\r\x{85}\x{2028}\x{2029}]*)[ \t\n\x0B\f\r]([^\n\r\x{85}\x{2028}\x{2029}]*)\][ \t\n\x0B\f\r]:([ \t\n\x0B\f\r][^\n\r\x{85}\x{2028}\x{2029}]*)";
    TIMEOUT_TEST = r"Test Case '-\[([^\n\r\x{85}\x{2028}\x{2029}]+) ([^\n\r\x{85}\x{2028}\x{2029}]+)\]' exceeded execution time allowance of ([^\n\r\x{85}\x{2028}\x{2029}]+)\. The test may have hung[^\n\r\x{85}\x{2028}\x{2029}]*";
    UDID = r"[A-Z0-9\-]*";
    BUNDLE_NAME = r"[- ]";
    METRIC_ANNOTATION_V2 = r"[!-~]+\([^)]*\)";
    LOGCAT_DATE_TIME = r"[0-9][0-9]-[0-9][0-9] [0-9][0-9]:[0-9][0-9]:[0-9][0-9]\.[0-9][0-9][0-9]";
    LOGCAT_PROCESS_ID = r"[0-9]+";
    LOGCAT_THREAD_ID = r"[a-zA-Z_0-9]+";
    LOGCAT_PRIORITY = r"[VDIWEAF]";
    LOGCAT_TAG = r"[^\n\r\x{85}\x{2028}\x{2029}]*?";
    LOGCAT_HEADER = r"^\[ ([0-9][0-9]-[0-9][0-9] [0-9][0-9]:[0-9][0-9]:[0-9][0-9]\.[0-9][0-9][0-9]) +([0-9]+): *([a-zA-Z_0-9]+) ([VDIWEAF])/([^\n\r\x{85}\x{2028}\x{2029}]*?) +\]$";
    PARAMETERIZED_METHOD = r"[^\n\r\x{85}\x{2028}\x{2029}]+\[[0-9]+\]";
    NM_SWIFT_TEST = r"[.|_]test";
    NM_TEXT_SYMBOL = r" t ";
    NM_OBJC_TEST = r" test";
    DEFAULT_TEST_CLASS_SUFFIX = r"[^\n\r\x{85}\x{2028}\x{2029}]*Tests*";
}

/// Built-in default only. Explicit user-supplied lookahead is rejected under D6.
/// Keep the original config default string for serialization in the config WP.
pub const DEFAULT_TEST_CLASS_PATTERN: &str = "^((?!Abstract).)*Test[s]*$";
pub fn default_test_class_matches(class: &str) -> bool {
    !class.contains("Abstract") && DEFAULT_TEST_CLASS_SUFFIX.matches(class)
}

fn without_final_terminator(input: &str) -> &str {
    input
        .strip_suffix("\r\n")
        .or_else(|| input.strip_suffix(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}']))
        .unwrap_or(input)
}

/// Preserve Java $ accepting a position before the final terminator without
/// consuming that terminator in the replacement. Trimming remains the caller's job.
pub fn strip_filter_comment(input: &str) -> String {
    let body = without_final_terminator(input);
    for captures in FILTER_COMMENT.find_all(input) {
        let m = captures.get(0).expect("group zero");
        if m.end() == input.len() || m.end() == body.len() {
            return format!("{}{}", &input[..m.start()], &input[m.end()..]);
        }
    }
    input.into()
}

/// ArtifactFinderListener takes group zero, including its leading whitespace.
pub fn find_artifact<'a>(pattern: &JavaRegex, input: &'a str) -> Option<&'a str> {
    pattern
        .find(input)
        .or_else(|| pattern.find(without_final_terminator(input)))
        .and_then(|c| c.get(0))
        .map(|m| m.as_str())
}

#[cfg(test)]
mod tests;
