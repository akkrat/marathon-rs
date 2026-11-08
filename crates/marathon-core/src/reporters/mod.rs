//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/Reporter.kt
//! and report/{html/HtmlSummaryReporter,timeline/TimelineReporter,allure/AllureReporter}.kt.

pub mod allure;
pub mod html;
pub mod timeline;

use crate::{artifact_io::*, error::*, executor::shared::failure};
use jcompat::{
    file_manager::{JavaPath, child_file},
    writers::{GsonWriter, WriterValue},
};
use std::io::Write;

fn object<const N: usize>(fields: [(&str, WriterValue); N]) -> WriterValue {
    WriterValue::Object(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
}
fn string(s: &String) -> WriterValue {
    WriterValue::String(s.clone())
}
fn gson(v: &WriterValue) -> VendorResult<String> {
    GsonWriter::default()
        .write(v)
        .map_err(|e| VendorError::Other(failure("java.lang.IllegalArgumentException", &e.0)))
}
fn child(dir: &JavaPath, name: &str) -> JavaPath {
    child_file(dir, &name.encode_utf16().collect::<Vec<_>>())
}
fn mkdir(dir: &JavaPath) -> VendorResult<()> {
    // File.mkdirs returns false on failure; the following file open supplies the exception.
    let _ = std::fs::create_dir_all(dir.to_file_path_buf().map_err(io_error)?);
    Ok(())
}
fn write(path: &JavaPath, bytes: &[u8]) -> VendorResult<()> {
    let mut file =
        std::fs::File::create(path.to_file_path_buf().map_err(io_error)?).map_err(|e| file_open_error(e, path))?;
    file.write_all(bytes).map_err(io_error)
}
fn exists(path: &JavaPath) -> bool {
    path.to_file_path_buf().is_ok_and(|p| p.exists())
}
