//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/screenshot/AdamScreenCaptureTestRunListener.kt
use super::*;
use crate::listeners::{AndroidTestRunListener, to_test};
use jcompat::logging::Level;
use marathon_adb::testrunner::TestEvent;
use marathon_core::{runtime::VendorFuture, vendor::VendorContext};
use std::path::PathBuf;
pub const METRIC: &str = "com.malinskiy.adam.junit4.android.screencapture.AdamScreenCaptureProcessor.v1";
pub struct AdamScreenCaptureTestRunListener {
    pub artifacts: ArtifactContext,
    attachments: Attachments,
}
impl AdamScreenCaptureTestRunListener {
    pub fn new(artifacts: ArtifactContext) -> Self {
        Self {
            artifacts,
            attachments: Attachments::default(),
        }
    }
}
impl AttachmentProvider for AdamScreenCaptureTestRunListener {
    fn register_listener(&self, listener: Arc<dyn AttachmentListener>) -> VendorResult<()> {
        self.attachments.register(listener)
    }
}
impl AndroidTestRunListener for AdamScreenCaptureTestRunListener {
    fn on_event_in<'a>(&'a self, event: &'a TestEvent, context: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if let TestEvent::TestEnded { id, metrics } = event {
                for (_, path) in metrics.entries().iter().filter(|(key, _)| *key == METRIC) {
                    let path = path.to_owned();
                    let pair = match path.rsplit('.').next().unwrap_or("") {
                        "jpeg" | "jpg" => Some((FileType::ScreenshotJpg, AttachmentType::ScreenshotJpeg)),
                        "png" => Some((FileType::ScreenshotPng, AttachmentType::ScreenshotPng)),
                        "webp" => Some((FileType::ScreenshotWebp, AttachmentType::ScreenshotWebp)),
                        "gif" => Some((FileType::ScreenshotGif, AttachmentType::ScreenshotGif)),
                        _ => None,
                    };
                    // N30/N31: wrong logger name and no pull despite promising a report file.
                    let logger = "ScreenCapturerTestRunListener";
                    if let Some((kind, attachment)) = pair {
                        let test = to_test(id);
                        let uuid = context
                            .hooks
                            .allocate_id("android.AdamScreenCaptureTestRunListener.testEnded")
                            .map_err(|e| other("ReplayError", e.message))?;
                        let file = self.artifacts.file(kind, Some(&test), Some(uuid), None).await?;
                        let local = file.clone();
                        let local = blocking(self.artifacts.device.runtime().dependencies.io.as_ref(), move || {
                            let cwd = std::env::current_dir().map_err(io)?;
                            let cwd = PathBuf::from(cwd.to_string_lossy().into_owned());
                            let absolute = jcompat::file_manager::absolute_file_path(
                                &local.to_string_lossy(),
                                &cwd.to_string_lossy(),
                            );
                            Ok(absolute.to_string_lossy().into_owned().to_owned())
                        })
                        .await?;
                        self.artifacts
                            .device
                            .safe_pull_file(&path, &local, context.clone())
                            .await?;
                        self.artifacts.device.log_named(
                            logger,
                            Level::Debug,
                            &format!("Received screen capture file {path}"),
                            None,
                        );
                        self.attachments.emit(&test, file, attachment, "screen")?;
                    } else {
                        self.artifacts.device.log_named(logger,Level::Warn,&format!("Unable to decode image format of screen capture {path}. The file will be available in the report directory, but will not be included as part of any visual report"),None);
                    }
                }
            }
            Ok(())
        })
    }
}
