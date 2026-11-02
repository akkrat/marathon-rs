//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/xcresulttool/Xcresulttool.kt
//!
//! The mapper is WP 4.6's [`decode_legacy`]; its structured failure is the logged cause (WP 4.4 decision 5).

use super::super::{Tools, argv, kt};
use crate::{
    cmd::blocking,
    model::XcodeVersion,
    xcresult::{LegacyDecode, decode_legacy},
};
use jcompat::logging::{Level, Logger};
use marathon_config::vendor::AppleTimeoutConfiguration;
use marathon_core::{error::VendorResult, vendor::VendorContext};

/// The first Xcode whose `xcresulttool get` reads result bundle objects only with `--legacy` (without it,
/// exit code 64); earlier versions do not know the option.
pub const LEGACY_GET_SINCE: XcodeVersion = XcodeVersion::new(16, 0, 0);
/// `TimeoutConfiguration.DEFAULT_XCRESULTTOOL`.
pub const DEFAULT_XCRESULTTOOL: jcompat::jtime::Duration = jcompat::jtime::Duration::of_seconds(120);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResultBundleFormat {
    #[default]
    Json,
    Raw,
}
impl ResultBundleFormat {
    pub fn value(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Raw => "raw",
        }
    }
}

/// The provider startup line (5.5a/5.6 log it at INFO once); `timeouts` are the effective values.
pub fn describe_timeout(timeouts: &AppleTimeoutConfiguration) -> String {
    format!(
        "xcresulttool timeout is {} (timeoutConfiguration.xcresulttool; when not set, the larger of shell {} and {})",
        timeouts.xcresulttool, timeouts.shell, DEFAULT_XCRESULTTOOL
    )
}

#[derive(Clone)]
pub struct Xcresulttool {
    tools: Tools,
    logger: Logger,
}
impl Xcresulttool {
    pub fn new(tools: Tools) -> Self {
        let logger = tools.logger("com.malinskiy.marathon.apple.bin.xcrun.xcresulttool.Xcresulttool");
        Self { tools, logger }
    }
    /// `xcode_version` decides `--legacy` (unknown: without). `version` is accepted and never emitted (A42).
    pub async fn get<T: LegacyDecode + Send + 'static>(
        &self,
        path: &str,
        xcode_version: Option<XcodeVersion>,
        format: ResultBundleFormat,
        id: Option<&str>,
        version: Option<&str>,
        context: &VendorContext,
    ) -> VendorResult<Option<T>> {
        let _ = version;
        let mut args = argv(["xcrun", "xcresulttool", "get"]);
        if xcode_version.is_some_and(|xcode| xcode >= LEGACY_GET_SINCE) {
            args.push("--legacy".into());
        }
        args.extend(argv(["--format", format.value(), "--path", path]));
        if let Some(id) = id {
            args.extend(argv(["--id", id]));
        }
        let Some(result) = self.tools.safe(args, self.tools.timeouts.xcresulttool, context).await? else {
            return Ok(None);
        };
        if !result.successful() {
            // A timed out call (null exit code) leaves truncated output: never parse it.
            self.tools.log(
                &self.logger,
                Level::Warn,
                || {
                    format!(
                        "xcresulttool get failed for {path} with exit code {}: {}",
                        result.exit_code.map_or_else(|| "null".into(), |code| code.to_string()),
                        result.combined_stderr()
                    )
                },
                None,
            );
            return Ok(None);
        }
        let json = kt::trim(&result.combined_stdout()).to_owned();
        if kt::is_blank(&json) {
            return Ok(None);
        }
        match blocking(&*self.tools.runtime, move || Ok(decode_legacy::<T>(&json))).await? {
            Ok(value) => Ok(value),
            Err(error) => {
                self.tools.log(
                    &self.logger,
                    Level::Warn,
                    || format!("Unable to parse xcresulttool output for {path}"),
                    Some(error.failure),
                );
                Ok(None)
            }
        }
    }
    /// Kotlin documents two or more inputs but validates nothing.
    pub async fn merge(&self, output_path: &str, paths: &[String], context: &VendorContext) -> VendorResult<bool> {
        let mut command = argv(["xcrun", "xcresulttool", "merge", "--output-path", output_path]);
        command.extend_from_slice(paths);
        Ok(self
            .tools
            .critical(command, self.tools.timeouts.xcresulttool, context)
            .await?
            .successful())
    }
}
