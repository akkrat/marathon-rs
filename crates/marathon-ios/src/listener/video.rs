//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/video/ScreenRecordingListener.kt
use super::pending::PendingArtifacts;
use super::*;
use marathon_apple::logparser::parser::DeviceFailureReason;
use marathon_config::vendor::{Codec, IosVideoConfiguration};

/// P1: an empty pidfile is treated like an absent one, matching stop().
pub async fn await_removal<'a>(
    clock: &'a dyn VendorClock,
    cancellation: Arc<dyn CancellationSignal>,
    timeout: i64,
    interval: i64,
    on_waiting: impl FnOnce() + Send + 'a,
    mut read: impl FnMut(Arc<dyn CancellationSignal>) -> VendorFuture<'a, Option<String>> + Send + 'a,
) -> VendorResult<bool> {
    Ok(clock
        .with_timeout_or_null(
            cancellation,
            timeout,
            Box::new(move |signal| {
                Box::pin(async move {
                    let present = |v: Option<String>| v.is_some_and(|s| !super::trim(&s).is_empty());
                    if present(read(signal.clone()).await?) {
                        on_waiting();
                        loop {
                            clock.delay(interval, signal.clone()).await?;
                            if !present(read(signal.clone()).await?) {
                                break;
                            }
                        }
                    }
                    Ok(())
                })
            }),
        )
        .await?
        .is_some())
}
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
    job: Mutex<Option<Arc<dyn VendorTask>>>,
    last: Mutex<Option<String>>,
    pub attachments: Arc<AttachmentProviderDelegate>,
    /// The last ended test's stop (D80), which the next recording and every later stop wait for.
    stopping: Mutex<Option<Arc<dyn VendorTask>>>,
    pending: Arc<PendingArtifacts>,
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
            job: Mutex::new(None),
            last: Mutex::new(None),
            attachments: Arc::default(),
            stopping: Mutex::new(None),
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
    async fn stop(&self) -> VendorResult<()> {
        let stopping = self.stopping.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(stopping) = stopping {
            // Its failure is reported with its test's video.
            let _ = stopping.join().await;
        }
        let job = self.job.lock().unwrap_or_else(|e| e.into_inner()).take();
        stop_recording(&self.c, &self.logger, job).await
    }
    async fn ended(&self, test: &Test, failed: bool) -> VendorResult<()> {
        // D80: the stop, transcode and pull run beside the next tests instead of holding back their events (U201).
        // The test's own recording job is taken now, so its stop cannot reach the next test's.
        let job = self.job.lock().unwrap_or_else(|e| e.into_inner()).take();
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let runtime = self.c.device.services().runtime.clone();
        let stop = {
            let mut stopping = self.stopping.lock().unwrap_or_else(|e| e.into_inner());
            let previous = stopping.take();
            let (c, logger) = (self.c.clone(), self.logger.clone());
            let stop = runtime.spawn(Box::pin(async move {
                if let Some(previous) = previous {
                    let _ = previous.join().await;
                }
                stop_recording(&c, &logger, job).await
            }));
            *stopping = Some(stop.clone());
            stop
        };
        let media = self.media();
        let test = test.clone();
        self.pending.submit(
            runtime.as_ref(),
            self.c.context.cancellation.clone(),
            Box::pin(async move {
                // A failed stop skips the pull, as Kotlin's inline stop did.
                stop.join().await?;
                media.collect(&test, failed).await
            }),
        );
        Ok(())
    }
}
/// Kotlin's stop(): ends `job`, the recording's start, then the recorder whose pid is in the pidfile.
async fn stop_recording(c: &ListenerContext, logger: &Component, job: Option<Arc<dyn VendorTask>>) -> VendorResult<()> {
    if let Some(job) = job {
        // Cancel a recording that is still waiting to start first, so that it can't start after the pidfile check.
        job.cancel(marathon_apple::cmd::cancelled("Recording finished"));
        // Kotlin cancelAndJoin does not rethrow the joined child's failure.
        let _ = job.join().await;
    }
    let path = c.device.remote_file_manager().remote_video_pidfile();
    if let Some(pid) = c.device.read_textfile(&path, &c.context).await? {
        let pid = super::trim(&pid);
        if !pid.is_empty() {
            c.device.stop_video_recording(&c.context).await?;
            c.log(logger, Level::DEBUG, format!("stopped video recording pid={pid}"));
        }
    }
    Ok(())
}
impl Media {
    async fn collect(&self, test: &Test, failed: bool) -> VendorResult<()> {
        self.catch_transfer(
            async {
                let remote = self.c.device.remote_file_manager();
                let src = remote.remote_video_for_test(test, &self.c.batch.id).to_owned();
                if !remote.test(&src, &self.c.context).await? {
                    return Ok(());
                }
                if self.c.policy(failed) {
                    if self.transcoding() {
                        self.transcode(
                            &src,
                            &remote.remote_temp_video_for_test(test, &self.c.batch.id).to_owned(),
                        )
                        .await?;
                    }
                    self.pull(&src, Some(test)).await?;
                }
                self.remove(&src).await
            }
            .await,
        )
    }
    async fn transcode(&self, src: &str, temp: &str) -> VendorResult<()> {
        self.c
            .device
            .remote_file_manager()
            .move_file(src, temp, &self.c.context)
            .await?;
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
        self.c.log(
            &self.logger,
            Level::DEBUG,
            format!(
                "Transcoding finished in {}ms {src}",
                self.c
                    .millis("ios.ScreenRecordingListener.transcode")?
                    .wrapping_sub(start)
            ),
        );
        Ok(())
    }
    async fn pull(&self, src: &str, test: Option<&Test>) -> VendorResult<()> {
        let local = self.c.file(FileType::Video, test).await?;
        let label = if test.is_some() {
            "ios.ScreenRecordingListener.pullTestVideo"
        } else {
            "ios.ScreenRecordingListener.pullLastBatchVideo"
        };
        let start = self.c.millis(label)?;
        self.c.device.pull_file(src, &local, &self.c.context).await?;
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
        Ok(())
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
        Box::pin(self.stop())
    }
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let stopped = self.stop().await;
            self.settle().await.and(stopped)
        })
    }
    fn test_started<'a>(&'a self, test: &'a Test) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.c.cancel(&self.job).await?;
            let src = self
                .c
                .device
                .remote_file_manager()
                .remote_video_for_test(test, &self.c.batch.id)
                .to_owned();
            *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(src.clone());
            let c = self.c.clone();
            let logger = self.logger.clone();
            let stopping = self.stopping.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let job = c.device.services().runtime.clone().spawn_cancellable(Box::new(move |signal| {
                Box::pin(async move {
                    let mut c = c;
                    c.context.cancellation = signal;
                    if let Some(stopping) = stopping {
                        // D80: the previous test's recording is stopped off the event path; this one starts after it,
                        // as it did when that stop ran before this callback.
                        tokio::select! {
                            biased;
                            reason = c.context.cancellation.cancelled() => return Err(VendorError::Cancelled(reason)),
                            _ = stopping.join() => {}
                        }
                    }
                    let pidfile = c.device.remote_file_manager().remote_video_pidfile();
                    let done = await_removal(
                        c.context.clock.as_ref(),
                        c.context.cancellation.clone(),
                        10_000,
                        100,
                        || {
                            c.log(
                                &logger,
                                Level::WARN,
                                "Previous video recording didn't finish yet. Current recording might be missing or cut from the start".into(),
                            );
                        },
                        |signal| {
                            let d = c.device.clone();
                            let p = pidfile.clone();
                            let mut context = c.context.clone();
                            context.cancellation = signal;
                            Box::pin(async move { d.read_textfile(&p, &context).await })
                        },
                    )
                    .await?;
                    if !done {
                        c.log(
                            &logger,
                            Level::WARN,
                            "Previous video recording didn't finish in 10s, starting a new one anyway".into(),
                        );
                    }
                    if c.device
                        .start_video_recording(&src, &c.context)
                        .await?
                        .is_some_and(|r| r.exit_code == Some(0))
                    {
                        let pid = c
                            .device
                            .read_textfile(&pidfile, &c.context)
                            .await?
                            .map(|p| super::trim(&p).to_owned())
                            .unwrap_or_else(|| "null".into());
                        c.log(&logger, Level::DEBUG, format!("started video recording pid={pid} to {src}"));
                    }
                    Ok(())
                })
            }));
            *self.job.lock().unwrap_or_else(|e| e.into_inner()) = Some(job);
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
            self.pending.settle().await;
            let media = self.media();
            media.catch_transfer(
                async {
                    if !self.healthy().await {
                        self.c.log(
                            &self.logger,
                            Level::WARN,
                            "Device is unhealthy, can't pull batch fail video".into(),
                        );
                        return Ok(());
                    }
                    self.stop().await?;
                    let src = self.last.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if let Some(src) = src {
                        if !self.c.device.remote_file_manager().test(&src, &self.c.context).await? {
                            return Ok(());
                        }
                        if media.transcoding() {
                            media.transcode(&src, &format!("{src}.tmp")).await?;
                        }
                        media.pull(&src, None).await?;
                        media.remove(&src).await?;
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
