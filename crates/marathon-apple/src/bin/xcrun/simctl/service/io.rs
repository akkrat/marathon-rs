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

/// simctl's stderr when the simulator's video service refuses a recording (status 16, EBUSY), D98. Xcode 16 to 26.x
/// before 26.6 print `Error starting video recorder: Error Domain=NSPOSIXErrorDomain Code=16 "Resource busy"
/// UserInfo={NSLocalizedFailureReason=A recording is already in progress}.` and the reason again; Xcode 26.6 has
/// `Host recording is already in progress` as the reason (U262). The one definition for the recorder's busy detection
/// and the worker supervisor's restart hint.
const RECORDING_BUSY_TEXT: [&str; 2] = ["Resource busy", "recording is already in progress"];

/// Whether a recorder's stderr carries simctl's busy refusal.
pub fn reports_recording_busy(stderr: &str) -> bool {
    RECORDING_BUSY_TEXT.iter().any(|text| stderr.contains(text))
}

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
        let args = Self::record_video_args(udid, remote_path, codec, display, mask);
        // The recording's owner drives stop. An executor timeout would send TERM before INT.
        self.service
            .open_recording(Durations::INFINITE, args, udid, context)
            .await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn record_video_nohup(
        &self,
        udid: &str,
        remote_path: &str,
        codec: Codec,
        display: Display,
        mask: Mask,
        pidfile: &str,
        context: &VendorContext,
    ) -> VendorResult<Option<CommandResult>> {
        let args = Self::record_video_args(udid, remote_path, codec, display, mask);
        let timeout = self.service.timeouts().video;
        let tools = &self.service.tools;
        tools
            .executor
            .safe_execute_nohup(
                pidfile,
                tools.request(super::super::SimctlService::command(args), timeout),
                context.clone(),
            )
            .await
    }

    fn record_video_args(udid: &str, remote_path: &str, codec: Codec, display: Display, mask: Mask) -> Vec<String> {
        argv([
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
        ])
    }
}
