//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/video/ScreenRecorder.kt
//! and vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/video/ScreenRecorderTestBatchListener.kt.
use super::*;
use crate::listeners::{AndroidTestRunListener, to_test};
use marathon_adb::testrunner::TestEvent;
use marathon_config::ScreenRecordingPolicy;
use marathon_core::{executor::shared::requested, runtime::*, vendor::VendorContext};
use marathon_logging::Level;
use std::path::PathBuf;

const POLL_MILLIS: i64 = 75;
// Reserve part of D102's stop budget for one last signal and exit check.
const STOP_MILLIS: i64 = 1_500;
const FORCE_STOP_MILLIS: i64 = 250;

/// Match the process name itself (N33). Toolbox can leave WCHAN empty and prints an
/// unlabelled state before NAME. Both toolbox and toybox put the name last.
fn recorder_pid(output: &str) -> Result<Option<u32>, ()> {
    let mut lines = output.lines();
    let header: Vec<_> = lines.next().ok_or(())?.split_whitespace().collect();
    let pid_column = header.iter().position(|column| *column == "PID").ok_or(())?;
    if header.last() != Some(&"NAME") {
        return Err(());
    }
    let mut recorder = None;
    for line in lines {
        let mut columns = line.split_whitespace();
        let pid = columns.nth(pid_column);
        let name = columns.next_back();
        if matches!(name, Some("screenrecord" | "/system/bin/screenrecord")) {
            let pid = pid
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|pid| *pid > 0)
                .ok_or(())?;
            if recorder.replace(pid).is_some() {
                return Err(());
            }
        }
    }
    Ok(recorder)
}

#[derive(PartialEq, Eq)]
enum Stop {
    Exited,
    Missing,
    Unconfirmed,
}

pub(crate) async fn stop_screen_record_for_cleanup(device: &AndroidDevice, context: VendorContext) -> VendorResult<()> {
    stop_screen_record(device, context, true).await.map(|_| ())
}

async fn stop_screen_record(device: &AndroidDevice, context: VendorContext, cleanup: bool) -> VendorResult<Stop> {
    let target = module_path!();
    device.log_named(target, Level::DEBUG, "Stopping screen recorder", None);
    let shell = async |command: &str, context: VendorContext| {
        if cleanup {
            device.cleanup_shell(command, context).await
        } else {
            device.recorder_stop_shell(command, context).await
        }
    };
    let signal_and_wait = async |pid: u32, signal, context: VendorContext| {
        // The recorder can exit before the signal arrives. Only the PID probe proves exit.
        shell(&format!("kill -{signal} {pid}"), context.clone()).await?;
        let probe = format!("if [ -d /proc/{pid} ]; then echo alive; else echo gone; fi");
        loop {
            // The shell can see screenrecord's /proc entry until the process is reaped.
            match shell(&probe, context.clone())
                .await?
                .filter(|reply| reply.exit_code == 0)
            {
                Some(reply) if reply.output.to_owned().trim() == "gone" => return Ok(Stop::Exited),
                Some(reply) if reply.output.to_owned().trim() == "alive" => {}
                _ => return Ok(Stop::Unconfirmed),
            }
            context.clock.delay(POLL_MILLIS, context.cancellation.clone()).await?;
        }
    };
    let mut pid = None;
    let known_pid = &mut pid;
    // D92 disposal keeps its original best-effort INT on the cleanup connection.
    let graceful_millis = if cleanup {
        STOP_MILLIS
    } else {
        STOP_MILLIS - FORCE_STOP_MILLIS
    };
    let result = timed(&context, graceful_millis, |signal| {
        let context = VendorContext {
            cancellation: signal,
            ..context.clone()
        };
        Box::pin(async move {
            let ps = if device.api_level() >= 26 { "ps -A" } else { "ps" };
            let Some(reply) = shell(ps, context.clone()).await?.filter(|reply| reply.exit_code == 0) else {
                return Ok(Stop::Unconfirmed);
            };
            let Ok(pid) = recorder_pid(&reply.output.to_owned()) else {
                return Ok(Stop::Unconfirmed);
            };
            *known_pid = pid;
            let Some(pid) = pid else {
                return Ok(Stop::Missing);
            };
            device.log_named(
                target,
                Level::TRACE,
                &format!("Sending INT to screen recorder PID {pid}"),
                None,
            );
            signal_and_wait(pid, 2, context).await
        })
    })
    .await;
    let (stopped, error) = match result {
        Ok(stopped) => (stopped.unwrap_or(Stop::Unconfirmed), None),
        Err(error @ VendorError::Cancelled(_)) => return Err(error),
        Err(error) => (Stop::Unconfirmed, Some(error)),
    };
    if stopped == Stop::Unconfirmed
        && !cleanup
        && let Some(pid) = pid
    {
        if let Some(reason) = context.cancellation.reason() {
            return Err(VendorError::Cancelled(reason));
        }
        let force = timed(&context, FORCE_STOP_MILLIS, |signal| {
            let context = VendorContext {
                cancellation: signal,
                ..context.clone()
            };
            Box::pin(signal_and_wait(pid, 9, context))
        })
        .await;
        let cause = match force {
            Ok(Some(Stop::Exited)) => return Ok(Stop::Exited),
            Ok(_) => None,
            Err(error @ VendorError::Cancelled(_)) => return Err(error),
            Err(error) => Some(error),
        };
        device.log_named(
            target,
            Level::WARN,
            &format!("Could not confirm screen recorder PID {pid} exited after KILL"),
            cause.as_ref().or(error.as_ref()).map(VendorError::failure),
        );
        return Ok(Stop::Unconfirmed);
    }
    match stopped {
        Stop::Missing => device.log_named(target, Level::WARN, "No screen recorder was found to stop", None),
        Stop::Unconfirmed => device.log_named(
            target,
            Level::ERROR,
            "Could not confirm screen recorder exit",
            error.as_ref().map(VendorError::failure),
        ),
        _ => {}
    }
    Ok(stopped)
}

