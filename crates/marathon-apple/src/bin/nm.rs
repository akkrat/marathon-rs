//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/nm/Nm.kt

use super::{Tools, argv, kt};
use crate::cmd::failure;
use jcompat::logging::{Level, Logger};
use marathon_core::{
    error::{VendorError, VendorResult},
    vendor::VendorContext,
};

pub const MAX_XARGS: i32 = 131072;

pub struct Nm {
    tools: Tools,
    logger: Logger,
}
impl Nm {
    pub fn new(tools: Tools) -> Self {
        let logger = tools.logger("com.malinskiy.marathon.apple.bin.nm.Nm");
        Self { tools, logger }
    }
    /// `prefilter` is the Kotlin set in its iteration order.
    pub async fn list(&self, path: &str, prefilter: &[String], context: &VendorContext) -> VendorResult<Vec<String>> {
        if prefilter.iter().any(|filter| filter.contains('\'')) {
            return Err(VendorError::Other(failure(
                "java.lang.RuntimeException",
                "nm list prefilter should not contain symbol \"'\"",
            )));
        }
        let filter = prefilter
            .iter()
            .map(|filter| format!("-e \"{filter}\""))
            .collect::<Vec<_>>()
            .join(" ");
        self.list_symbols_via(path, format!("'nm \"{path}\" | grep {filter}'"), false, context)
            .await
    }
    pub async fn swift_tests(&self, path: &str, context: &VendorContext) -> VendorResult<Vec<String>> {
        let pipeline = format!(
            "'nm -gUj \"{path}\" | xargs -s {MAX_XARGS} xcrun swift-demangle | cut -d\\  -f3 | grep -e \"[\\\\.|_]\"test'"
        );
        self.list_symbols_via(path, pipeline, true, context).await
    }
    pub async fn objective_c_tests(&self, path: &str, context: &VendorContext) -> VendorResult<Vec<String>> {
        let pipeline = format!(
            "'nm -U \"{path}\" | grep \" t \" | cut -d\\  -f3,4 | cut -d \"-\" -f2 | cut -d \"[\" -f2 | cut -d \"]\" -f1 | grep \" test\"'"
        );
        self.list_symbols_via(path, pipeline, true, context).await
    }
    /// `complete`: a scan that could not start or did not finish fails, since its symbols are then missing
    /// rather than absent.
    async fn list_symbols_via(
        &self,
        path: &str,
        arg: String,
        complete: bool,
        context: &VendorContext,
    ) -> VendorResult<Vec<String>> {
        let result = self
            .tools
            .safe(argv(["sh", "-c", &arg]), self.tools.timeouts.shell, context)
            .await?;
        if complete && result.as_ref().is_none_or(|result| result.exit_code.is_none()) {
            let how = if result.is_none() {
                "could not be started"
            } else {
                "did not finish"
            };
            return Err(VendorError::Other(failure(
                "com.malinskiy.marathon.apple.cmd.CommandException",
                format!("Symbols of {path} could not be listed: the command {how}"),
            )));
        }
        match result {
            Some(result) if result.successful() => Ok(result
                .stdout
                .iter()
                .map(|line| kt::trim(line))
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()),
            Some(result) => {
                let (stdout, stderr) = (result.combined_stdout(), result.combined_stderr());
                if !kt::trim(&stdout).is_empty() || !kt::trim(&stderr).is_empty() {
                    self.tools.log(
                        &self.logger,
                        Level::Warn,
                        || {
                            kt::trim_indent(&format!(
                                "\n                failed to extract symbol table from {path}\n                stdout: {stdout}\n                stderr: {stderr}\n            "
                            ))
                        },
                        None,
                    );
                }
                Ok(Vec::new())
            }
            None => Ok(Vec::new()),
        }
    }
}
