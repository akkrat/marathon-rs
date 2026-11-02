//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/filter/FilterValuesFileReader.kt

use super::failure;
use crate::error::{Failure, OperationResult};
use jcompat::{
    file_manager::{JavaPath, absolute_file_path_utf16},
    jstring,
    logging::Level,
    utf8,
    writers::JavaString,
};
use std::{io, sync::Arc};

/// Called only on the vendor blocking pool. A run can forward these records to
/// its output task; the sink must retain the supplied logger and cause.
pub trait FilterLogger: Send + Sync {
    fn log(&self, level: Level, logger: &str, message: JavaString, cause: Option<Failure>);
}
pub trait FilterValuesReader: Send + Sync {
    fn read_values(&self, path: &JavaPath) -> OperationResult<Vec<JavaString>>;
}
pub struct FileValuesReader {
    pub logger: Arc<dyn FilterLogger>,
}
impl FilterValuesReader for FileValuesReader {
    fn read_values(&self, path: &JavaPath) -> OperationResult<Vec<JavaString>> {
        let native = path.to_file_path_buf();
        // File.exists returns false for invalid legacy File paths (including
        // NUL); unlike NIO Paths.get, it does not throw during this check.
        if !native.as_ref().is_ok_and(|path| path.exists()) {
            // The JVM resolves against its cached `user.dir`, so a deleted or
            // unreadable working directory never fails here; log the path as given.
            let absolute = match std::env::current_dir() {
                Ok(cwd) => {
                    let cwd: Vec<u16> = cwd.to_string_lossy().encode_utf16().collect();
                    absolute_file_path_utf16(&path.0, &cwd)
                }
                Err(_) => JavaPath(path.0.clone()),
            };
            let mut message = JavaString::from("Filtering configuration file ");
            message.0.extend(absolute.0);
            message.push_str(" does not exist. Applying empty list.");
            self.logger.log(Level::Error, "FilterValuesFileReader", message, None);
            return Ok(Vec::new());
        }
        let native = native.map_err(|e| io_failure(path, &e))?;
        let bytes = std::fs::read(&native).map_err(|e| io_failure(path, &e))?;
        Ok(parse_values(&bytes))
    }
}

pub fn parse_values(bytes: &[u8]) -> Vec<JavaString> {
    let text: Vec<u16> = utf8::decode_utf8(bytes).encode_utf16().collect();
    jstring::lines(&JavaString(text))
        .into_iter()
        .filter_map(|line| {
            let line = line.0.as_slice();
            let trimmed = jstring::trim(line);
            if trimmed.is_empty() || trimmed.first() == Some(&u16::from(b'#')) {
                return None;
            }
            // Java's built-in \s is ASCII; `$` can also match just before
            // a final NEL/LS/PS, which must survive replacement (NEL is not
            // Kotlin whitespace). Embedded terminators stop `.*`.
            let mut result = line.to_vec();
            for i in 0..line.len() {
                if line[i] == u16::from(b'#') && i > 0 && ascii_space(line[i - 1]) {
                    let terminator = line[i..]
                        .iter()
                        .position(|u| matches!(*u, 0x85 | 0x2028 | 0x2029))
                        .map(|p| i + p);
                    if terminator.is_some_and(|p| p + 1 != line.len()) {
                        continue;
                    }
                    let mut start = i;
                    while start > 0 && ascii_space(line[start - 1]) {
                        start -= 1;
                    }
                    result.drain(start..terminator.unwrap_or(line.len()));
                    break;
                }
            }
            Some(JavaString(jstring::trim(&result).to_vec()))
        })
        .collect()
}
fn ascii_space(c: u16) -> bool {
    matches!(c, 9..=13 | 32)
}

pub(super) fn io_failure(path: &JavaPath, error: &io::Error) -> Failure {
    let class = if matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied | io::ErrorKind::IsADirectory
    ) {
        "java.io.FileNotFoundException"
    } else {
        "java.io.IOException"
    };
    let reason = match error.kind() {
        io::ErrorKind::NotFound => "No such file or directory",
        io::ErrorKind::PermissionDenied => "Permission denied",
        io::ErrorKind::IsADirectory => "Is a directory",
        _ => return failure(class, &error.to_string()),
    };
    let mut message = JavaString(path.0.clone());
    message.push_str(" (");
    message.push_str(reason);
    message.push_str(")");
    Failure {
        class: class.into(),
        message: Some(message),
        cause: None,
        stacktrace: None,
    }
}
