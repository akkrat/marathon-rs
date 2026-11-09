//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/command/parse/MarathonTestParseCommand.kt
//! and execution/command/parse/ParseCommandResult.kt (the YAML itself is 3.9b's `parse_results`).

use marathon_logging::Level;

use crate::{
    analytics::MetricsContext,
    artifact_io::{blocking, file_open_error, nio_error},
    logging::{LogRecord, LogSender},
    model::Test,
    report::projection::parse_results,
    runtime::{VendorFuture, VendorRuntime},
    wiring::ParseOutput,
};
use jcompat::{file_manager::absolute_file_path, };
use std::path::PathBuf;
use std::{io::Write, sync::Arc};

/// `private val log = LogSettings.logger {}` in MarathonTestParseCommand.kt.
pub const LOGGER: &str = "marathon_core::execution::parse_command";
/// `EXTENSION`: the file is `.yml` although `parse -o`'s help says `.yaml` (C12).
pub const EXTENSION: &str = ".yml";

pub struct MarathonTestParseCommand {
    io: Arc<dyn VendorRuntime>,
    output_dir: PathBuf,
    /// `user.dir`, against which `outputDir.absolutePath` resolves.
    cwd: PathBuf,
    logs: LogSender,
    stamp: Arc<dyn Fn() -> (i64, i64) + Send + Sync>,
}
impl MarathonTestParseCommand {
    pub fn new(io: Arc<dyn VendorRuntime>, output_dir: PathBuf, cwd: PathBuf, context: &MetricsContext) -> Self {
        Self {
            io,
            output_dir,
            cwd,
            logs: context.logs.clone(),
            stamp: context.stamp.clone(),
        }
    }
    fn info(&self, message: String) {
        let (at_millis, wall_millis) = (self.stamp)();
        self.logs.push(LogRecord { target: module_path!().into(),
            logger: LOGGER.into(),
            at_millis,
            wall_millis,
            level: Level::INFO,
            message,
            cause: None,
        });
    }
}

/// `File(parent, child)` on Unix: the normalized child appended with one separator,
/// even when the child is absolute; an empty child is the parent itself.
fn java_child(parent: &std::path::Path, child: &str) -> PathBuf {
    let child = jcompat::file_manager::file_path(child);
    let child = child.to_string_lossy();
    if child.is_empty() {
        return parent.to_path_buf();
    }
    let mut path = parent.to_string_lossy().into_owned();
    if child.starts_with('/') {
        if path == "/" {
            path.clear();
        }
    } else if !path.ends_with('/') {
        path.push('/');
    }
    path.push_str(&child);
    PathBuf::from(path)
}

impl ParseOutput for MarathonTestParseCommand {
    fn execute<'a>(
        &'a self,
        tests: &'a [Test],
        flaky_tests: &'a [Test],
        output_file_name: Option<&'a str>,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let result = parse_results(tests, flaky_tests);
            self.info("Parse execute mode. Result".into());
            match output_file_name {
                Some(name) => {
                    let directory = absolute_file_path(&self.output_dir.to_string_lossy(), &self.cwd.to_string_lossy());
                    let mut file_name = name.to_owned();
                    file_name.push_str(EXTENSION);
                    blocking(self.io.as_ref(), move || {
                        // Files.createDirectories, then File(dirPath.toFile(), name + EXTENSION).writeText.
                        let native = &directory;
                        std::fs::create_dir_all(native).map_err(|e| nio_error(e, &directory))?;
                        let target = java_child(&directory, &file_name);
                        let path = &target;
                        let mut file = std::fs::File::create(path).map_err(|e| file_open_error(e, &target))?;
                        file.write_all(result.as_bytes()).map_err(crate::artifact_io::io_error)
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
        let parent = PathBuf::from("/out");
        let child = |c: &str| java_child(&parent, c).to_string_lossy().into_owned();
        assert_eq!(child("r.yml"), "/out/r.yml");
        assert_eq!(child("/abs.yml"), "/out/abs.yml");
        assert_eq!(child("a//b/.yml"), "/out/a/b/.yml");
        assert_eq!(child(""), "/out");
        assert_eq!(child("/"), "/out/");
        assert_eq!(child("./r.yml"), "/out/./r.yml");
        let root = PathBuf::from("/");
        assert_eq!(java_child(&root, "/x").to_string_lossy().into_owned(), "/x");
        assert_eq!(java_child(&root, "x").to_string_lossy().into_owned(), "/x");
    }
}
