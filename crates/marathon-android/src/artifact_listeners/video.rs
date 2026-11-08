//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/video/ScreenRecorder.kt
//! and vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/video/ScreenRecorderTestBatchListener.kt.
use super::*;
use crate::listeners::{AndroidTestRunListener, to_test};
use jcompat::logging::Level;
use marathon_adb::testrunner::TestEvent;
use marathon_config::ScreenRecordingPolicy;
use marathon_core::{executor::shared::requested, runtime::*, vendor::VendorContext};

/// Kotlin splits only on spaces, uses the last nonempty line, and normalizes the Int.
pub fn grep_pid(output: &str) -> String {
    let last = output.split(['\r', '\n']).rfind(|s| !s.is_empty()).unwrap_or("");
    last.split(' ')
        .filter(|s| !s.chars().all(jcompat::jstring::is_whitespace))
        .nth(1)
        .and_then(jcompat::jnum::parse_int)
        .map(|i| i.to_string())
        .unwrap_or_default()
}
pub async fn stop_screen_record(device: &AndroidDevice, context: VendorContext) -> VendorResult<()> {
    device.log_named("ScreenRecorder", Level::Debug, "Stopping screen recorder", None);
    for _ in 0..5 {
        let killed: VendorResult<bool> = async {
            let cmd = if device.version().is_greater_or_equal_than(26) {
                "ps -A | grep screenrecord"
            } else {
                "ps | grep screenrecord"
            };
            let output = device
                .safe_execute_shell_command(cmd, "", context.clone())
                .await?
                .map(|r| r.output.to_owned())
                .unwrap_or_default();
            let pid = grep_pid(&output);
            if !output.chars().all(jcompat::jstring::is_whitespace) {
                device.log_named(
                    "ScreenRecorder",
                    Level::Trace,
                    &format!("Extracted PID {pid} from output {output}"),
                    None,
                );
            }
            if pid.is_empty() {
                device.log_named(
                    "ScreenRecorder",
                    Level::Warn,
                    "Did not kill any screen recording process",
                    None,
                );
                return Ok(false);
            }
            device.log_named(
                "ScreenRecorder",
                Level::Trace,
                &format!("Killing PID {pid} on {}", device.serial_number().to_owned()),
                None,
            );
            device
                .safe_execute_shell_command(&format!("kill -2 {pid}"), "", context.clone())
                .await?;
            Ok(true)
        }
        .await;
        let killed = match killed {
            Ok(v) => v,
            Err(e) => {
                device.log_named(
                    "ScreenRecorder",
                    Level::Error,
                    "Error while killing recording processes",
                    Some(e.failure()),
                );
                false
            }
        };
        context.clock.delay(300, context.cancellation.clone()).await?;
        if !killed {
            break;
        }
    }
    Ok(())
}
#[derive(Default)]
struct Recording {
    failed: bool,
    cancelling: bool,
    chunks: std::collections::HashMap<Test, i64>,
    last: Vec<String>,
}
pub struct ScreenRecorderTestBatchListener {
    pub artifacts: ArtifactContext,
    policy: ScreenRecordingPolicy,
    chunked: bool,
    state: Arc<Mutex<Recording>>,
    job: Mutex<Option<Arc<dyn VendorTask>>>,
    attachments: Attachments,
}
impl ScreenRecorderTestBatchListener {
    pub fn new(artifacts: ArtifactContext, policy: ScreenRecordingPolicy) -> Self {
        let chunked = artifacts.device.api_level() < 34;
        Self {
            artifacts,
            policy,
            chunked,
            state: Arc::default(),
            job: Mutex::new(None),
            attachments: Attachments::default(),
        }
    }
    fn remote(&self, test: &Test, chunk: i64) -> VendorResult<String> {
        self.artifacts.device.file_manager().remote_chunked_video_for_test(
            test,
            &self.artifacts.batch.id,
            self.chunked.then_some(chunk),
        )
    }
    async fn join(&self) {
        let job = lock(&self.job).take();
        if let Some(job) = job {
            job.cancel(requested("screen recorder stopped"));
            let _ = job.join().await;
        }
    }
    async fn stop(&self, context: VendorContext) -> VendorResult<()> {
        lock(&self.state).cancelling = true;
        let start = self.artifacts.now("android.ScreenRecorderTestBatchListener.stop")?;
        let result = stop_screen_record(&self.artifacts.device, context).await;
        if result.is_err() {
            self.join().await;
            return result;
        }
        let end = self.artifacts.now("android.ScreenRecorderTestBatchListener.stop")?;
        self.artifacts.device.log_named(
            "ScreenRecorderTestBatchListener",
            Level::Trace,
            &format!("stop {}ms", end.wrapping_sub(start)),
            None,
        );
        let start = self.artifacts.now("android.ScreenRecorderTestBatchListener.join")?;
        self.artifacts
            .device
            .log_named("ScreenRecorderTestBatchListener", Level::Trace, "cancel", None);
        self.join().await;
        let end = self.artifacts.now("android.ScreenRecorderTestBatchListener.join")?;
        self.artifacts.device.log_named(
            "ScreenRecorderTestBatchListener",
            Level::Trace,
            &format!("join {}ms", end.wrapping_sub(start)),
            None,
        );
        Ok(())
    }
    async fn pull(
        &self,
        remote: &str,
        test: Option<&Test>,
        chunk: Option<String>,
        context: VendorContext,
    ) -> VendorResult<JavaPath> {
        let local = self.artifacts.file(FileType::Video, test, None, chunk).await?;
        let label = if test.is_some() {
            "android.ScreenRecorderTestBatchListener.pullTestVideoFile"
        } else {
            "android.ScreenRecorderTestBatchListener.pullLastBatchVideo"
        };
        let start = self.artifacts.now(label)?;
        self.artifacts
            .device
            .safe_pull_file(remote, &String::from_utf16_lossy(&local.0.clone()).to_owned(), context)
            .await?;
        let end = self.artifacts.now(label)?;
        self.artifacts.device.log_named(
            "ScreenRecorderTestBatchListener",
            Level::Trace,
            &format!(
                "Pulling finished in {}ms {} ",
                end.wrapping_sub(start),
                remote.to_owned()
            ),
            None,
        );
        Ok(local)
    }
    async fn remove(&self, remote: &str, context: VendorContext) -> VendorResult<()> {
        let start = self
            .artifacts
            .now("android.ScreenRecorderTestBatchListener.removeRemoteVideoFile")?;
        self.artifacts
            .device
            .file_manager()
            .remove_remote_path(remote, false, context)
            .await?;
        let end = self
            .artifacts
            .now("android.ScreenRecorderTestBatchListener.removeRemoteVideoFile")?;
        self.artifacts.device.log_named(
            "ScreenRecorderTestBatchListener",
            Level::Trace,
            &format!("Removed file in {}ms {}", end.wrapping_sub(start), remote.to_owned()),
            None,
        );
        Ok(())
    }
    async fn finish_test(&self, test: &Test, context: VendorContext) -> VendorResult<()> {
        self.stop(context.clone()).await?;
        let (failed, count) = {
            let state = lock(&self.state);
            (state.failed, *state.chunks.get(test).unwrap_or(&0))
        };
        if self.policy == ScreenRecordingPolicy::OnAny || failed {
            for i in 0..=count {
                let file = self
                    .pull(
                        &self.remote(test, i)?,
                        Some(test),
                        self.chunked.then(|| i.to_string().as_str().to_owned()),
                        context.clone(),
                    )
                    .await?;
                let name = if self.chunked {
                    format!("screen-{i}")
                } else {
                    "screen".into()
                };
                self.attachments.emit(test, file, AttachmentType::Video, &name)?;
            }
        }
        for i in 0..=count {
            self.remove(&self.remote(test, i)?, context.clone()).await?;
        }
        Ok(())
    }
}
impl AttachmentProvider for ScreenRecorderTestBatchListener {
    fn register_listener(&self, listener: Arc<dyn AttachmentListener>) -> VendorResult<()> {
        self.attachments.register(listener)
    }
}
impl AndroidTestRunListener for ScreenRecorderTestBatchListener {
    fn cleanup_in(&self, _context: VendorContext) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            lock(&self.state).cancelling = true;
            self.join().await;
            Ok(())
        })
    }
    fn on_event_in<'a>(&'a self, event: &'a TestEvent, context: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            match event {
                TestEvent::TestStarted { id } => {
                    let test = to_test(id);
                    let first = self.remote(&test, 0)?;
                    {
                        let mut state = lock(&self.state);
                        state.failed = false;
                        state.cancelling = false;
                        state.chunks.insert(test.clone(), 0);
                        state.last = vec![first.clone()];
                    }
                    let artifacts = self.artifacts.clone();
                    let state = self.state.clone();
                    let chunked = self.chunked;
                    let job = self
                        .artifacts
                        .device
                        .runtime()
                        .dependencies
                        .io
                        .spawn_cancellable(Box::new(move |signal| {
                            Box::pin(async move {
                                let context = VendorContext {
                                    cancellation: signal,
                                    ..context
                                };
                                let mut remote = first;
                                loop {
                                    let result = async {
                                        let start = artifacts.now("android.ScreenRecorder.startRecordingTestVideo")?;
                                        artifacts
                                            .device
                                            .safe_start_screen_recorder(
                                                &remote.to_owned(),
                                                &artifacts
                                                    .device
                                                    .runtime()
                                                    .configuration
                                                    .screen_record_configuration
                                                    .video_configuration,
                                                context.clone(),
                                            )
                                            .await?;
                                        let end = artifacts.now("android.ScreenRecorder.startRecordingTestVideo")?;
                                        artifacts.device.log_named(
                                            "ScreenRecorder",
                                            Level::Debug,
                                            &format!(
                                                "Recording finished in {}ms {}",
                                                end.wrapping_sub(start),
                                                remote.to_owned()
                                            ),
                                            None,
                                        );
                                        Ok(())
                                    }
                                    .await;
                                    if let Err(e) = result {
                                        let (level, msg) = if matches!(e, VendorError::Cancelled(_)) {
                                            (Level::Warn, "screenrecord start was interrupted")
                                        } else {
                                            (Level::Error, "Something went wrong while screen recording")
                                        };
                                        artifacts
                                            .device
                                            .log_named("ScreenRecorder", level, msg, Some(e.failure()));
                                        return Err(e);
                                    }
                                    let mut state = lock(&state);
                                    if !chunked || state.cancelling {
                                        break;
                                    }
                                    let chunk = state.chunks.entry(test.clone()).or_default();
                                    *chunk = chunk.wrapping_add(1);
                                    remote = artifacts.device.file_manager().remote_chunked_video_for_test(
                                        &test,
                                        &artifacts.batch.id,
                                        Some(*chunk),
                                    )?;
                                    state.last.push(remote.clone());
                                }
                                Ok(())
                            })
                        }));
                    *lock(&self.job) = Some(job);
                }
                TestEvent::TestFailed { .. } | TestEvent::TestAssumptionFailed { .. } => {
                    let mut state = lock(&self.state);
                    state.cancelling = true;
                    state.failed = true;
                }
                TestEvent::TestEnded { id, .. } => {
                    lock(&self.state).cancelling = true;
                    match self.finish_test(&to_test(id), context).await {
                        Err(VendorError::Transfer(_)) => self.artifacts.device.log_named(
                            "ScreenRecorderTestBatchListener",
                            Level::Warn,
                            "Can't pull video",
                            None,
                        ),
                        r => r?,
                    }
                    lock(&self.state).last.clear();
                }
                TestEvent::TestRunFailed { .. } => {
                    lock(&self.state).cancelling = true;
                    let result = async {
                        let healthy = timed(&context, 10_000, |signal| {
                            Box::pin(async {
                                self.artifacts
                                    .device
                                    .execute_shell_command(
                                        "echo quickhealthcheck",
                                        "",
                                        VendorContext {
                                            cancellation: signal,
                                            ..context.clone()
                                        },
                                    )
                                    .await
                            })
                        })
                        .await?
                        .flatten()
                        .is_some_and(|r| r.output.to_owned().contains("quickhealthcheck"));
                        if healthy {
                            self.stop(context.clone()).await?;
                            let remotes = lock(&self.state).last.clone();
                            for (i, remote) in remotes.iter().enumerate() {
                                self.pull(remote, None, Some(i.to_string().as_str().to_owned()), context.clone())
                                    .await?;
                            }
                            for remote in remotes {
                                self.remove(&remote, context.clone()).await?;
                            }
                        }
                        Ok(())
                    }
                    .await;
                    match result {
                        Err(VendorError::Transfer(_)) => self.artifacts.device.log_named(
                            "ScreenRecorderTestBatchListener",
                            Level::Warn,
                            "Can't pull video",
                            None,
                        ),
                        r => r?,
                    }
                }
                _ => {}
            }
            Ok(())
        })
    }
}
