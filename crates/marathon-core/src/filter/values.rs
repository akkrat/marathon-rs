//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/filter/FilterValuesFileReader.kt

use marathon_logging::Level;

use super::failure;
use crate::error::{Failure, OperationResult};
use jcompat::{file_manager::absolute_file_path, jstring, };
use marathon_error::FailureKind;
use std::{io, sync::Arc};

/// Called only on the vendor blocking pool. A run can forward these records to
/// its output task; the sink must retain the supplied logger and cause.
pub trait FilterComponent: Send + Sync {
    fn log(&self, level: Level, logger: &str, message: String, cause: Option<Failure>);
}
pub trait FilterValuesReader: Send + Sync {
    fn read_values(&self, path: &std::path::Path) -> OperationResult<Vec<String>>;
}
pub struct FileValuesReader {
    pub logger: Arc<dyn FilterComponent>,
}
impl FilterValuesReader for FileValuesReader {
    fn read_values(&self, path: &std::path::Path) -> OperationResult<Vec<String>> {
        let native = path;
        // File.exists returns false for invalid legacy File paths (including
        // NUL); unlike NIO Paths.get, it does not throw during this check.
        if !native.exists() {
            // The JVM resolves against its cached `user.dir`, so a deleted or
            // unreadable working directory never fails here; log the path as given.
            let absolute = match std::env::current_dir() {
                Ok(cwd) => {
                    let cwd = cwd.to_string_lossy();
                    absolute_file_path(&path.to_string_lossy(), &cwd)
                }
                Err(_) => path.to_path_buf(),
            };
            let mut message = String::from("Filtering configuration file ");
            message.push_str(&absolute.to_string_lossy());
            message.push_str(" does not exist. Applying empty list.");
            self.logger.log(Level::ERROR, "FilterValuesFileReader", message, None);
            return Ok(Vec::new());
        }

        let bytes = std::fs::read(native).map_err(|e| io_failure(path, &e))?;
        Ok(parse_values(&bytes))
    }
}

pub fn parse_values(bytes: &[u8]) -> Vec<String> {
    jstring::lines(&String::from_utf8_lossy(bytes))
        .into_iter()
        .filter_map(|line| {
            let trimmed = jstring::trim(&line);
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            // Java's built-in \s is ASCII; `$` also matches before a final NEL/LS/PS.
            let mut result = line.clone();
            for (i, c) in line.char_indices() {
                if c == '#' && i > 0 && ascii_space(line.as_bytes()[i - 1]) {
                    let terminator = line[i..].find(['\u{85}', '\u{2028}', '\u{2029}']).map(|p| i + p);
                    if terminator.is_some_and(|p| line[p..].chars().count() != 1) {
                        continue;
                    }
                    let mut start = i;
                    while start > 0 && ascii_space(line.as_bytes()[start - 1]) {
                        start -= 1;
                    }
                    result.replace_range(start..terminator.unwrap_or(line.len()), "");
                    break;
                }
            }
            Some(jstring::trim(&result).to_owned())
        })
        .collect()
}
fn ascii_space(c: u8) -> bool {
    matches!(c, 9..=13 | 32)
}

pub(super) fn io_failure(path: &std::path::Path, error: &io::Error) -> Failure {
    let class = if matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied | io::ErrorKind::IsADirectory
    ) {
        FailureKind::FileNotFound
    } else {
        FailureKind::Io
    };
    let reason = match error.kind() {
        io::ErrorKind::NotFound => "No such file or directory",
        io::ErrorKind::PermissionDenied => "Permission denied",
        io::ErrorKind::IsADirectory => "Is a directory",
        _ => return failure(class, &error.to_string()),
    };
    let mut message = path.to_string_lossy().into_owned();
    message.push_str(" (");
    message.push_str(reason);
    message.push(')');
    Failure {
        kind: class,
        message: Some(message),
        cause: None,
        stacktrace: None,
    }
}
