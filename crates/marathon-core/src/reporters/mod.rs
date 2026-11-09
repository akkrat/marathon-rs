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
    fn reporter_paths_reject_nul_before_filesystem_access() {
        let path = std::path::Path::new("pool\0tail");
        for result in [mkdir(path), write(path, b"report")] {
            let error = result.expect_err("NUL must be rejected");
            assert_eq!(error.failure().class, "java.nio.file.InvalidPathException");
            assert_eq!(error.failure().message.as_deref(), Some("Nul character not allowed"));
        }
    }
}
