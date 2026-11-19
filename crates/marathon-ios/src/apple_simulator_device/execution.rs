//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt

use super::*;
use marathon_apple::{
    apple_device_test_runner::{AppleDeviceTestRunner, RunError},
    cmd::{
        ShutdownWindow, Termination, TransferCleanup, failure,
        remote::ssh::sshj::error::{CONNECTION_EXCEPTION, ILLEGAL_STATE_EXCEPTION, TRANSPORT_EXCEPTION, is_instance},
        retained_tasks::panic_as_error,
    },
    device::batch_failure_policy::BatchLoss,
};
use marathon_config::vendor::GrantLifecycle;
use marathon_core::executor::shared::Signal;
use marathon_error::FailureKind;

/// The line listeners a batch has installed, removed when it is dropped.
struct InstalledLines<'a> {
    device: &'a AppleSimulatorDevice,
    lines: Vec<Arc<dyn LineListener>>,
}
impl Drop for InstalledLines<'_> {
    fn drop(&mut self) {
        for line in std::mem::take(&mut self.lines) {
            let _ = self.device.remove_line_listener(&line);
        }
    }
}

/// D93: a sealed simulator's one window, with its verdict, and the receipt it is decided on: what the simulator ran when
/// it was sealed. Sealed, it starts nothing new.
pub(super) struct Shutdown {
    window: Arc<ShutdownWindow>,
    /// Sealed by the run's stop, whose published bound the lifecycle and the release share; a disposal outside a stop
    /// runs them under their own commands' limits, as before.
    by_run_stop: bool,
    batches: Vec<Arc<BatchJob>>,
    preparations: Vec<Arc<prepare::PreparationJob>>,
    transfers: Vec<Arc<marathon_apple::cmd::TransferReceipt>>,
    /// The xcodebuild sessions registered and not confirmed to have ended when it was sealed: one still running (a
    /// batch's, a parse's) and one whose exec or close could not be confirmed. No session registers after the seal.
    sessions: Vec<Arc<marathon_apple::xcodebuild_session::XcodebuildSession>>,
}
pub(super) struct BatchJob {
    signal: Arc<Signal>,
    done: Arc<SharedDeferred<()>>,
    outcome: Mutex<Option<Result<(), RunError>>>,
}
impl AppleSimulatorDevice {
    pub(super) async fn execute_batch(
        &self,
        configuration: Arc<Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorResult<()> {
        let outcome = self
            .execute_registered(configuration, pool, batch, result, &context)
            .await;
        // Kotlin finally runs even for disposal's rejection, before an unstarted batch runs.
        let finished = self
            .services
            .monotonic
            .nanos("ios.AppleSimulatorDevice.nanoTime")
            .map_err(clock_error)?;
        self.state().previous_batch_finished_at = Some(finished);
        outcome
    }
    async fn execute_registered(
        &self,
        configuration: Arc<Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<Deferred<TestBatchResults>>,
        context: &VendorContext,
    ) -> VendorResult<()> {
        let job = Arc::new(BatchJob {
            signal: Signal::new(),
            done: SharedDeferred::new(),
            outcome: Mutex::new(None),
        });
        let rejected = {
            let mut state = self.state();
            let refusal = state.refusal(&self.serial);
            if refusal.is_none() {
                state.running = Some(job.clone());
            }
            refusal
        };
        if let Some(refusal) = rejected {
            return Err(BatchLoss::new(failure(FailureKind::IllegalState, refusal)).into_vendor());
        }
        let device = self.this()?;
        let running = job.clone();
        let ctx = VendorContext {
            cancellation: job.signal.clone(),
            ..context.clone()
        };
        let output = result.clone();
        let batch_id = batch.id.clone();
        let caller_cancellation = context.cancellation.clone();
        self.retained.spawn(self.services.runtime.as_ref(), async move {
            // D84: a panic is this batch's error too, so that the caller and disposal hear of its end.
            let outcome = match panic_as_error(async {
                Ok(device
                    .run_batch(configuration, pool, batch, output, &ctx, caller_cancellation)
                    .await)
            })
            .await
            {
                Ok(outcome) => outcome,
                Err(panic) => Err(RunError::from(panic)),
            };
            let completion = outcome.clone().map_err(RunError::into_vendor);
            *running.outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
            {
                let mut state = device.state();
                if state
                    .running
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &running))
                {
                    state.running = None;
                }
            }
            running.done.complete(completion);
            Ok(())
        });
        let awaited = job.done.wait_in(context.cancellation.clone()).await.map(|_| ());
        let outcome = match awaited {
            Ok(()) => Ok(()),
            Err(error) => {
                let recorded = job.outcome.lock().unwrap_or_else(|e| e.into_inner()).clone();
                match recorded {
                    Some(Err(original)) if original.clone().into_vendor() == error => Err(original),
                    _ => Err(RunError::from(error)),
                }
            }
        };
        self.classify(outcome, &job, &batch_id, &result, context).await
    }
    async fn run_batch(
        &self,
        configuration: Arc<Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<Deferred<TestBatchResults>>,
        context: &VendorContext,
        caller_cancellation: Arc<dyn marathon_core::runtime::CancellationSignal>,
    ) -> Result<(), RunError> {
        if let Some(reason) = context.cancellation.reason() {
            return Err(VendorError::Cancelled(reason).into());
        }
        let previous = self.state().previous_batch_finished_at;
        if let Some(previous) = previous
            && self.services.logging.enabled(&self.logger, Level::DEBUG)
        {
            let now = self
                .services
                .monotonic
                .nanos("ios.AppleSimulatorDevice.nanoTime")
                .map_err(clock_error)?;
            self.log(
                Level::DEBUG,
                format!(
                    "Device {} idle gap before batch {}: {} ms",
                    self.serial,
                    batch.id.to_owned(),
                    now.wrapping_sub(previous) / 1_000_000
                ),
                None,
            );
        }
        let (listeners, lines) =
            self.create_execution_listeners(pool, batch.clone(), result.clone(), context.clone())?;
        // D84: removed however the batch ends, an unwind included, so that the next batch does not inherit them.
        let mut installed = InstalledLines {
            device: self,
            lines: Vec::new(),
        };
        for line in &lines {
            self.add_line_listener(line.clone())?;
            installed.lines.push(line.clone());
        }
        let outcome = async {
            let bundle = self.state().bundle.clone().ok_or_else(|| {
                VendorError::Other(failure(
                    FailureKind::UninitializedProperty,
                    "lateinit property testBundle has not been initialized",
                ))
            })?;
            if self.services.vendor.permissions.lifecycle == GrantLifecycle::BeforeEachBatch {
                for permission in &self.services.vendor.permissions.grant {
                    self.grant(*permission, bundle.app_id(), context).await?;
                }
            }
            if let Some(reason) = context.cancellation.reason() {
                return Err(VendorError::Cancelled(reason).into());
            }
            let mut runner = AppleDeviceTestRunner::new(
                self.this()?,
                self.services.runtime.clone(),
                self.services.logging.clone(),
            );
            runner.observe_batch_results(result, caller_cancellation);
            runner.session_close_timeout =
                marathon_apple::bin::java_duration_of_millis(self.services.timeouts.session_close_millis);
            runner
                .execute(&configuration, &bundle, &batch, &listeners, context)
                .await
        }
        .await;
        for line in std::mem::take(&mut installed.lines) {
            self.remove_line_listener(&line)?;
        }
        outcome
    }
    async fn classify(
        &self,
        outcome: Result<(), RunError>,
        job: &Arc<BatchJob>,
        batch: &str,
        results: &Arc<Deferred<TestBatchResults>>,
        context: &VendorContext,
    ) -> VendorResult<()> {
        let completed = results.completed();
        let progress = completed
            .as_ref()
            .is_some_and(|r| r.as_ref().is_ok_and(|r| !r.finished.is_empty() || !r.failed.is_empty()));
        let error = match outcome {
            Ok(()) => {
                let loss = self.state().policy.on_batch_completed(progress);
                if let Some(loss) = loss {
                    self.mark_lost();
                    return Err(loss.into_vendor());
                }
                return Ok(());
            }
            Err(error) => error,
        };
        if matches!(error.error, VendorError::DeviceLost(_)) {
            self.retire();
            return Err(error.into_vendor());
        }
        if error.error.failure().kind == FailureKind::Timeout {
            let error = error.into_vendor();
            let loss = self
                .state()
                .policy
                .on_batch_timed_out(error.failure().clone(), progress);
            if let Some(loss) = loss {
                self.mark_lost();
                return Err(loss.into_vendor());
            }
            return Err(error);
        }
        if matches!(error.error, VendorError::Cancelled(_)) {
            self.join_cancelled(job, context).await?;
            if context.cancellation.reason().is_none() {
                return Err(BatchLoss::new(error.into_vendor().failure().clone()).into_vendor());
            }
            return Err(error.into_vendor());
        }
        let class = error.error.failure().kind.to_owned();
        if [CONNECTION_EXCEPTION, TRANSPORT_EXCEPTION, ILLEGAL_STATE_EXCEPTION]
            .iter()
            .any(|base| is_instance(&class, base.clone()))
        {
            self.mark_lost();
            return Err(BatchLoss::new(error.into_vendor().failure().clone()).into_vendor());
        }
        if let Some(reason) = error.reason {
            let results = completed.transpose()?;
            let passed = results.as_ref().is_some_and(|r| !r.finished.is_empty());
            let loss = self.state().policy.on_batch_failed(
                reason,
                error.clone().into_vendor().failure().clone(),
                passed,
                progress,
            );
            if let Some(loss) = loss {
                if listeners::not_fixed_by_setup(reason) {
                    self.retire();
                } else {
                    self.mark_lost();
                }
                return Err(loss.into_vendor());
            }
            if results.is_none() {
                return Err(VendorError::BatchExecution(failure(
                    FailureKind::BatchExecution,
                    error
                        .error
                        .failure()
                        .message
                        .to_owned()
                        .unwrap_or_else(|| reason.name().into()),
                )));
            }
            self.log(
                Level::WARN,
                format!(
                    "Test batch {} failed on {}, keeping the device",
                    batch.to_owned(),
                    self.serial
                ),
                Some(error.into_vendor().failure().clone()),
            );
            return Ok(());
        }
        Err(error.into_vendor())
    }
    /// A batch whose call was cancelled. D93: once the simulator is sealed this is part of its one window, and what did
    /// not end in it is for its disposal to report; otherwise the call has its own window, as in Kotlin.
    async fn join_cancelled(&self, job: &Arc<BatchJob>, context: &VendorContext) -> VendorResult<()> {
        let sealed = self.state().shutdown.clone();
        if let Some(shutdown) = sealed {
            if let Termination::Unconfirmed(_) = self.finish_work().await {
                self.retire();
            }
            // An error while deciding is this call's error too, as the window's wait's was before D93.
            return match shutdown.window.failure() {
                Some(error) => Err(error),
                None => Ok(()),
            };
        }
        self.stop_and_join(Some(job.clone()), Vec::new(), Vec::new(), context)
            .await
    }
    /// D93: seals the simulator, once: from now on it starts no batch, preparation or send, its window for ending what it
    /// runs (`cancelled_batch_millis`) starts, and what it runs now is the receipt its one verdict is decided on. No I/O,
    /// no blocking: the run's stop calls it on the core thread.
    pub(super) fn seal(&self) -> Arc<Shutdown> {
        let mut state = self.state();
        self.seal_locked(&mut state, false)
    }
    /// The run's stop: its window also bounds the on-dispose lifecycle and the lease's release (D93's 75 s).
    pub(super) fn seal_for_run_stop(&self) {
        let mut state = self.state();
        self.seal_locked(&mut state, true);
    }
    fn seal_locked(&self, state: &mut State, by_run_stop: bool) -> Arc<Shutdown> {
        if let Some(shutdown) = &state.shutdown {
            return shutdown.clone();
        }
        let deadline = self
            .services
            .context
            .clock
            .now_millis()
            .saturating_add(self.services.timeouts.cancelled_batch_millis.max(0));
        let shutdown = Arc::new(Shutdown {
            window: ShutdownWindow::new(deadline),
            by_run_stop,
            batches: state.running.clone().into_iter().collect(),
            preparations: state.preparations.clone(),
            // D91: a send whose cleanup was not confirmed earlier in the run counts too (`send_stop` keeps those).
            transfers: state
                .transfers
                .iter()
                .filter(|t| t.ended() != Some(TransferCleanup::Confirmed))
                .cloned()
                .collect(),
            sessions: state
                .sessions
                .iter()
                .filter(|s| s.ended() != Some(true))
                .cloned()
                .collect(),
        });
        state.shutdown = Some(shutdown.clone());
        match self.me.upgrade() {
            Some(device) => {
                let deciding = shutdown.clone();
                self.retained.spawn(self.services.runtime.as_ref(), async move {
                    device.decide(deciding).await;
                    Ok(())
                });
            }
            None => {
                shutdown.window.decide(Termination::Unconfirmed(vec![
                    "the simulator's allocation was released".into(),
                ]));
            }
        }
        shutdown
    }
    /// D93: the one decision on the sealed simulator's work, latched in its window; a failure or a panic while deciding is
    /// `Unconfirmed`.
    async fn decide(&self, shutdown: Arc<Shutdown>) {
        match panic_as_error(self.await_receipt(&shutdown)).await {
            Ok(verdict) => {
                shutdown.window.decide(verdict);
            }
            Err(error) => shutdown.window.fail(error),
        }
    }
    /// Waits for the receipt until the window's end, forcing in its reserve what has not ended; then what has not.
    async fn await_receipt(&self, shutdown: &Shutdown) -> VendorResult<Termination> {
        // The clock the window was sealed on.
        let clock = self.services.context.clock.clone();
        let deadline = shutdown.window.deadline;
        let reserve =
            marathon_apple::cmd::device_commands::force_reserve_millis(self.services.timeouts.cancelled_batch_millis);
        let graceful = deadline.saturating_sub(reserve).saturating_sub(clock.now_millis());
        // D93 (review round 2): the verdict is these waits' outcome. Work not seen ended before the deadline is unconfirmed,
        // whatever a later look would find.
        let in_time = self.wait_for(shutdown, graceful, deadline).await?;
        let in_time = in_time || {
            self.stop_all(shutdown);
            let left = deadline.saturating_sub(clock.now_millis()).max(0);
            let protected = VendorContext {
                cancellation: Signal::new(),
                ..self.services.context.clone()
            };
            let forced = shutdown.sessions.iter().filter(|s| s.ended().is_none()).map(|session| {
                let protected = protected.clone();
                async move {
                    let _ = session
                        .stop(marathon_apple::bin::java_duration_of_millis(left), &protected)
                        .await;
                }
            });
            let (_, waited) = tokio::join!(
                futures_util::future::join_all(forced),
                self.wait_for(shutdown, left, deadline)
            );
            waited?
        };
        let mut left = Vec::new();
        if shutdown.batches.iter().any(|b| b.done.completed().is_none()) {
            left.push("its batch has not ended".to_owned());
        }
        if shutdown.preparations.iter().any(|p| p.done.completed().is_none()) {
            left.push("a preparation has not ended".to_owned());
        }
        for transfer in &shutdown.transfers {
            match transfer.ended() {
                None => left.push("a send has not ended".to_owned()),
                Some(TransferCleanup::Unconfirmed(why)) => left.push(format!(
                    "a send's cleanup was not confirmed ({})",
                    why.message.as_deref().unwrap_or_default()
                )),
                Some(TransferCleanup::Confirmed) => {}
            }
        }
        for session in &shutdown.sessions {
            match session.ended() {
                None => left.push("an xcodebuild session has not ended".to_owned()),
                Some(false) => left.push("an xcodebuild session was not confirmed to have closed".to_owned()),
                Some(true) => {}
            }
        }
        if !in_time {
            if left.is_empty() {
                left.push("its work was not seen to end before its window ended".to_owned());
            }
            return Ok(Termination::Unconfirmed(left));
        }
        Ok(if left.is_empty() {
            Termination::Confirmed
        } else {
            Termination::Unconfirmed(left)
        })
    }
    /// D93: stops everything in the receipt: the batch, the preparations, the sends (by the window's end). Idempotent.
    fn stop_all(&self, shutdown: &Shutdown) {
        let disposed = || {
            marathon_core::error::Cancellation::Requested(failure(
                FailureKind::JobCancelled,
                format!("Device {} is being disposed", self.serial),
            ))
        };
        for job in &shutdown.batches {
            job.signal.cancel(marathon_core::error::Cancellation::Requested(failure(
                FailureKind::JobCancelled,
                "LazyDeferredCoroutine was cancelled",
            )));
        }
        for preparation in &shutdown.preparations {
            preparation.signal.cancel(disposed());
        }
        for transfer in &shutdown.transfers {
            transfer.stop(disposed(), Some(shutdown.window.deadline));
        }
    }
    /// D93: seals the simulator if it is not yet, stops what it runs and returns its one verdict, decided at most once,
    /// within its window. Every caller (the cancelled call, the run's stop, disposal) reads the same verdict.
    pub(super) async fn finish_work(&self) -> Termination {
        let shutdown = self.seal();
        self.stop_all(&shutdown);
        shutdown.window.verdict().await
    }
    /// Waits up to `millis` for everything in the receipt, and then for the last xcodebuild session's producer (a batch
    /// opens its session itself); whether all of it ended. Nothing left: no wait, only a look.
    async fn wait_for(&self, receipt: &Shutdown, millis: i64, deadline: i64) -> VendorResult<bool> {
        let ended = || {
            receipt.batches.iter().all(|b| b.done.completed().is_some())
                && receipt.preparations.iter().all(|p| p.done.completed().is_some())
                && receipt.transfers.iter().all(|t| t.ended().is_some())
                && receipt.sessions.iter().all(|s| s.ended().is_some())
        };
        if ended() {
            // A look after the deadline counts for nothing.
            return Ok(self.services.context.clock.now_millis() <= deadline);
        }
        if millis <= 0 {
            return Ok(false);
        }
        let cells: Vec<_> = receipt
            .batches
            .iter()
            .map(|b| b.done.clone())
            .chain(receipt.preparations.iter().map(|p| p.done.clone()))
            .collect();
        let sends = receipt.transfers.clone();
        let sessions = receipt.sessions.clone();
        // Joining observes completion, not the jobs' results; the timeout only bounds this wait.
        let finished = self
            .services
            .context
            .clock
            .with_timeout_or_null(
                Signal::new(),
                millis,
                Box::new(move |signal| {
                    Box::pin(async move {
                        for done in cells {
                            match done.wait_in(signal.clone()).await {
                                Err(VendorError::Cancelled(reason)) if done.completed().is_none() => {
                                    return Err(VendorError::Cancelled(reason));
                                }
                                _ => {}
                            }
                        }
                        for send in sends {
                            if let Err(VendorError::Cancelled(reason)) = send.wait_in(signal.clone()).await
                                && send.ended().is_none()
                            {
                                return Err(VendorError::Cancelled(reason));
                            }
                        }
                        for session in sessions {
                            tokio::select! { biased;
                                reason = signal.cancelled() => return Err(VendorError::Cancelled(reason)),
                                _ = session.wait_ended() => {}
                            }
                        }
                        Ok(())
                    })
                }),
            )
            .await?
            .is_some();
        Ok(finished && ended())
    }
    /// D93: the simulator's lifecycle and lease are no longer safe: once, one WARN (`warning`), its commands revoked with
    /// `why`, its lease quarantined in its connection factory now, and the simulator retired.
    fn quarantine(&self, warning: String, why: String) {
        {
            let mut state = self.state();
            if state.quarantined {
                return;
            }
            state.quarantined = true;
            state.retired = true;
        }
        self.commands.quarantine(failure(
            FailureKind::IllegalState,
            format!("Device {}: {why}", self.serial),
        ));
        self.log(Level::WARN, warning, None);
    }
    /// D93: what could not be confirmed to have ended.
    fn quarantine_unconfirmed(&self, left: &[String]) {
        let limit = marathon_core::error::duration_for_log(marathon_apple::bin::java_duration_of_millis(
            self.services.timeouts.cancelled_batch_millis,
        ));
        self.quarantine(
            format!(
                "Device {}'s work was not confirmed to have ended within {limit} ({}); not running its on-dispose lifecycle, sending it no further command and keeping its connection out of use",
                self.serial,
                left.join(", "),
            ),
            format!("its work was not confirmed to have ended: {}", left.join(", ")),
        );
    }
    /// D93: the on-dispose lifecycle of a simulator whose work was confirmed to have ended. After the run's stop, within
    /// what is left of its window, and at least its force reserve (5 s of 70): a sibling that held the stop to the window's
    /// end must not cost a confirmed simulator its lifecycle. A lifecycle that does not end in that time may have left a
    /// command running: quarantined. Outside a stop it runs as before, each command within its own limit.
    async fn run_on_dispose(&self, shutdown: &Shutdown) -> VendorResult<()> {
        let actions = &self.services.vendor.lifecycle_configuration.on_dispose;
        if actions.is_empty() || !self.connected() {
            return Ok(());
        }
        if !shutdown.by_run_stop {
            let context = VendorContext {
                cancellation: Signal::new(),
                ..self.services.context.clone()
            };
            return self.handle_lifecycle(actions, &context).await;
        }
        let limit = marathon_core::error::duration_for_log(marathon_apple::bin::java_duration_of_millis(
            self.services.timeouts.cancelled_batch_millis,
        ));
        let lifecycle_cut = |device: &Self, why: &str| {
            device.quarantine(
                format!(
                    "Device {}'s on-dispose lifecycle {why} within its {limit} window; sending it no further command and keeping its connection out of use",
                    device.serial
                ),
                format!("its on-dispose lifecycle {why} within its window"),
            )
        };
        let clock = self.services.context.clock.clone();
        let reserve =
            marathon_apple::cmd::device_commands::force_reserve_millis(self.services.timeouts.cancelled_batch_millis);
        let left = shutdown.window.deadline.saturating_sub(clock.now_millis()).max(reserve);
        // D93 (review rounds 2, 3): at the bound the disposal stops waiting for the lifecycle, and does not drop it: it runs
        // in a task of the simulator's own, cancelled there, so that a command whose reply is pending still gets its
        // executor's own bounded cleanup (D30's close; U154's CLOSE for an exec with no reply) rather than being cut off.
        // A command deaf to its cancellation is left to that cleanup and to the quarantine.
        let signal = Signal::new();
        let context = VendorContext {
            cancellation: signal.clone(),
            ..self.services.context.clone()
        };
        let ran = SharedDeferred::new();
        {
            let (device, actions, ran) = (self.this()?, actions.clone(), ran.clone());
            self.retained
                .spawn_published(self.services.runtime.as_ref(), ran, async move {
                    device.handle_lifecycle(&actions, &context).await
                });
        }
        let ended = tokio::select! { biased;
            ended = ran.wait() => Some(ended),
            _ = clock.delay(left, Signal::new()) => None,
        };
        match ended {
            Some(ended) => {
                ended?;
            }
            None => {
                signal.cancel(marathon_core::error::Cancellation::Requested(failure(
                    FailureKind::JobCancelled,
                    format!("Device {}'s on-dispose lifecycle ran out of its window", self.serial),
                )));
                lifecycle_cut(self, "did not end");
            }
        }
        Ok(())
    }
    /// D93: the window's end, on the vendor clock, if the run's stop sealed the simulator: its lease's release is held to
    /// it (a disposal outside a stop waits for the release as before).
    pub fn shutdown_deadline(&self) -> Option<i64> {
        self.state()
            .shutdown
            .as_ref()
            .filter(|s| s.by_run_stop)
            .map(|s| s.window.deadline)
    }
    /// Cancels the batch, the preparations and the sends given and waits for all of them within one bound, the cancelled
    /// batch's. U179: it waits on their completion cells, not on the retained tasks, which also hold disposal itself.
    /// D91: a send's cell is its receipt, which its owner ends however long ago its caller stopped waiting.
    pub(super) async fn stop_and_join(
        &self,
        batch: Option<Arc<BatchJob>>,
        preparations: Vec<Arc<prepare::PreparationJob>>,
        transfers: Vec<Arc<marathon_apple::cmd::TransferReceipt>>,
        context: &VendorContext,
    ) -> VendorResult<()> {
        if let Some(job) = &batch {
            job.signal.cancel(marathon_core::error::Cancellation::Requested(failure(
                FailureKind::JobCancelled,
                "LazyDeferredCoroutine was cancelled",
            )));
        }
        for preparation in &preparations {
            preparation
                .signal
                .cancel(marathon_core::error::Cancellation::Requested(failure(
                    FailureKind::JobCancelled,
                    format!("Device {} is being disposed", self.serial),
                )));
        }
        // D91: a send's cleanup is to have ended where this wait does, if not sooner by the bridge's own bound.
        let by = context
            .clock
            .now_millis()
            .saturating_add(self.services.timeouts.cancelled_batch_millis);
        for transfer in &transfers {
            transfer.stop(
                marathon_core::error::Cancellation::Requested(failure(
                    FailureKind::JobCancelled,
                    format!("Device {} is being disposed", self.serial),
                )),
                Some(by),
            );
        }
        let cells: Vec<_> = batch
            .iter()
            .map(|job| job.done.clone())
            .chain(preparations.iter().map(|p| p.done.clone()))
            .collect();
        if cells.is_empty() && transfers.is_empty() {
            return Ok(());
        }
        let sends = transfers.clone();
        // Joining observes completion, not the jobs' results; the timeout only bounds this wait.
        let finished = context
            .clock
            .with_timeout_or_null(
                Signal::new(),
                self.services.timeouts.cancelled_batch_millis,
                Box::new(move |signal| {
                    Box::pin(async move {
                        for done in cells {
                            match done.wait_in(signal.clone()).await {
                                Err(VendorError::Cancelled(reason)) if done.completed().is_none() => {
                                    return Err(VendorError::Cancelled(reason));
                                }
                                _ => {}
                            }
                        }
                        for send in sends {
                            if let Err(VendorError::Cancelled(reason)) = send.wait_in(signal.clone()).await
                                && send.ended().is_none()
                            {
                                return Err(VendorError::Cancelled(reason));
                            }
                        }
                        Ok(())
                    })
                }),
            )
            .await?
            .is_some();
        if finished {
            return Ok(());
        }
        let limit = marathon_core::error::duration_for_log(marathon_apple::bin::java_duration_of_millis(
            self.services.timeouts.cancelled_batch_millis,
        ));
        if batch.is_some_and(|job| job.done.completed().is_none()) {
            self.log(
                Level::WARN,
                format!(
                    "Cancelled batch on {} has not finished within {limit}, taking the device out of the run",
                    self.serial,
                ),
                None,
            );
            self.retire();
        }
        if preparations.iter().any(|p| p.done.completed().is_none()) {
            self.log(
                Level::WARN,
                format!(
                    "Cancelled preparation of {} has not finished within {limit}, disposing of the simulator anyway",
                    self.serial,
                ),
                None,
            );
        }
        if transfers.iter().any(|t| t.ended().is_none()) {
            self.log(
                Level::WARN,
                format!(
                    "Cancelled send to {} has not finished within {limit}, disposing of the simulator anyway",
                    self.serial,
                ),
                None,
            );
        }
        Ok(())
    }
    pub async fn await_session_closed(
        &self,
        timeout: jcompat::jtime::Duration,
        context: &VendorContext,
    ) -> VendorResult<bool> {
        let session = self.state().last_session.clone();
        match session {
            Some(session) => session.await_closed(timeout, context).await,
            None => Ok(true),
        }
    }
    pub async fn dispose_and_join(&self) -> VendorResult<()> {
        let started = SharedDeferred::new();
        let (current, shutdown, owner) = {
            let mut state = self.state();
            let owner = state.disposal.is_none();
            let current = state.disposal.get_or_insert_with(|| started.clone()).clone();
            // U179: from here on no preparation starts (`prepare_device` checks `disposal` under this lock), and, D91, no
            // send (`send_stop`). D93: the window is the one the run's stop sealed, or starts with this first disposal.
            let shutdown = self.seal_locked(&mut state, false);
            (current, shutdown, owner)
        };
        if owner {
            let device = self.this()?;
            let done = started.clone();
            // Disposal is protected and separately owned even if an async adapter is dropped.
            self.retained.spawn(self.services.runtime.as_ref(), async move {
                // D84: a panic is published to every caller too; the device is then as disposed as the panic left it.
                let outcome = panic_as_error(async {
                    let disposed: VendorResult<()> = async {
                        // D93: only work confirmed to have ended lets the on-dispose lifecycle run, within the window.
                        match device.finish_work().await {
                            Termination::Confirmed => device.run_on_dispose(&shutdown).await?,
                            Termination::Unconfirmed(left) => device.quarantine_unconfirmed(&left),
                        }
                        Ok(())
                    }
                    .await;
                    Ok(disposed)
                })
                .await;
                match outcome {
                    Ok(Ok(())) => {
                        done.complete(Ok(()));
                    }
                    Ok(Err(error)) => {
                        device.log(
                            Level::DEBUG,
                            "Error closing command executor",
                            Some(error.failure().clone()),
                        );
                        done.complete(Ok(()));
                    }
                    Err(panic) => {
                        device.log(Level::ERROR, "Disposal failed", Some(panic.failure().clone()));
                        // D93: a disposal that panicked has confirmed nothing.
                        device.quarantine_unconfirmed(&[marathon_apple::cmd::device_commands::failure_text(
                            panic.failure(),
                        )]);
                        done.complete(Err(panic));
                    }
                }
                Ok(())
            });
        }
        current.wait().await?;
        Ok(())
    }
}
