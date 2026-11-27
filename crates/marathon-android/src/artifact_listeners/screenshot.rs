//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/screenshot/ScreenCapturerTestRunListener.kt
use super::*;
use crate::listeners::{AndroidTestRunListener, to_test};
use marathon_adb::testrunner::TestEvent;
use marathon_config::ScreenRecordingPolicy;
use marathon_core::{executor::shared::requested, runtime::*, screenshot::*, vendor::VendorContext};
use marathon_logging::Level;
use std::sync::atomic::{AtomicBool, Ordering};

struct Capture {
    job: Arc<dyn VendorTask>,
    delivered: Arc<AtomicBool>,
}
pub struct ScreenCapturerTestRunListener {
    pub artifacts: ArtifactContext,
    policy: ScreenRecordingPolicy,
    capturer: Arc<ScreenCapturer>,
    job: Mutex<Option<Capture>>,
    state: Mutex<(bool, Option<Test>)>,
    attachments: Attachments,
}
impl ScreenCapturerTestRunListener {
    pub fn new(artifacts: ArtifactContext, policy: ScreenRecordingPolicy) -> Self {
        let runtime = artifacts.device.runtime();
        let deps = &runtime.dependencies;
        let c = &runtime
            .configuration
            .screen_record_configuration
            .screenshot_configuration;
        let capturer = ScreenCapturer::new(
            CaptureSettings {
                device: artifacts.device.info(),
                pool: artifacts.pool.clone(),
                batch_id: artifacts.batch.id.clone(),
                delay: jcompat::jtime::duration_of(0, c.delay_ms as i64 * 1_000_000).unwrap(),
                height: c.height,
                width: c.width,
                timeout: runtime.configuration.timeout_configuration.screencapturer,
            },
            CaptureServices {
                io: deps.io.clone(),
                clock: deps.clock.clone(),
                wall: deps.hooks.clone(),
                files: deps.files.clone(),
                log: deps.log.clone(),
            },
            Arc::new(artifacts.device.clone()),
            &mut lock(&deps.logging),
        );
        Self {
            artifacts,
            policy,
            capturer: Arc::new(capturer),
            job: Mutex::new(None),
            state: Mutex::new((false, None)),
            attachments: Attachments::default(),
        }
    }
    pub fn captures_taken(&self) -> i32 {
        self.capturer.captures_taken()
    }
    async fn join(&self) -> Option<bool> {
        let capture = lock(&self.job).take()?;
        capture.job.cancel(requested("screen capturer stopped"));
        let _ = capture.job.join().await;
        Some(capture.delivered.load(Ordering::Acquire))
    }
}
impl AttachmentProvider for ScreenCapturerTestRunListener {
    fn register_listener(&self, l: Arc<dyn AttachmentListener>) -> VendorResult<()> {
        self.attachments.register(l)
    }
}
impl AndroidTestRunListener for ScreenCapturerTestRunListener {
    fn cleanup_in(&self, _context: VendorContext) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.join().await;
            if self.artifacts.device.stopping() {
                let test = lock(&self.state).1.take();
                if let Some(test) = test {
                    let file = self
                        .artifacts
                        .file(FileType::Screenshot, Some(&test), None, None)
                        .await?;
                    match tokio::fs::remove_file(file).await {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(io(e)),
                    }
                }
            }
            Ok(())
        })
    }
    fn on_event_in<'a>(&'a self, event: &'a TestEvent, _context: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            match event {
                TestEvent::TestStarted { id } => {
                    let test = to_test(id);
                    *lock(&self.state) = (false, Some(test.clone()));
                    self.artifacts.device.log_named(
                        module_path!(),
                        Level::DEBUG,
                        &format!(
                            "Starting recording for {}",
                            test.to_simple_safe_test_name('.').to_owned()
                        ),
                        None,
                    );
                    let capturer = self.capturer.clone();
                    let delivered = Arc::new(AtomicBool::new(false));
                    let receipt = delivered.clone();
                    let job = self.artifacts.device.spawn_owned(Box::new(move |signal| {
                        capturer.start_owned_with_delivery(&test, signal, receipt)
                    }))?;
                    *lock(&self.job) = Some(Capture { job, delivered });
                }
                TestEvent::TestFailed { .. } => lock(&self.state).0 = true,
                TestEvent::TestEnded { id, .. } => {
                    let test = to_test(id);
                    lock(&self.state).1 = None;
                    self.artifacts.device.log_named(
                        module_path!(),
                        Level::DEBUG,
                        &format!(
                            "Finished recording for {}",
                            test.to_simple_safe_test_name('.').to_owned()
                        ),
                        None,
                    );
                    if let Some(delivered) = self.join().await {
                        let file = self
                            .artifacts
                            .file(FileType::Screenshot, Some(&test), None, None)
                            .await?;
                        let keep = (lock(&self.state).0 || self.policy == ScreenRecordingPolicy::OnAny)
                            && self.captures_taken() > 0
                            && delivered;
                        if keep {
                            self.attachments
                                .emit(&test, file, AttachmentType::ScreenshotGif, "screen")?;
                        } else {
                            let _ = tokio::fs::remove_file(file).await;
                        }
                    }
                }
                TestEvent::TestRunFailed { .. } => {
                    let delivered = self.join().await == Some(true);
                    let test = lock(&self.state).1.clone();
                    if let Some(test) = test {
                        let file = self
                            .artifacts
                            .file(FileType::Screenshot, Some(&test), None, None)
                            .await?;
                        if delivered {
                            let batch = self.artifacts.file(FileType::Screenshot, None, None, None).await?;
                            blocking(self.artifacts.device.runtime().dependencies.io.as_ref(), move || {
                                let _ = std::fs::rename(file, batch);
                                Ok(())
                            })
                            .await?;
                        } else {
                            let _ = tokio::fs::remove_file(file).await;
                        }
                    }
                }
                _ => {}
            }
            Ok(())
        })
    }
}
