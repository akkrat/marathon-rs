//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/video/ScreenRecordingListener.kt
use super::pending::PendingArtifacts;
use super::*;
use crate::apple_simulator_device::recording::{Recording, RecordingFinalization};
use marathon_apple::logparser::parser::DeviceFailureReason;
use marathon_config::vendor::{Codec, IosVideoConfiguration};

pub fn transcode_command(config: &IosVideoConfiguration, src: &str, temp: &str) -> Vec<String> {
    let size = config.transcoding.size.value();
    let optional = match config.codec {
        Codec::H264 => "-movflags +faststart",
        Codec::Hevc => "",
    };
    vec![
        "sh".into(),
        "-c".into(),
        format!(
            "{} -i {temp} -vf \"scale={size}:{size}:force_original_aspect_ratio=decrease:force_divisible_by=2\" -preset ultrafast {optional} {src}",
            config.transcoding.binary.to_owned()
        ),
    ]
}
pub struct ScreenRecordingListener {
    c: ListenerContext,
    logger: Component,
    recordings: Mutex<RecordingSlots>,
    pub attachments: Arc<AttachmentProviderDelegate>,
    /// Each remote video's last queued transfer: a test that runs again in the batch records to the same file.
    collecting: Mutex<std::collections::HashMap<String, Arc<dyn VendorTask>>>,
    pending: Arc<PendingArtifacts>,
}

#[derive(Default)]
struct RecordingSlots {
    active: Option<Arc<Recording>>,
    /// The last ended test's stop (D80), which the next recording and every later stop wait for.
    stopping: Option<Arc<Recording>>,
    last_path: Option<String>,
}

