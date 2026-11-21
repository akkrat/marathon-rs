//! One simulator capture and its stop result.

use super::{AppleSimulatorDevice, VendorContext, VendorResult};
use marathon_apple::{
    apple_device::AppleDevice,
    // simctl's stderr for the busy refusal; one definition with the worker supervisor's restart hint (U262).
    bin::xcrun::simctl::service::reports_recording_busy as reports_busy,
    cmd::{
        CommandExecutor, CommandRequest, CommandSession, base_command::CommandTermination,
        device_commands::as_lifecycle, exec_outcome, retained_tasks::panic_as_error,
    },
};
use marathon_core::{
    executor::shared::{SharedDeferred, Signal},
    runtime::{CancellationSignal, VendorClock, VendorFuture},
};
use marathon_logging::Level;
use std::{pin::Pin, sync::Arc};
use tokio::sync::watch;

const STOP_BUDGET: i64 = 15_000;
/// simctl's own stderr line once the simulator's video service has started (D98).
const START_MARKER: &str = "Recording started";
/// simctl's exit status when the simulator's video service refuses a second recording (EBUSY).
const BUSY_STATUS: i32 = 16;

#[derive(Clone, Copy)]
struct StopWindow {
    interrupt: i64,
    end: i64,
}

/// Process outcome only. Artifact policy and delivery are owned by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingFinalization {
    NotStarted,
    Finalized,
    Unfinalized,
    Unconfirmed,
    /// D98: video is off on this simulator for the rest of the run; nothing was launched.
    Skipped,
}

enum Capture {
    Video(String),
    DeviceLog(Vec<String>),
}

pub struct Recording {
    video: bool,
    deadline: watch::Sender<Option<StopWindow>>,
    legacy: watch::Sender<bool>,
    done: Arc<SharedDeferred<RecordingFinalization>>,
    opened: Arc<SharedDeferred<()>>,
}

impl Recording {
    fn new(video: bool) -> Arc<Self> {
        Arc::new(Self {
            video,
            deadline: watch::channel(None).0,
            legacy: watch::channel(false).0,
            done: SharedDeferred::new(),
            opened: SharedDeferred::new(),
        })
    }

    pub fn request_stop(&self, clock: &dyn VendorClock, deadline: Option<i64>) {
        let now = clock.now_millis();
        let end = now.saturating_add(STOP_BUDGET).min(deadline.unwrap_or(i64::MAX));
        let interrupt = now.saturating_add(end.saturating_sub(now).max(0) * 2 / 3);
        self.limit(StopWindow { interrupt, end });
    }

    pub(crate) fn request_healthy_stop(&self, clock: &dyn VendorClock, shell_millis: i64, deadline: Option<i64>) {
        let interrupt = clock.now_millis().saturating_add(shell_millis.max(STOP_BUDGET));
        let end = interrupt.saturating_add(5_000).min(deadline.unwrap_or(i64::MAX));
        self.limit(StopWindow {
            interrupt: interrupt.min(end),
            end,
        });
    }

    fn limit(&self, limit: StopWindow) {
        self.deadline.send_modify(|current| {
            *current = Some(match *current {
                Some(prior) => StopWindow {
                    interrupt: prior.interrupt.min(limit.interrupt),
                    end: prior.end.min(limit.end),
                },
                None => limit,
            });
        });
    }

    pub(crate) async fn wait_opened(&self) -> VendorResult<()> {
        self.opened.wait().await.map(|_| ())
    }

    pub async fn finalization(&self) -> VendorResult<RecordingFinalization> {
        self.done.wait().await.map(|result| *result)
    }

    // Best-effort legacy stop is still requested at disposal. Only an owned
    // recorder supplies a D93 cleanup receipt.
    pub(super) fn needs_stop(&self) -> bool {
        self.done.completed().is_none() || self.ended() == Some(false)
    }

    pub(super) async fn wait_for_receipt(&self) {
        let mut legacy = self.legacy.subscribe();
        tokio::select! {
            _ = legacy.wait_for(|legacy| *legacy) => {}
            _ = self.finalization() => {}
        }
    }

    pub(super) fn ended(&self) -> Option<bool> {
        if *self.legacy.borrow() {
            return Some(true);
        }
        self.done
            .completed()
            .map(|result| result.is_ok_and(|result| *result != RecordingFinalization::Unconfirmed))
    }

