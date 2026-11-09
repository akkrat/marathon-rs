//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/Reporter.kt
//! and report/{html/HtmlSummaryReporter,timeline/TimelineReporter,allure/AllureReporter}.kt.

pub mod allure;
pub mod html;
pub mod timeline;

use crate::{artifact_io::*, error::*, executor::shared::failure};
use jcompat::{
    file_manager::child_file,
    writers::{GsonWriter, WriterValue},
};
use std::io::Write;
use std::path::PathBuf;

fn object<const N: usize>(fields: [(&str, WriterValue); N]) -> WriterValue {
    WriterValue::Object(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
}
fn string(s: &str) -> WriterValue {
    WriterValue::String(s.to_string())
}
fn gson(v: &WriterValue) -> VendorResult<String> {
    GsonWriter::default()
        .write(v)
        .map_err(|e| VendorError::Other(failure("java.lang.IllegalArgumentException", &e.0)))
}
fn json<T: serde::Serialize + ?Sized>(value: &T) -> VendorResult<String> {
    serde_json::to_string(value).map_err(json_error)
}
fn json_error(error: serde_json::Error) -> VendorError {
    VendorError::Other(failure("java.lang.IllegalArgumentException", &error.to_string()))
}

#[derive(Default)]
struct HtmlSafeFormatter;
impl serde_json::ser::Formatter for HtmlSafeFormatter {
    fn write_string_fragment<W: Write + ?Sized>(&mut self, writer: &mut W, fragment: &str) -> std::io::Result<()> {
        let mut start = 0;
        for (at, ch) in fragment.char_indices() {
            let escape = match ch {
                '<' => b"\\u003c".as_slice(),
                '>' => b"\\u003e".as_slice(),
                '&' => b"\\u0026".as_slice(),
                '=' => b"\\u003d".as_slice(),
                '\'' => b"\\u0027".as_slice(),
                '\u{2028}' => b"\\u2028".as_slice(),
                '\u{2029}' => b"\\u2029".as_slice(),
                _ => continue,
            };
            writer.write_all(&fragment.as_bytes()[start..at])?;
            writer.write_all(escape)?;
            start = at + ch.len_utf8();
        }
        writer.write_all(&fragment.as_bytes()[start..])
    }
}
fn html_json<T: serde::Serialize + ?Sized>(value: &T) -> VendorResult<String> {
    let mut bytes = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut bytes, HtmlSafeFormatter);
    value.serialize(&mut serializer).map_err(json_error)?;
    String::from_utf8(bytes)
        .map_err(|e| VendorError::Other(failure("java.lang.IllegalArgumentException", &e.to_string())))
}

fn child(dir: &std::path::Path, name: &str) -> PathBuf {
    child_file(dir, name)
}
fn mkdir(dir: &std::path::Path) -> VendorResult<()> {
    // Keep invalid-input errors even though File.mkdirs ignores filesystem failures.
    validate_path(dir)?;
    let _ = std::fs::create_dir_all(dir);
    Ok(())
}
fn write(path: &std::path::Path, bytes: &[u8]) -> VendorResult<()> {
    validate_path(path)?;
    let mut file = std::fs::File::create(path).map_err(|e| file_open_error(e, path))?;
    file.write_all(bytes).map_err(io_error)
}
fn exists(path: &std::path::Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_json_escapes_script_delimiters_keys_and_line_separators() {
        let text = "</script><>&='\u{2028}\u{2029}名😀\n\\\"";
        let value = std::collections::BTreeMap::from([(text, [text])]);
        let html = html_json(&value).unwrap();
        for forbidden in ['<', '>', '&', '=', '\'', '\u{2028}', '\u{2029}'] {
            assert!(!html.contains(forbidden));
        }
        assert!(html.contains(r"\u003c/script\u003e"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&html).unwrap(),
            serde_json::to_value(value).unwrap()
        );
        assert!(json(&text).unwrap().contains("</script>"));
    }
    #[test]
    fn reporter_paths_reject_nul_before_filesystem_access() {
        let path = std::path::Path::new("pool\0tail");
        for result in [mkdir(path), write(path, b"report")] {
            let error = result.expect_err("NUL must be rejected");
            assert_eq!(error.failure().class, "java.nio.file.InvalidPathException");
            assert_eq!(error.failure().message.as_deref(), Some("Nul character not allowed"));
        }
    }
}