/// The transfer side of the listener, owned so that a test's video can be collected off the event path (D80).
#[derive(Clone)]
struct Media {
    c: ListenerContext,
    logger: Component,
    attachments: Arc<AttachmentProviderDelegate>,
}
impl ScreenRecordingListener {
    pub fn new(c: ListenerContext) -> Self {
        let logger = c.component("marathon_ios::listener::video");
        Self {
            c,
            logger,
            recordings: Mutex::default(),
            attachments: Arc::default(),
            collecting: Mutex::default(),
            pending: Arc::default(),
        }
    }
    /// The batch's queued video work, for the barrier that settles it before results are built.
    pub fn pending(&self) -> Arc<PendingArtifacts> {
        self.pending.clone()
    }
    /// Waits for the videos of the tests that have ended and returns the first failure among them.
    pub async fn settle(&self) -> VendorResult<()> {
        self.pending.settle().await;
        self.pending.take_error()
    }
    fn media(&self) -> Media {
        Media {
            c: self.c.clone(),
            logger: self.logger.clone(),
            attachments: self.attachments.clone(),
        }
    }
    fn stop_current(&self, healthy: bool) -> Option<Arc<Recording>> {
        let mut slots = self.recordings.lock().unwrap_or_else(|e| e.into_inner());
        let recording = slots.active.take()?;
        let context = &self.c.context;
        if healthy && context.cancellation.reason().is_none() {
            let shell = self
                .c
                .device
                .services()
                .vendor
                .timeout_configuration
                .shell
                .to_millis()
                .unwrap_or(i64::MAX);
            recording.request_healthy_stop(context.clock.as_ref(), shell, self.c.device.shutdown_deadline());
        } else {
            recording.request_stop(context.clock.as_ref(), self.c.device.shutdown_deadline());
        }
        slots.stopping = Some(recording.clone());
        Some(recording)
    }
    /// Joins the protected stop even when the batch context has been cancelled.
    pub async fn stop(&self) -> VendorResult<RecordingFinalization> {
        self.stop_with_budget(true).await
    }
    async fn stop_with_budget(&self, healthy: bool) -> VendorResult<RecordingFinalization> {
        match self.stop_current(healthy) {
            Some(recording) => recording.finalization().await,
            None => {
                let prior = self
                    .recordings
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .stopping
                    .clone();
                if let Some(prior) = prior {
                    if !healthy {
                        prior.request_stop(self.c.context.clock.as_ref(), None);
                    }
                    let _ = prior.finalization().await;
                }
                if self.c.device.uses_legacy_recording().await.unwrap_or(false) {
                    let _ = self.c.device.stop_legacy_recording_if_present(&self.c.context).await?;
                }
                Ok(RecordingFinalization::NotStarted)
            }
        }
    }
    async fn ended(&self, test: &Test, failed: bool) -> VendorResult<()> {
        // D80: stop, transcode and transfer stay off the test event path.
        let stop = self.stop_current(true);
        self.recordings.lock().unwrap_or_else(|e| e.into_inner()).last_path = None;
        let runtime = self.c.device.services().runtime.clone();
        let media = self.media();
        let src = self
            .c
            .device
            .remote_file_manager()
            .remote_video_for_test(test, &self.c.batch.id)
            .to_owned();
        let test = test.clone();
        let source = src.clone();
        let collected = self.pending.submit(
            runtime.as_ref(),
            self.c.context.cancellation.clone(),
            Box::pin(async move {
                let result = match stop {
                    Some(stop) => stop.finalization().await,
                    None => Ok(RecordingFinalization::NotStarted),
                };
                if media.recording_deliverable(&source, media.c.policy(failed), result)? {
                    media.collect(&test, failed).await?;
                }
                Ok(())
            }),
        );
        self.collecting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(src, collected);
        Ok(())
    }
}
impl Media {
    fn recording_deliverable(
        &self,
        src: &str,
        requested: bool,
        result: VendorResult<RecordingFinalization>,
    ) -> VendorResult<bool> {
        let reason = match result {
            Ok(RecordingFinalization::Finalized) => return Ok(true),
            Ok(RecordingFinalization::NotStarted) => "the recording did not start",
            Ok(RecordingFinalization::Unfinalized) => "the recording did not finalize",
            Ok(RecordingFinalization::Unconfirmed) => "recorder cleanup could not be confirmed",
            Err(error) => {
                if requested {
                    self.undelivered(src, &error.failure().to_string());
                }
                return match error {
                    VendorError::Transfer(_) => Ok(false),
                    other => Err(other),
                };
            }
        };
        if requested {
            self.undelivered(src, reason);
        }
        Ok(false)
    }
    async fn collect(&self, test: &Test, failed: bool) -> VendorResult<()> {
        self.catch_transfer(
            async {
                let remote = self.c.device.remote_file_manager();
                let src = remote.remote_video_for_test(test, &self.c.batch.id).to_owned();
                if !remote.test(&src, &self.c.context).await? {
                    if self.c.policy(failed) {
                        self.undelivered(&src, "the movie is missing on the worker");
                    }
                    return Ok(());
                }
                if !self.c.policy(failed) {
                    return self.remove(&src).await;
                }
                let temp = remote.remote_temp_video_for_test(test, &self.c.batch.id);
                self.deliver(&src, &temp, Some(test)).await
            }
            .await,
        )
    }
    async fn deliver(&self, src: &str, temp: &str, test: Option<&Test>) -> VendorResult<()> {
        let received = async {
            let source = if self.transcoding() && !self.transcode(src, temp).await? {
                temp
            } else {
                src
            };
            self.pull(source, test)
                .await
                .map(|delivered| delivered.then_some(source))
        }
        .await;
        match received {
            Ok(Some(source)) => self.remove(source).await,
            Ok(None) => {
                self.undelivered(src, "the receive did not complete");
                Ok(())
            }
            Err(error) => {
                self.undelivered(src, &error.failure().to_string());
                match error {
                    VendorError::Transfer(_) => Ok(()),
                    other => Err(other),
                }
            }
        }
    }
    fn undelivered(&self, src: &str, reason: &str) {
        self.c.log(
            &self.logger,
            Level::ERROR,
            format!("Video {src} was not delivered: {reason}"),
        );
    }
    async fn transcode(&self, src: &str, temp: &str) -> VendorResult<bool> {
        let moved = self
            .c
            .device
            .execute_worker_command(vec!["mv".into(), "-f".into(), src.into(), temp.into()], &self.c.context)
            .await?;
        if !moved.is_some_and(|result| result.successful()) {
            return Err(VendorError::Transfer(marathon_apple::cmd::failure(
                marathon_error::FailureKind::Transfer,
                format!("Could not stage video {src} for transcoding"),
            )));
        }
        let start = self.c.millis("ios.ScreenRecordingListener.transcode")?;
        let config = &self
            .c
            .device
            .services()
            .vendor
            .screen_record_configuration
            .video_configuration;
        let result = self
            .c
            .device
            .execute_worker_command(transcode_command(config, src, temp), &self.c.context)
            .await?;
        let succeeded = result.as_ref().is_some_and(|result| result.successful());
        if let Some(result) = result.filter(|r| !r.successful()) {
            self.c.log(
                &self.logger,
                Level::ERROR,
                format!(
                    "Transcoding failed for {src}: {}, {}",
                    result.combined_stdout(),
                    result.combined_stderr()
                ),
            );
        }
        let elapsed = self
            .c
            .millis("ios.ScreenRecordingListener.transcode")?
            .wrapping_sub(start);
        if succeeded {
            self.c.log(
                &self.logger,
                Level::DEBUG,
                format!("Transcoding finished in {elapsed}ms {src}"),
            );
        }
        Ok(succeeded)
    }
    async fn pull(&self, src: &str, test: Option<&Test>) -> VendorResult<bool> {
        let local = self.c.file(FileType::Video, test).await?;
        let label = if test.is_some() {
            "ios.ScreenRecordingListener.pullTestVideo"
        } else {
            "ios.ScreenRecordingListener.pullLastBatchVideo"
        };
        let start = self.c.millis(label)?;
        if !self.c.device.pull_file(src, &local, &self.c.context).await? {
            return Ok(false);
        }
        self.c.log(
            &self.logger,
            Level::DEBUG,
            format!(
                "Pulling finished in {}ms {src}",
                self.c.millis(label)?.wrapping_sub(start)
            ),
        );
        if let Some(test) = test {
            self.attachments.on_attachment(
                test,
                Attachment {
                    file: local,
                    kind: AttachmentType::Video,
                    name: Some("screen".into()),
                },
            )?;
        }
        Ok(true)
    }
    async fn remove(&self, src: &str) -> VendorResult<()> {
        let start = self.c.millis("ios.ScreenRecordingListener.removeRemoteVideo")?;
        self.c
            .device
            .remote_file_manager()
            .remove_remote_path(src, &self.c.context)
            .await?;
        self.c.log(
            &self.logger,
            Level::DEBUG,
            format!(
                "Removed file in {}ms {src}",
                self.c
                    .millis("ios.ScreenRecordingListener.removeRemoteVideo")?
                    .wrapping_sub(start)
            ),
        );
        Ok(())
    }
    fn catch_transfer(&self, result: VendorResult<()>) -> VendorResult<()> {
        match result {
            Err(VendorError::Transfer(_)) => {
                self.c.log(&self.logger, Level::WARN, "Can't pull video".into());
                Ok(())
            }
            other => other,
        }
    }
    fn transcoding(&self) -> bool {
        self.c.device.supports_transcoding()
            && self
                .c
                .device
                .services()
                .vendor
                .screen_record_configuration
                .video_configuration
                .transcoding
                .enabled
    }
}
impl ScreenRecordingListener {
    async fn healthy(&self) -> bool {
        let healthy = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let out = healthy.clone();
        let c = self.c.clone();
        // A44 precedent: Kotlin catches cancellation from the worker command too.
        let outcome = self
            .c
            .context
            .clock
            .with_timeout_or_null(
                self.c.context.cancellation.clone(),
                10_000,
                Box::new(move |signal| {
                    Box::pin(async move {
                        let mut context = c.context.clone();
                        context.cancellation = signal;
                        if let Ok(Some(r)) = c
                            .device
                            .execute_worker_command(vec!["echo".into(), "quickhealthcheck".into()], &context)
                            .await
                        {
                            out.store(
                                r.combined_stdout().contains("quickhealthcheck"),
                                std::sync::atomic::Ordering::Release,
                            );
                        }
                        Ok(())
                    })
                }),
            )
            .await;
        outcome.ok().flatten().is_some() && healthy.load(std::sync::atomic::Ordering::Acquire)
    }
}
impl AppleTestRunListener for ScreenRecordingListener {
    fn before_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { self.stop().await.map(|_| ()) })
    }
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let stopped = self.stop().await.map(|_| ());
            self.settle().await.and(stopped)
        })
    }
    fn test_started<'a>(&'a self, test: &'a Test) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let previous = self.stop_current(true).or_else(|| {
                self.recordings
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .stopping
                    .clone()
            });
            let src = self
                .c
                .device
                .remote_file_manager()
                .remote_video_for_test(test, &self.c.batch.id)
                .to_owned();
            let collecting = self
                .collecting
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&src)
                .cloned();
            // D80: a repeated test also waits for its previous transfer to finish using this path.
            let ready = Box::pin(async move {
                if let Some(previous) = previous
                    && previous.finalization().await? == RecordingFinalization::Unconfirmed
                {
                    return Ok(false);
                }
                if let Some(collecting) = collecting {
                    let _ = collecting.join().await;
                }
                Ok(true)
            });
            let mut slots = self.recordings.lock().unwrap_or_else(|e| e.into_inner());
            let recording = match self
                .c
                .device
                .begin_recording(src.clone(), self.c.context.clone(), ready)
            {
                Ok(recording) => recording,
                Err(error) => {
                    self.c.log(
                        &self.logger,
                        Level::DEBUG,
                        format!("Video recording was refused: {}", error.failure().for_log()),
                    );
                    return Ok(());
                }
            };
            slots.last_path = Some(src);
            slots.active = Some(recording);
            Ok(())
        })
    }
    fn test_failed<'a>(&'a self, test: &'a Test, _: i64, _: i64, _: Option<&'a str>) -> VendorFuture<'a, ()> {
        Box::pin(self.ended(test, true))
    }
    fn test_passed<'a>(&'a self, test: &'a Test, _: i64, _: i64) -> VendorFuture<'a, ()> {
        Box::pin(self.ended(test, false))
    }
    fn test_ignored<'a>(&'a self, test: &'a Test, _: i64, _: i64) -> VendorFuture<'a, ()> {
        Box::pin(self.ended(test, false))
    }
    fn test_run_failed<'a>(&'a self, _: &'a str, _: DeviceFailureReason) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            // The ended tests' videos go first, as when they were pulled inline; their failure waits for after_test_run.
            let stopped = self.stop_with_budget(false).await;
            self.pending.settle().await;
            let media = self.media();
            media.catch_transfer(
                async {
                    let healthy = self.healthy().await;
                    if !healthy {
                        self.c.log(
                            &self.logger,
                            Level::WARN,
                            "Device is unhealthy, can't pull batch fail video".into(),
                        );
                    }
                    let src = self
                        .recordings
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .last_path
                        .take();
                    if let Some(src) = src
                        && media.recording_deliverable(&src, true, stopped)?
                    {
                        if !healthy {
                            media.undelivered(&src, "the device is unhealthy");
                            return Ok(());
                        }
                        if !self.c.device.remote_file_manager().test(&src, &self.c.context).await? {
                            media.undelivered(&src, "the movie is missing on the worker");
                            return Ok(());
                        }
                        media.deliver(&src, &format!("{src}.tmp"), None).await?;
                    }
                    Ok(())
                }
                .await,
            )
        })
    }
}

impl marathon_core::listener::AttachmentProvider for ScreenRecordingListener {
    fn register_listener(&self, listener: Arc<dyn marathon_core::listener::AttachmentListener>) -> VendorResult<()> {
        self.attachments.register_listener(listener)
    }
}