    async fn run(
        &self,
        device: &AppleSimulatorDevice,
        capture: Capture,
        context: VendorContext,
        ready: VendorFuture<'static, bool>,
    ) -> VendorResult<RecordingFinalization> {
        let mut deadline = self.deadline.subscribe();
        let batch = context.cancellation.clone();
        let protected = VendorContext {
            cancellation: Signal::new(),
            ..context
        };
        let ready = tokio::select! {
            biased;
            _ = deadline.wait_for(Option::is_some) => return Ok(RecordingFinalization::NotStarted),
            _ = batch.cancelled() => return Ok(RecordingFinalization::NotStarted),
            result = ready => result?,
        };
        if !ready
            || device
                .state()
                .recordings
                .iter()
                .any(|recording| recording.ended() == Some(false))
        {
            return Ok(RecordingFinalization::NotStarted);
        }
        if matches!(capture, Capture::Video(_)) && device.services().busy_video.is_busy(&device.video_key) {
            return Ok(RecordingFinalization::Skipped);
        }
        if let Capture::Video(path) = &capture {
            match device.uses_legacy_recording().await {
                Ok(true) => {
                    self.legacy.send_replace(true);
                    return Ok(self
                        .run_legacy(device, path, &protected, batch.as_ref(), &mut deadline)
                        .await);
                }
                Ok(false) => {}
                Err(_) => return Ok(RecordingFinalization::NotStarted),
            }
        }
        // Keep the open awaiter alive after handoff so a late acknowledgement still supplies
        // the session needed to stop. The device's admission gate refuses a launch after seal.
        let opened = {
            let timeouts = &device.services().vendor.timeout_configuration;
            let (opening, timeout) = match &capture {
                Capture::Video(path) => (device.start_video_recording(path, &protected), timeouts.video),
                Capture::DeviceLog(command) => (
                    device.commands.execute(
                        CommandRequest::new(command.clone(), marathon_apple::extensions::Durations::INFINITE),
                        protected.clone(),
                    ),
                    timeouts.shell,
                ),
            };
            let mut opening = std::pin::pin!(exec_outcome::observe(opening));
            let clock = protected.clock.as_ref();
            let launch_millis = timeout.to_millis().unwrap_or(i64::MAX).max(0);
            let stopping = async {
                tokio::select! {
                    _ = deadline.wait_for(Option::is_some) => {}
                    _ = batch.cancelled() => self.request_stop(protected.clock.as_ref(), None),
                }
            };
            let mut open_timeout = std::pin::pin!(clock.delay(launch_millis, Signal::new()));
            tokio::select! {
                biased;
                result = &mut opening => Some(result),
                _ = &mut open_timeout => None,
                _ = stopping => tokio::select! {
                    biased;
                    result = wait_stage(opening.as_mut(), &mut deadline, clock, clock.now_millis(), 6) => result,
                    _ = &mut open_timeout => None,
                }
            }
        };
        let Some((session, started_nothing)) = opened else {
            return Ok(RecordingFinalization::Unconfirmed);
        };
        let session = match session {
            Ok(session) => session,
            Err(_) if started_nothing => return Ok(RecordingFinalization::NotStarted),
            Err(_) => return Ok(RecordingFinalization::Unconfirmed),
        };
        self.opened.complete(Ok(()));
        let video = match &capture {
            Capture::Video(path) => Some((device, path.as_str())),
            Capture::DeviceLog(_) => None,
        };
        let cancellation = async {
            batch.cancelled().await;
            self.request_stop(protected.clock.as_ref(), None);
            std::future::pending::<()>().await;
        };
        let (outcome, started, busy) = tokio::select! {
            _ = cancellation => unreachable!("stop publisher remains active"),
            result = self.run_session(&session, &protected, &mut deadline, video) => result,
        };
        // Status 16 alone is not enough: only simctl's busy refusal on stderr shows the video service is held.
        if video.is_some()
            && !started
            && busy
            && session.base.exit_code.termination() == Some(CommandTermination::Status(BUSY_STATUS))
        {
            self.recorder_busy(device);
        }
        // The outcome comes from the exit report. Closing a channel alone cannot establish it.
        // Retain channel/reader close independently if there is no time left to await it.
        device
            .services()
            .runtime
            .spawn(Box::pin(async move { session.close().await }));
        Ok(outcome)
    }

