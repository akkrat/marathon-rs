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
fn mkdir(dir: &PathBuf) -> VendorResult<()> {
    // File.mkdirs returns false on failure; the following file open supplies the exception.
    let _ = std::fs::create_dir_all(dir);
    Ok(())
}
fn write(path: &PathBuf, bytes: &[u8]) -> VendorResult<()> {
    let mut file = std::fs::File::create(path).map_err(|e| file_open_error(e, path))?;
    file.write_all(bytes).map_err(io_error)
}
fn exists(path: &std::path::Path) -> bool {
    path.exists()
}
