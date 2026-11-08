//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/screenshot/ScreenCapturerTestRunListener.kt
use super::*;
use marathon_apple::logparser::parser::DeviceFailureReason;
use marathon_core::screenshot::{CaptureServices, CaptureSettings, ScreenCapturer};
pub struct ScreenCapturerTestRunListener {
    c: ListenerContext,
    logger: Logger,
    capturer: Arc<ScreenCapturer>,
    job: Mutex<Option<Arc<dyn VendorTask>>>,
    last: Mutex<Option<Test>>,
    pub attachments: Arc<AttachmentProviderDelegate>,
}
impl ScreenCapturerTestRunListener {
    pub fn new(c: ListenerContext) -> Self {
        let logger = c.logger("ScreenCapturerTestRunListener");
        let s = c.device.services();
        let config = &s.vendor.screen_record_configuration.screenshot_configuration;
        let logging = s.logging.clone();
        let capturer = ScreenCapturer::new(
            CaptureSettings {
                device: c.device.info(),
                pool: c.pool.clone(),
                batch_id: c.batch.id.clone(),
                delay: config.delay,
                height: config.height,
                width: config.width,
                timeout: s.vendor.timeout_configuration.screenshot,
            },
            CaptureServices {
                io: s.runtime.clone(),
                clock: c.context.clock.clone(),
                wall: s.wall.clone(),
                files: s.output.clone(),
                log: Arc::new(move |logger, level, message, cause| {
                    if logging.enabled(logger, level) {
                        (logging.sink)(logger, level, message.to_owned(), cause.cloned());
                    }
                }),
            },
            c.device.clone(),
            &mut s.logging.registry.lock().unwrap_or_else(|e| e.into_inner()),
        );
        Self {
            c,
            logger,
            capturer: Arc::new(capturer),
            job: Mutex::new(None),
            last: Mutex::new(None),
            attachments: Arc::default(),
        }
    }
    async fn ended(&self, test: &Test, failed: bool) -> VendorResult<()> {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.c.log(
            &self.logger,
            Level::Debug,
            format!(
                "Finished recording for {}",
                test.to_simple_safe_test_name('.').to_owned()
            ),
        );
        self.c.cancel(&self.job).await?;
        let file = self.c.file(FileType::Screenshot, Some(test)).await?;
        if self.c.policy(failed) && self.capturer.captures_taken() > 0 {
            self.attachments.on_attachment(
                test,
                Attachment {
                    file,
                    kind: AttachmentType::ScreenshotGif,
                    name: Some("screen".into()),
                },
            )?;
        } else {
            blocking(self.c.device.services().runtime.as_ref(), move || {
                if let Ok(path) = file.to_file_path_buf() {
                    let _ = std::fs::remove_file(path);
                }
                Ok(())
            })
            .await?;
        }
        Ok(())
    }
}
impl AppleTestRunListener for ScreenCapturerTestRunListener {
    fn test_started<'a>(&'a self, test: &'a Test) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.c.cancel(&self.job).await?;
            *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(test.clone());
            self.c.log(
                &self.logger,
                Level::Debug,
                format!(
                    "Starting recording for {}",
                    test.to_simple_safe_test_name('.').to_owned()
                ),
            );
            let capturer = self.capturer.clone();
            let test = test.clone();
            let job = self
                .c
                .device
                .services()
                .runtime
                .spawn_cancellable(Box::new(move |signal| capturer.start_owned(&test, signal)));
            *self.job.lock().unwrap_or_else(|e| e.into_inner()) = Some(job);
            Ok(())
        })
    }
    fn test_failed<'a>(&'a self, test: &'a Test, _: i64, _: i64, _: Option<&'a String>) -> VendorFuture<'a, ()> {
        Box::pin(self.ended(test, true))
    }
    fn test_passed<'a>(&'a self, test: &'a Test, _: i64, _: i64) -> VendorFuture<'a, ()> {
        Box::pin(self.ended(test, false))
    }
    fn test_ignored<'a>(&'a self, test: &'a Test, _: i64, _: i64) -> VendorFuture<'a, ()> {
        Box::pin(self.ended(test, false))
    }
    fn test_run_failed<'a>(&'a self, _: &'a String, _: DeviceFailureReason) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.c.cancel(&self.job).await?;
            let test = self.last.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(test) = test {
                let src = self.c.file(FileType::Screenshot, Some(&test)).await?;
                // Kotlin File.length returns zero on a failed stat; renameTo's Boolean is ignored.
                let nonempty = {
                    let src = src.clone();
                    blocking(self.c.device.services().runtime.as_ref(), move || {
                        Ok(src
                            .to_file_path_buf()
                            .ok()
                            .and_then(|p| std::fs::metadata(p).ok())
                            .is_some_and(|m| m.len() > 0))
                    })
                    .await?
                };
                if nonempty {
                    let dst = self.c.file(FileType::Screenshot, None).await?;
                    blocking(self.c.device.services().runtime.as_ref(), move || {
                        if let (Ok(src), Ok(dst)) = (src.to_file_path_buf(), dst.to_file_path_buf()) {
                            let _ = std::fs::rename(src, dst);
                        }
                        Ok(())
                    })
                    .await?;
                }
            }
            Ok(())
        })
    }
}

impl marathon_core::listener::AttachmentProvider for ScreenCapturerTestRunListener {
    fn register_listener(&self, listener: Arc<dyn marathon_core::listener::AttachmentListener>) -> VendorResult<()> {
        self.attachments.register_listener(listener)
    }
}
