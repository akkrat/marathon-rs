//! One foreground simctl session and its stop result.

use super::{AppleSimulatorDevice, VendorContext, VendorResult};
use marathon_apple::{
    apple_device::AppleDevice,
    cmd::{
        CommandExecutor, CommandRequest, CommandSession, base_command::CommandTermination, exec_outcome,
        retained_tasks::panic_as_error,
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

/// Process outcome only. Artifact policy and delivery are owned by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingFinalization {
    NotStarted,
    Finalized,
    Unfinalized,
    Unconfirmed,
}

enum Capture {
    Video(String),
    DeviceLog(Vec<String>),
}

pub struct Recording {
    deadline: watch::Sender<Option<i64>>,
    done: Arc<SharedDeferred<RecordingFinalization>>,
    opened: Arc<SharedDeferred<()>>,
}

impl Recording {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            deadline: watch::channel(None).0,
            done: SharedDeferred::new(),
            opened: SharedDeferred::new(),
        })
    }

    pub fn request_stop(&self, clock: &dyn VendorClock, deadline: Option<i64>) {
        let limit = clock.now_millis().saturating_add(STOP_BUDGET);
        let deadline = limit.min(deadline.unwrap_or(i64::MAX));
        self.deadline.send_if_modified(|current| {
            if current.is_some_and(|current| current <= deadline) {
                return false;
            }
            *current = Some(deadline);
            true
        });
    }

    pub(crate) async fn wait_opened(&self) -> VendorResult<()> {
        self.opened.wait().await.map(|_| ())
    }

    pub async fn finalization(&self) -> VendorResult<RecordingFinalization> {
        self.done.wait().await.map(|result| *result)
    }

    pub(super) fn ended(&self) -> Option<bool> {
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
        if let Capture::Video(path) = &capture {
            let logger = device.services().logging.component("marathon_ios::listener::video");
            device.services().logging.log(
                &logger,
                Level::DEBUG,
                || format!("started video recording to {path}"),
                None,
            );
        }
        let outcome = self
            .run_session(&session, &protected, batch.as_ref(), &mut deadline)
            .await;
        // The outcome comes from the exit report. Closing a channel alone cannot establish it.
        // Retain channel/reader close independently if there is no time left to await it.
        device
            .services()
            .runtime
            .spawn(Box::pin(async move { session.close().await }));
        Ok(outcome)
    }

    async fn run_session(
        &self,
        session: &CommandSession,
        context: &VendorContext,
        batch: &dyn CancellationSignal,
        deadline: &mut watch::Receiver<Option<i64>>,
    ) -> RecordingFinalization {
        let clock = context.clock.as_ref();
        let exit = std::pin::pin!(async {
            let result = session.await_result(context).await;
            match (result, session.base.exit_code.termination()) {
                (Ok(_), Some(CommandTermination::Status(0))) => RecordingFinalization::Finalized,
                (Ok(_), Some(CommandTermination::Status(_) | CommandTermination::Signal(_))) => {
                    RecordingFinalization::Unfinalized
                }
                _ => RecordingFinalization::Unconfirmed,
            }
        });
        let mut exit = exit;
        tokio::select! {
            biased;
            outcome = &mut exit => return outcome,
            _ = deadline.wait_for(Option::is_some) => {}
            _ = batch.cancelled() => self.request_stop(clock, None),
        }

        let start = clock.now_millis();
        let stages = [
            CommandSession::interrupt,
            CommandSession::terminate,
            CommandSession::kill,
        ];
        // Reserve time for TERM, KILL and the exit report. Most of the window is for the movie writer.
        for (signal, stage) in stages.into_iter().zip([4, 5, 6]) {
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
}

async fn wait_stage<T>(
    mut exit: Pin<&mut (impl Future<Output = T> + Send)>,
    deadline: &mut watch::Receiver<Option<i64>>,
    clock: &dyn VendorClock,
    start: i64,
    stage: i64,
) -> Option<T> {
    loop {
        let end = deadline.borrow_and_update().unwrap_or(start);
        let budget = end.saturating_sub(start).max(0);
        let cutoff = start.saturating_add(budget.saturating_mul(stage) / 6);
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
        let recording = Recording::new();
        {
            let mut state = self.state();
            if let Some(error) = state.refusal_error(&self.serial) {
                return Err(error);
            }
            state.recordings.retain(|record| record.ended() != Some(true));
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
            owned.done.complete(result);
            Ok(())
        }));
        Ok(recording)
    }
}
