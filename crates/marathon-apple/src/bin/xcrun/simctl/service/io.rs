//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/simctl/service/IoService.kt

use super::SimctlService;
use crate::{
    bin::argv,
    cmd::{CommandResult, CommandSession},
    extensions::Durations,
};
use marathon_config::vendor::{Codec, Display, Mask, ScreenshotType};
use marathon_core::{error::VendorResult, vendor::VendorContext};
use marathon_logging::{Component, Level};

pub struct IoService {
    service: SimctlService,
    logger: Component,
}
impl IoService {
    pub fn new(service: SimctlService) -> Self {
        let logger = service
            .tools
            .component("marathon_apple::bin::xcrun::simctl::service::io");
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
            Level::DEBUG,
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
    pub async fn record_video(
        &self,
        udid: &str,
        remote_path: &str,
        codec: Codec,
        display: Display,
        mask: Mask,
        context: &VendorContext,
    ) -> VendorResult<CommandSession> {
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
        // The recording's owner drives stop. An executor timeout would send TERM before INT.
        self.service.open(Durations::INFINITE, args, context).await
    }
}
