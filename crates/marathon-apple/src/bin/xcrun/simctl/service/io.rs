//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/simctl/service/IoService.kt

use super::SimctlService;
use crate::{
    bin::argv,
    cmd::{CommandResult, CommandSession, blocking, millis, safe_shell_result},
    extensions::Durations,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use marathon_config::vendor::{Codec, Display, Mask, ScreenshotType};
use marathon_core::{
    error::{VendorError, VendorResult},
    vendor::VendorContext,
};
use marathon_logging::{Component, Level};

/// simctl's stderr when the simulator's video service refuses a recording (status 16, EBUSY), D98. Xcode 16 to 26.x
/// before 26.6 print `Error starting video recorder: Error Domain=NSPOSIXErrorDomain Code=16 "Resource busy"
/// UserInfo={NSLocalizedFailureReason=A recording is already in progress}.` and the reason again; Xcode 26.6 has
/// `Host recording is already in progress` as the reason (U262). The one definition for the recorder's busy detection
/// and the worker supervisor's restart hint.
const RECORDING_BUSY_TEXT: [&str; 2] = ["Resource busy", "recording is already in progress"];

const MAX_FRAME_BYTES: usize = 24 * 1024 * 1024;
const MAX_BASE64_BYTES: usize = MAX_FRAME_BYTES.div_ceil(3) * 4;
const FRAME_SCRIPT: &str = concat!(
    "umask 077; file=$1; limit=$2; shift 2; ",
    "trap '/bin/rm -f \"$file\"' EXIT; trap 'exit 143' TERM; trap 'exit 129' HUP; ",
    "\"$@\" \"$file\" >/dev/null || exit $?; ",
    "size=$(/usr/bin/wc -c < \"$file\") || exit $?; ",
    "[ \"$size\" -le \"$limit\" ] || exit 65; ",
    // Unlink before sending the frame, so a cancelled or stalled encoder
    // never holds a pathname open on the worker.
    "exec 3< \"$file\"; /bin/rm -f \"$file\" || exit $?; exec /usr/bin/base64 <&3"
);

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn decode_frame(mut encoded: Vec<u8>) -> Option<Vec<u8>> {
    encoded.retain(|byte| !matches!(byte, b'\r' | b'\n'));
    if encoded.is_empty() || encoded.len() > MAX_BASE64_BYTES {
        return None;
    }
    STANDARD.decode(encoded).ok()
}

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
        let mut args = Self::screenshot_args(udid, kind, display, mask);
        args.push(destination.to_owned());
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
    /// D100: capture and stream one frame privately on the worker's existing SSH connection.
    pub async fn screenshot_ssh(
        &self,
        udid: &str,
        destination: &str,
        kind: ScreenshotType,
        display: Display,
        mask: Mask,
        context: &VendorContext,
    ) -> VendorResult<Option<Vec<u8>>> {
        let file = format!("{destination}.capture-{:032x}.{}", rand::random::<u128>(), kind.value());
        let mut command = vec![
            "/bin/sh".into(),
            "-c".into(),
            quote(FRAME_SCRIPT),
            "marathon-screenshot".into(),
            quote(&file),
            MAX_FRAME_BYTES.to_string(),
        ];
        command.extend(
            SimctlService::command(Self::screenshot_args(udid, kind, display, mask))
                .iter()
                .map(|arg| quote(arg)),
        );
        let tools = &self.service.tools;
        // The timeout covers open, exec reply and both streams, not just
        // process exit (which can precede its final bytes).
        let mut request = tools.request(command, Durations::INFINITE);
        request.capture_limits = Some((MAX_BASE64_BYTES + MAX_BASE64_BYTES / 16, 64 * 1024));
        let timeout = millis(self.service.timeouts().screenshot)?;
        let mut frame = None;
        let output = &mut frame;
        let completed = context
            .clock
            .with_timeout_or_null(
                context.cancellation.clone(),
                timeout,
                Box::new(move |cancellation| {
                    Box::pin(async move {
                        let context = VendorContext {
                            cancellation,
                            ..context.clone()
                        };
                        let capture = async {
                            tools
                                .executor
                                .execute(request.clone(), context.clone())
                                .await?
                                .await_capture()
                                .await
                        }
                        .await;
                        if let Some(Some(bytes)) = safe_shell_result(tools.executor.as_ref(), &request, capture)? {
                            // Never include either stream in an error or in debug output.
                            *output = tokio::select! { biased;
                                reason = context.cancellation.cancelled() => return Err(VendorError::Cancelled(reason)),
                                decoded = blocking(tools.runtime.as_ref(), move || Ok(decode_frame(bytes))) => decoded?,
                            };
                        }
                        Ok(())
                    })
                }),
            )
            .await?;
        Ok(completed.and(frame))
    }

    fn screenshot_args(udid: &str, kind: ScreenshotType, display: Display, mask: Mask) -> Vec<String> {
        vec![
            "io".to_owned(),
            udid.to_owned(),
            "screenshot".to_owned(),
            format!("--type={}", kind.value()),
            format!("--display={}", display.value()),
            format!("--mask={}", mask.value()),
        ]
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

#[cfg(test)]
mod tests {
    use super::*;
    use marathon_process::Command;

    #[test]
    fn decode_a_nonempty_base64_frame_d100() {
        assert_eq!(decode_frame(b"YQ==\r\n".to_vec()), Some(b"a".to_vec()));
        assert_eq!(decode_frame(b"\r\n".to_vec()), None);
        assert_eq!(decode_frame(b"YW&j".to_vec()), None);
        assert_eq!(decode_frame(vec![b'A'; MAX_BASE64_BYTES + 1]), None);
    }

    #[tokio::test]
    async fn worker_frame_keeps_simctl_status_and_removes_the_temporary_file_d100() -> std::io::Result<()> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("a 'quoted' file.png");
        let run = |limit: &str, body: &str| {
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", FRAME_SCRIPT, "marathon-screenshot"])
                .arg(&file)
                .args([limit, "/bin/sh", "-c", body, "xcrun"]);
            command
        };
        let result = run("3", "printf abc > \"$1\"; echo not-a-frame; exit 37")
            .output()
            .await?;
        assert_eq!(result.status.code(), Some(37));
        assert!(result.stdout.is_empty());
        assert!(!file.exists());
        let oversize = run("2", "printf abc > \"$1\"").output().await?;
        assert_eq!(oversize.status.code(), Some(65));
        assert!(oversize.stdout.is_empty());
        assert!(!file.exists());
        let success = run("3", "printf abc > \"$1\"; echo not-a-frame").output().await?;
        assert!(success.status.success());
        assert_eq!(decode_frame(success.stdout), Some(b"abc".to_vec()));
        assert!(!file.exists());
        Ok(())
    }
}