    async fn run_legacy(
        &self,
        device: &AppleSimulatorDevice,
        path: &str,
        context: &VendorContext,
        batch: &dyn CancellationSignal,
        deadline: &mut watch::Receiver<Option<StopWindow>>,
    ) -> RecordingFinalization {
        let pidfile = device.remote_file_manager().remote_video_pidfile();
        let logger = device.services().logging.component("marathon_ios::listener::video");
        let logger_for_wait = logger.clone();
        let wait_for_pidfile = context.clock.with_timeout_or_null(
            context.cancellation.clone(), 10_000, Box::new(|signal| {
                Box::pin(async move {
                    let read_context = VendorContext { cancellation: signal.clone(), ..context.clone() };
                    let mut warned = false;
                    while device.read_textfile(&pidfile, &read_context).await?.is_some_and(|pid| !pid.trim().is_empty()) {
                        if !warned {
                            device.services().logging.log(&logger_for_wait, Level::WARN, ||
                                "Previous video recording didn't finish yet. Current recording might be missing or cut from the start".into(), None);
                            warned = true;
                        }
                        context.clock.delay(100, signal.clone()).await?;
                    }
                    Ok(())
                })
            }),
        );
        let pidfile_ready = tokio::select! {
            biased;
            _ = deadline.wait_for(Option::is_some) => return RecordingFinalization::NotStarted,
            _ = batch.cancelled() => return RecordingFinalization::NotStarted,
            result = wait_for_pidfile => result,
        };
        let Ok(pidfile_ready) = pidfile_ready else {
            return RecordingFinalization::NotStarted;
        };
        if pidfile_ready.is_none() {
            device.services().logging.log(
                &logger,
                Level::WARN,
                || "Previous video recording didn't finish in 10s, starting a new one anyway".into(),
                None,
            );
        }
        let launched = device.start_legacy_recording(path, context).await;
        if !matches!(launched, Ok(Some(result)) if result.exit_code == Some(0)) {
            return RecordingFinalization::NotStarted;
        }
        self.opened.complete(Ok(()));
        let pidfile = device.remote_file_manager().remote_video_pidfile();
        let pid = device
            .read_textfile(&pidfile, context)
            .await
            .unwrap_or_default()
            .map_or_else(|| "null".to_owned(), |pid| pid.trim().to_owned());
        device.services().logging.log(
            &logger,
            Level::DEBUG,
            || format!("started video recording pid={pid} to {path}"),
            None,
        );
        tokio::select! {
            _ = deadline.wait_for(Option::is_some) => {},
            _ = batch.cancelled() => self.request_stop(context.clock.as_ref(), None),
        }
        let mut stop = std::pin::pin!(as_lifecycle(device.stop_legacy_recording_if_present(context)));
        match wait_stage(
            stop.as_mut(),
            deadline,
            context.clock.as_ref(),
            context.clock.now_millis(),
            6,
        )
        .await
        {
            Some(Ok(true)) => RecordingFinalization::Finalized,
            _ => RecordingFinalization::Unfinalized,
        }
    }

    /// D98: the busy refusal with no live recorder of marathon's own on the simulator means its video service is held
    /// by a recording nobody can stop (a signal before the start, including marathon's own TERM or KILL at a stop
    /// cutoff, a crash, another tool). Every later start fails the same way until
    /// the simulator restarts, so video stops here for the rest of the run with one warning. x102: on the simulator, not
    /// on this device object, so that an object set up for it later in the run neither records nor warns again.
    fn recorder_busy(&self, device: &AppleSimulatorDevice) {
        let first = {
            // Under the device's lock, as a capture registers, so that none registered after it misses the state.
            let state = device.state();
            // Only a launched recorder can hold the service; the next test's capture can be registered and waiting.
            let own = state.recordings.iter().any(|recording| {
                !std::ptr::eq(recording.as_ref(), self)
                    && recording.video
                    && recording.opened.completed().is_some_and(|opened| opened.is_ok())
                    && recording.ended() != Some(true)
            });
            !own && device.services().busy_video.mark_busy(&device.video_key)
        };
        if first {
            let (serial, udid) = (&device.serial, &device.udid);
            device.log_busy_video(
                Level::WARN,
                format!(
                    "Simulator {serial}: its video service is busy with a recording already in progress. Video \
                     recording is off on this simulator for the rest of the run. Restart the simulator \
                     (xcrun simctl shutdown {udid}, then xcrun simctl boot {udid}) to record again."
                ),
            );
        }
    }