#[derive(Default)]
struct Recording {
    failed: bool,
    cancelling: bool,
    pending: bool,
    test: Option<Test>,
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
    async fn stop(&self, context: VendorContext) -> VendorResult<Vec<String>> {
        let expected = {
            let mut state = lock(&self.state);
            state.cancelling = true;
            state.pending && !state.last.is_empty()
        };
        let start = self.artifacts.now("android.ScreenRecorderTestBatchListener.stop")?;
        let result = if let Some(reason) = context.cancellation.reason() {
            Err(VendorError::Cancelled(reason))
        } else if expected {
            stop_screen_record(&self.artifacts.device, context, false).await
        } else {
            Ok(Stop::Missing)
        };
        let end = self.artifacts.now("android.ScreenRecorderTestBatchListener.stop")?;
        self.artifacts.device.log_named(
            "ScreenRecorderTestBatchListener",
            Level::TRACE,
            &format!("stop {}ms", end.wrapping_sub(start)),
            None,
        );
        let start = self.artifacts.now("android.ScreenRecorderTestBatchListener.join")?;
        self.join().await;
        let end = self.artifacts.now("android.ScreenRecorderTestBatchListener.join")?;
        self.artifacts.device.log_named(
            "ScreenRecorderTestBatchListener",
            Level::TRACE,
            &format!("join {}ms", end.wrapping_sub(start)),
            None,
        );
        let mut state = lock(&self.state);
        let stopped = matches!(&result, Ok(Stop::Exited));
        if !state.last.is_empty()
            && (stopped || !state.pending || matches!(&result, Err(VendorError::Cancelled(_))))
            && !self.artifacts.device.stopping()
        {
            self.artifacts.device.screenrecord_active(false);
        }
        // No PID is not proof that an in-flight start reached the device. Earlier chunks
        // have their own shell exit receipts, but don't pull this one without one.
        if stopped {
            state.pending = false;
        }
        result?;
        let mut paths = std::mem::take(&mut state.last);
        if state.pending {
            paths.pop();
            state.pending = false;
        }
        Ok(paths)
    }
    async fn pull(
        &self,
        remote: &str,
        test: Option<&Test>,
        chunk: Option<String>,
        context: VendorContext,
    ) -> VendorResult<Option<PathBuf>> {
        let local = self.artifacts.file(FileType::Video, test, None, chunk).await?;
        let label = if test.is_some() {
            "android.ScreenRecorderTestBatchListener.pullTestVideoFile"
        } else {
            "android.ScreenRecorderTestBatchListener.pullLastBatchVideo"
        };
        let start = self.artifacts.now(label)?;
        let device = &self.artifacts.device;
        let reason = match device
            .receive_movie_with_timeout(remote, &local.to_string_lossy(), context)
            .await
        {
            Ok(Some(())) => None,
            Ok(None) => Some("the receive did not complete".to_owned()),
            Err(VendorError::Transfer(failure)) => Some(failure.message.clone().unwrap_or_else(|| failure.to_string())),
            Err(error) => {
                device.log_named(
                    module_path!(),
                    Level::ERROR,
                    &format!("Video {remote} was not delivered: {}", error.failure()),
                    None,
                );
                return Err(error);
            }
        };
        if let Some(reason) = reason {
            device.log_named(
                module_path!(),
                Level::ERROR,
                &format!("Video {remote} was not delivered: {reason}"),
                None,
            );
            return Ok(None);
        }
        let end = self.artifacts.now(label)?;
        self.artifacts.device.log_named(
            "ScreenRecorderTestBatchListener",
            Level::TRACE,
            &format!(
                "Pulling finished in {}ms {} ",
                end.wrapping_sub(start),
                remote.to_owned()
            ),
            None,
        );
        Ok(Some(local))
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
            Level::TRACE,
            &format!("Removed file in {}ms {}", end.wrapping_sub(start), remote.to_owned()),
            None,
        );
        Ok(())
    }
    async fn finish_test(&self, test: &Test, context: VendorContext) -> VendorResult<()> {
        let remotes = self.stop(context.clone()).await?;
        let (matches_test, failed) = {
            let state = lock(&self.state);
            (state.test.as_ref() == Some(test), state.failed)
        };
        if !matches_test {
            return Ok(());
        }
        let mut removable = Vec::new();
        for (i, remote) in remotes.into_iter().enumerate() {
            if self.policy == ScreenRecordingPolicy::OnAny || failed {
                let Some(file) = self
                    .pull(
                        &remote,
                        Some(test),
                        self.chunked.then(|| i.to_string().as_str().to_owned()),
                        context.clone(),
                    )
                    .await?
                else {
                    continue;
                };
                let name = if self.chunked {
                    format!("screen-{i}")
                } else {
                    "screen".into()
                };
                self.attachments.emit(test, file, AttachmentType::Video, &name)?;
            }
            removable.push(remote);
        }
        for remote in removable {
            self.remove(&remote, context.clone()).await?;
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
                        state.pending = false;
                        state.test = Some(test.clone());
                        state.last.clear();
                    }
                    let artifacts = self.artifacts.clone();
                    let state = self.state.clone();
                    let chunked = self.chunked;
                    let job = self.artifacts.device.spawn_owned(Box::new(move |signal| {
                        Box::pin(async move {
                            let context = VendorContext {
                                cancellation: signal,
                                ..context
                            };
                            let mut remote = first;
                            let mut chunk = 0i64;
                            loop {
                                let result = async {
                                    artifacts.device.ensure_running()?;
                                    {
                                        let mut state = lock(&state);
                                        // Once stop takes this lock, no new chunk can be admitted.
                                        if state.cancelling {
                                            return Ok(false);
                                        }
                                        state.pending = true;
                                        state.last.push(remote.clone());
                                        artifacts.device.screenrecord_active(true);
                                    }
                                    let start = artifacts.now("android.ScreenRecorder.startRecordingTestVideo")?;
                                    let completed = artifacts
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
                                        Level::DEBUG,
                                        &format!(
                                            "Recording finished in {}ms {}",
                                            end.wrapping_sub(start),
                                            remote.to_owned()
                                        ),
                                        None,
                                    );
                                    Ok(completed)
                                }
                                .await;
                                match result {
                                    Ok(false) => break,
                                    Ok(true) => lock(&state).pending = false,
                                    Err(error) => {
                                        if !matches!(error, VendorError::Cancelled(_)) || !lock(&state).cancelling {
                                            artifacts.device.log_named(
                                                module_path!(),
                                                Level::ERROR,
                                                "Screen recording failed",
                                                Some(error.failure()),
                                            );
                                        }
                                        return Err(error);
                                    }
                                }
                                if !chunked {
                                    break;
                                }
                                chunk = chunk.wrapping_add(1);
                                remote = artifacts.device.file_manager().remote_chunked_video_for_test(
                                    &test,
                                    &artifacts.batch.id,
                                    Some(chunk),
                                )?;
                            }
                            Ok(())
                        })
                    }))?;
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
                            Level::WARN,
                            "Can't pull video",
                            None,
                        ),
                        r => r?,
                    }
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
                            let remotes = self.stop(context.clone()).await?;
                            let mut removable = Vec::new();
                            for (i, remote) in remotes.iter().enumerate() {
                                if self
                                    .pull(remote, None, Some(i.to_string().as_str().to_owned()), context.clone())
                                    .await?
                                    .is_some()
                                {
                                    removable.push(remote);
                                }
                            }
                            for remote in removable {
                                self.remove(remote, context.clone()).await?;
                            }
                        }
                        Ok(())
                    }
                    .await;
                    match result {
                        Err(VendorError::Transfer(_)) => self.artifacts.device.log_named(
                            "ScreenRecorderTestBatchListener",
                            Level::WARN,
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
