//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/command/parse/MarathonTestParseCommand.kt
//! and execution/command/parse/ParseCommandResult.kt (the YAML itself is 3.9b's `parse_results`).

use crate::{
    analytics::MetricsContext,
    artifact_io::{blocking, file_open_error, nio_error},
    logging::{LogRecord, LogSender},
    model::Test,
    report::projection::parse_results,
    runtime::{VendorFuture, VendorRuntime},
    wiring::ParseOutput,
};
use jcompat::{
    file_manager::{JavaPath, absolute_file_path_utf16},
    logging::Level,
    writers::JavaString,
};
use std::{io::Write, sync::Arc};

/// `private val log = MarathonLogging.logger {}` in MarathonTestParseCommand.kt.
pub const LOGGER: &str = "com.malinskiy.marathon.execution.command.parse.MarathonTestParseCommand";
/// `EXTENSION`: the file is `.yml` although `parse -o`'s help says `.yaml` (C12).
pub const EXTENSION: &str = ".yml";

pub struct MarathonTestParseCommand {
    io: Arc<dyn VendorRuntime>,
    output_dir: JavaPath,
    /// `user.dir`, against which `outputDir.absolutePath` resolves.
    cwd: JavaPath,
    logs: LogSender,
    stamp: Arc<dyn Fn() -> (i64, i64) + Send + Sync>,
}
impl MarathonTestParseCommand {
    pub fn new(io: Arc<dyn VendorRuntime>, output_dir: JavaPath, cwd: JavaPath, context: &MetricsContext) -> Self {
        Self {
            io,
            output_dir,
            cwd,
            logs: context.logs.clone(),
            stamp: context.stamp.clone(),
        }
    }
    fn info(&self, message: JavaString) {
        let (at_millis, wall_millis) = (self.stamp)();
        self.logs.push(LogRecord {
            logger: LOGGER.into(),
            at_millis,
            wall_millis,
            level: Level::Info,
            message,
            cause: None,
        });
    }
}

/// `File(parent, child)` on Unix: the normalized child appended with one separator,
/// even when the child is absolute; an empty child is the parent itself.
fn java_child(parent: &JavaPath, child: &JavaString) -> JavaPath {
    let slash = u16::from(b'/');
    let mut normalized: Vec<u16> = Vec::with_capacity(child.0.len());
    for &unit in &child.0 {
        if unit == slash && normalized.last() == Some(&slash) {
            continue;
        }
        normalized.push(unit);
    }
    if normalized.len() > 1 && normalized.last() == Some(&slash) {
        normalized.pop();
    }
    if normalized.is_empty() {
        return parent.clone();
    }
    let mut path = parent.0.clone();
    if normalized[0] == slash {
        if path == [slash] {
            path.clear();
        }
    } else if path.last() != Some(&slash) {
        path.push(slash);
    }
    path.extend_from_slice(&normalized);
    JavaPath(path)
}

impl ParseOutput for MarathonTestParseCommand {
    fn execute<'a>(
        &'a self,
        tests: &'a [Test],
        flaky_tests: &'a [Test],
        output_file_name: Option<&'a JavaString>,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let result = parse_results(tests, flaky_tests);
            self.info("Parse execute mode. Result".into());
            match output_file_name {
                Some(name) => {
                    let directory = absolute_file_path_utf16(&self.output_dir.0, &self.cwd.0);
                    let mut file_name = name.clone();
                    file_name.push_str(EXTENSION);
                    blocking(self.io.as_ref(), move || {
                        // Files.createDirectories, then File(dirPath.toFile(), name + EXTENSION).writeText.
                        let native = directory.to_file_path_buf().map_err(|e| nio_error(e, &directory))?;
                        std::fs::create_dir_all(&native).map_err(|e| nio_error(e, &directory))?;
                        let target = java_child(&directory, &file_name);
                        let path = target.to_file_path_buf().map_err(|e| file_open_error(e, &target))?;
                        let mut file = std::fs::File::create(path).map_err(|e| file_open_error(e, &target))?;
                        file.write_all(&result.to_utf8_bytes())
                            .map_err(crate::artifact_io::io_error)
                    })
                    .await
                }
                None => {
                    self.info(result);
                    Ok(())
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn java_child_matches_file_parent_child() {
        let parent = JavaPath::from_text("/out");
        let child = |c: &str| String::from_utf16_lossy(&java_child(&parent, &c.into()).0);
        assert_eq!(child("r.yml"), "/out/r.yml");
        assert_eq!(child("/abs.yml"), "/out/abs.yml");
        assert_eq!(child("a//b/.yml"), "/out/a/b/.yml");
        assert_eq!(child(""), "/out");
        let root = JavaPath::from_text("/");
        assert_eq!(String::from_utf16_lossy(&java_child(&root, &"/x".into()).0), "/x");
        assert_eq!(String::from_utf16_lossy(&java_child(&root, &"x".into()).0), "/x");
    }
}