    /// The process outcome, whether a recorder confirmed its start, and whether its stderr reported the busy refusal
    /// (D98).
    async fn run_session(
        &self,
        session: &CommandSession,
        context: &VendorContext,
        deadline: &mut watch::Receiver<Option<StopWindow>>,
        video: Option<(&AppleSimulatorDevice, &str)>,
    ) -> (RecordingFinalization, bool, bool) {
        let clock = context.clock.as_ref();
        // A device log has no start marker and never waits for one.
        let started = watch::channel(video.is_none()).0;
        let worker = session.worker_report();
        let busy = std::sync::atomic::AtomicBool::new(false);
        // D98: a signal before simctl's start marker leaves the simulator's video service recording with no client,
        // and every later recording on that simulator then fails with the busy code. A supervised SSH worker reads
        // the recorder's stderr and holds INT itself; otherwise the marker is read here and INT waits for it.
        let gated = video.is_some() && worker.is_none();
        let confirm = || {
            if !started.send_replace(true)
                && let Some((device, path)) = video
            {
                let logger = device.services().logging.component("marathon_ios::listener::video");
                device.services().logging.log(
                    &logger,
                    Level::DEBUG,
                    || format!("started video recording to {path}"),
                    None,
                );
            }
        };
        let exit = std::pin::pin!(async {
            let result = if gated {
                read_output(session, |line| {
                    if line.strip_suffix('\r').unwrap_or(line) == START_MARKER {
                        confirm();
                    } else if reports_busy(line) {
                        busy.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                })
                .await
            } else {
                let reported = async {
                    if let Some(mut worker) = worker.as_ref().map(|worker| worker.started.clone())
                        && worker.wait_for(|started| *started).await.is_ok()
                    {
                        confirm();
                    }
                    std::future::pending::<()>().await
                };
                tokio::select! {
                    biased;
                    result = session.await_result(context) => result.map(|_| ()),
                    _ = reported => unreachable!("the start report never ends the wait"),
                }
            };
            if let Some(worker) = &worker {
                if *worker.started.borrow() {
                    confirm();
                }
                if worker.stderr.get().is_some_and(|stderr| reports_busy(stderr)) {
                    busy.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            match (result, session.base.exit_code.termination()) {
                (Ok(()), Some(CommandTermination::Status(0))) => RecordingFinalization::Finalized,
                (Ok(()), Some(CommandTermination::NotStarted)) => RecordingFinalization::NotStarted,
                (Ok(()), Some(CommandTermination::Status(_) | CommandTermination::Signal(_))) => {
                    RecordingFinalization::Unfinalized
                }
                _ => RecordingFinalization::Unconfirmed,
            }
        });
        let mut exit = exit;
        let outcome = async {
            tokio::select! {
                biased;
                outcome = &mut exit => return outcome,
                _ = deadline.wait_for(Option::is_some) => {}
            }

            let start = clock.now_millis();
            // D98: the marker can arrive until the INT cutoff, which this wait never extends. A recorder that has not
            // confirmed by then gets TERM at that cutoff, as one that ignored INT would.
            let mut interrupt = true;
            if gated {
                let mut confirmed = started.subscribe();
                let mut waiting = std::pin::pin!(async {
                    tokio::select! {
                        biased;
                        outcome = &mut exit => Some(outcome),
                        _ = confirmed.wait_for(|started| *started) => None,
                    }
                });
                match wait_stage(waiting.as_mut(), deadline, clock, start, 4).await {
                    Some(Some(outcome)) => return outcome,
                    Some(None) => {}
                    None => interrupt = false,
                }
            }
            let stages = [
                CommandSession::interrupt,
                CommandSession::terminate,
                CommandSession::kill,
            ];
            // Reserve time for TERM, KILL and the exit report. Most of the window is for the movie writer.
            for (signal, stage) in stages.into_iter().zip([4, 5, 6]) {
                if stage == 4 && !interrupt {
                    continue;
                }
                let sent = signal(session).is_ok();
                if let Some(outcome) = wait_stage(exit.as_mut(), deadline, clock, start, stage).await {
                    return match (stage, sent, outcome) {
                        (4, true, _) => outcome,
                        (_, _, RecordingFinalization::Finalized) => RecordingFinalization::Unfinalized,
                        _ => outcome,
                    };
                }
            }
            RecordingFinalization::Unconfirmed
        }
        .await;
        (
            outcome,
            *started.borrow(),
            busy.load(std::sync::atomic::Ordering::Relaxed),
        )
    }
}

/// Drains a session's output to its exit, as `await_result` does, handing each stderr line to `line`.
async fn read_output(session: &CommandSession, line: impl Fn(&str)) -> VendorResult<()> {
    let stderr = async {
        while let Some(text) = session.base.stderr.receive().await? {
            line(&text);
        }
        Ok(())
    };
    let stdout = async {
        while session.base.stdout.receive().await?.is_some() {}
        Ok(())
    };
    tokio::select! {
        biased;
        reason = session.base.scope.cancelled() => Err(marathon_core::error::VendorError::Cancelled(reason)),
        result = async { tokio::try_join!(stderr, stdout, session.base.exit_code.wait()).map(|_| ()) } => result,
    }
}

async fn wait_stage<T>(
    mut exit: Pin<&mut (impl Future<Output = T> + Send)>,
    deadline: &mut watch::Receiver<Option<StopWindow>>,
    clock: &dyn VendorClock,
    start: i64,
    stage: i64,
) -> Option<T> {
    loop {
        let window = deadline.borrow_and_update().unwrap_or(StopWindow {
            interrupt: start,
            end: start,
        });
        let cutoff = match stage {
            4 => window.interrupt.min(window.end),
            5 => window
                .interrupt
                .saturating_add(window.end.saturating_sub(window.interrupt) / 2),
            _ => window.end,
        };
        tokio::select! {
            biased;
            outcome = &mut exit => return Some(outcome),
            Ok(()) = deadline.changed() => {},
            _ = clock.delay(cutoff.saturating_sub(clock.now_millis()).max(0), Signal::new()) => return None,
        }
    }
}

impl AppleSimulatorDevice {
    pub(crate) fn begin_recording(
        self: &Arc<Self>,
        path: String,
        context: VendorContext,
        ready: VendorFuture<'static, bool>,
    ) -> VendorResult<Arc<Recording>> {
        self.begin_capture(Capture::Video(path), context, ready)
    }

    pub(crate) fn begin_device_log(
        self: &Arc<Self>,
        command: Vec<String>,
        context: VendorContext,
    ) -> VendorResult<Arc<Recording>> {
        self.begin_capture(Capture::DeviceLog(command), context, Box::pin(async { Ok(true) }))
    }

    fn begin_capture(
        self: &Arc<Self>,
        capture: Capture,
        context: VendorContext,
        ready: VendorFuture<'static, bool>,
    ) -> VendorResult<Arc<Recording>> {
        let is_video = matches!(capture, Capture::Video(_));
        let recording = Recording::new(is_video);
        {
            let mut state = self.state();
            if let Some(error) = state.refusal_error(&self.serial) {
                return Err(error);
            }
            // D98: after a busy video service, later tests launch nothing and report nothing per test.
            if is_video && self.services.busy_video.is_busy(&self.video_key) {
                recording.opened.complete(Err(marathon_core::error::VendorError::Other(
                    marathon_apple::cmd::failure(
                        marathon_error::FailureKind::IllegalState,
                        "Video recording is off on this simulator",
                    ),
                )));
                recording.done.complete(Ok(RecordingFinalization::Skipped));
                return Ok(recording);
            }
            state.recordings.retain(|record| record.needs_stop());
            state.recordings.push(recording.clone());
        }
        let device = self.clone();
        let owned = recording.clone();
        self.services.runtime.spawn(Box::pin(async move {
            let result = panic_as_error(owned.run(&device, capture, context, ready)).await;
            if owned.opened.completed().is_none() {
                owned
                    .opened
                    .complete(Err(result.as_ref().err().cloned().unwrap_or_else(|| {
                        marathon_core::error::VendorError::Other(marathon_apple::cmd::failure(
                            marathon_error::FailureKind::Io,
                            "The worker did not confirm the capture started",
                        ))
                    })));
            }
            if is_video && matches!(result, Ok(RecordingFinalization::NotStarted)) {
                let logger = device.services().logging.component("marathon_ios::listener::video");
                device
                    .services()
                    .logging
                    .log(&logger, Level::WARN, || "Video recording did not start".into(), None);
            }
            owned.done.complete(result);
            Ok(())
        }));
        Ok(recording)
    }
}
