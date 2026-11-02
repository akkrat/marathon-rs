//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/simctl/service/IoService.kt

use super::SimctlService;
use crate::{bin::argv, cmd::CommandResult};
use jcompat::logging::{Level, Logger};
use marathon_config::vendor::{Codec, Display, Mask, ScreenshotType};
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct IoService {
    service: SimctlService,
    logger: Logger,
}
impl IoService {
    pub fn new(service: SimctlService) -> Self {
        let logger = service
            .tools
            .logger("com.malinskiy.marathon.apple.bin.xcrun.simctl.service.IoService");
        Self { service, logger }
    }
    pub async fn screenshot(
        &self,
        udid: &str,
        destination: &str,
        kind: ScreenshotType,
        display: Display,
        mask: Mask,
        context: &VendorContext,
    ) -> VendorResult<bool> {
        let args = vec![
            "io".to_owned(),
            udid.to_owned(),
            "screenshot".to_owned(),
            format!("--type={}", kind.value()),
            format!("--display={}", display.value()),
            format!("--mask={}", mask.value()),
            destination.to_owned(),
        ];
        let timeout = self.service.timeouts().screenshot;
        let result = self.service.safe_execute(timeout, args, context).await?;
        if result.as_ref().is_some_and(CommandResult::successful) {
            return Ok(true);
        }
        self.service.tools.log(
            &self.logger,
            Level::Debug,
            || {
                let text = |stream: Option<String>| stream.unwrap_or_else(|| "null".into());
                format!(
                    "failed to capture screenshot, stdout={}, stderr={}",
                    text(result.as_ref().map(CommandResult::combined_stdout)),
                    text(result.as_ref().map(CommandResult::combined_stderr))
                )
            },
            None,
        );
        Ok(false)
    }
    /// Recording needs SIGINT, which neither the JVM process API nor OpenSSH exec delivers, hence nohup.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_video(
        &self,
        udid: &str,
        remote_path: &str,
        codec: Codec,
        display: Display,
        mask: Mask,
        pidfile: &str,
        context: &VendorContext,
    ) -> VendorResult<Option<CommandResult>> {
        let args = argv([
            "io",
            udid,
            "recordVideo",
            "--codec",
            codec.value(),
            "--display",
            display.value(),
            "--mask",
            mask.value(),
            "--force",
            remote_path,
        ]);
        let timeout = self.service.timeouts().video;
        self.service.safe_execute_nohup(pidfile, timeout, args, context).await
    }
}
