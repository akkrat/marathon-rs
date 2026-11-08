//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/screenshot/ScreenCapturerTestRunListener.kt
use super::*;
use crate::listeners::{AndroidTestRunListener, to_test};
use jcompat::logging::Level;
use marathon_adb::testrunner::TestEvent;
use marathon_config::ScreenRecordingPolicy;
use marathon_core::{executor::shared::requested, runtime::*, screenshot::*, vendor::VendorContext};
pub struct ScreenCapturerTestRunListener {
    pub artifacts: ArtifactContext,
    policy: ScreenRecordingPolicy,
    capturer: Arc<ScreenCapturer>,
    job: Mutex<Option<Arc<dyn VendorTask>>>,
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
    async fn join(&self) -> bool {
        let job = lock(&self.job).take();
        if let Some(job) = job {
            job.cancel(requested("screen capturer stopped"));
            let _ = job.join().await;
            true
        } else {
            false
        }
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
                        "ScreenCapturerTestRunListener",
                        Level::Debug,
                        &format!(
                            "Starting recording for {}",
                            test.to_simple_safe_test_name('.').to_owned()
                        ),
                        None,
                    );
                    let capturer = self.capturer.clone();
                    *lock(&self.job) = Some(
                        self.artifacts
                            .device
                            .runtime()
                            .dependencies
                            .io
                            .spawn_cancellable(Box::new(move |signal| capturer.start_owned(&test, signal))),
                    );
                }
                TestEvent::TestFailed { .. } => lock(&self.state).0 = true,
                TestEvent::TestEnded { id, .. } => {
                    let test = to_test(id);
                    lock(&self.state).1 = None;
                    self.artifacts.device.log_named(
                        "ScreenCapturerTestRunListener",
                        Level::Debug,
                        &format!(
                            "Finished recording for {}",
                            test.to_simple_safe_test_name('.').to_owned()
                        ),
                        None,
                    );
                    let had_job = self.join().await;
                    if !lock(&self.state).0 && self.policy == ScreenRecordingPolicy::OnFailure {
                        if had_job {
                            let file = self
                                .artifacts
                                .file(FileType::Screenshot, Some(&test), None, None)
                                .await?
                                .to_file_path_buf()
                                .map_err(io)?;
                            blocking(self.artifacts.device.runtime().dependencies.io.as_ref(), move || {
                                let _ = std::fs::remove_file(file);
                                Ok(())
                            })
                            .await?;
                        }
                    } else {
                        for listener in self.attachments.listeners() {
                            let file = self
                                .artifacts
                                .file(FileType::Screenshot, Some(&test), None, None)
                                .await?;
                            listener.on_attachment(
                                &test,
                                Attachment {
                                    file,
                                    kind: AttachmentType::ScreenshotGif,
                                    name: Some("screen".into()),
                                },
                            )?;
                        }
                    }
                }
                TestEvent::TestRunFailed { .. } => {
                    self.join().await;
                    let test = lock(&self.state).1.clone();
                    if let Some(test) = test {
                        let file = self
                            .artifacts
                            .file(FileType::Screenshot, Some(&test), None, None)
                            .await?
                            .to_file_path_buf()
                            .map_err(io)?;
                        let path = file.clone();
                        let nonempty = blocking(self.artifacts.device.runtime().dependencies.io.as_ref(), move || {
                            Ok(path.metadata().map(|m| m.len() > 0).unwrap_or(false))
                        })
                        .await?;
                        if nonempty {
                            let batch = self
                                .artifacts
                                .file(FileType::Screenshot, None, None, None)
                                .await?
                                .to_file_path_buf()
                                .map_err(io)?;
                            blocking(self.artifacts.device.runtime().dependencies.io.as_ref(), move || {
                                let _ = std::fs::rename(file, batch);
                                Ok(())
                            })
                            .await?;
                        }
                    }
                }
                _ => {}
            }
            Ok(())
        })
    }
}
