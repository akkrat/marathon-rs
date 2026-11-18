//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt

use super::*;
use marathon_apple::{
    apple_device_test_runner::{AppleDeviceTestRunner, RunError},
    cmd::{
        Termination, failure,
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

/// D93: what a sealed simulator's one window covers.
struct Receipt {
    batches: Vec<Arc<BatchJob>>,
    preparations: Vec<Arc<prepare::PreparationJob>>,
    transfers: Vec<Arc<marathon_apple::cmd::TransferReceipt>>,
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
        let sealed = self.state().shutdown_by;
        match sealed {
            Some(deadline) => {
                if let Termination::Unconfirmed(_) = self.finish_work(Some(job.clone()), deadline, context).await? {
                    self.retire();
                }
                Ok(())
            }
            None => {
                self.stop_and_join(Some(job.clone()), Vec::new(), Vec::new(), context)
                    .await
            }
        }
    }
    /// D93: seals the simulator, once: from now on it starts no batch, preparation or send, and its window for ending
    /// what it runs (`cancelled_batch_millis`) starts. Returns when that window ends, on the vendor clock. No I/O, no
    /// blocking: the run's stop calls it on the core thread.
    pub(super) fn seal(&self) -> i64 {
        let mut state = self.state();
        self.seal_locked(&mut state)
    }
    fn seal_locked(&self, state: &mut State) -> i64 {
        *state.shutdown_by.get_or_insert_with(|| {
            self.services
                .context
                .clock
                .now_millis()
                .saturating_add(self.services.timeouts.cancelled_batch_millis.max(0))
        })
    }
    /// D93: what the sealed simulator still runs, which its one window covers: `batch` (the one a cancelled call was
    /// running) and the one running now, its preparations and the sends not ended. Sealed, it registers nothing new.
    fn receipt(&self, batch: Option<Arc<BatchJob>>) -> Receipt {
        let state = self.state();
        let running = state
            .running
            .clone()
            .filter(|r| !batch.as_ref().is_some_and(|b| Arc::ptr_eq(b, r)));
        Receipt {
            batches: batch.into_iter().chain(running).collect(),
            preparations: state.preparations.clone(),
            transfers: state.unended_transfers(),
        }
    }
    /// D93: stops what the sealed simulator still runs and waits for it until `deadline`; then says whether all of it has
    /// ended: the batches, the preparations, each send with its cleanup confirmed (D91), and the last xcodebuild session,
    /// whose producer must have ended with its command closed. In the last part of the window (the force reserve), a
    /// session whose producer still runs is stopped. Every caller passes the one deadline, so no wait outlasts it.
    pub(super) async fn finish_work(
        &self,
        batch: Option<Arc<BatchJob>>,
        deadline: i64,
        context: &VendorContext,
    ) -> VendorResult<Termination> {
        let receipt = self.receipt(batch);
        let disposed = || {
            marathon_core::error::Cancellation::Requested(failure(
                FailureKind::JobCancelled,
                format!("Device {} is being disposed", self.serial),
            ))
        };
        for job in &receipt.batches {
            job.signal.cancel(marathon_core::error::Cancellation::Requested(failure(
                FailureKind::JobCancelled,
                "LazyDeferredCoroutine was cancelled",
            )));
        }
        for preparation in &receipt.preparations {
            preparation.signal.cancel(disposed());
        }
        for transfer in &receipt.transfers {
            transfer.stop(disposed(), Some(deadline));
        }
        // The clock the window was sealed on, whoever's context this is.
        let clock = self.services.context.clock.clone();
        let reserve =
            marathon_apple::cmd::device_commands::force_reserve_millis(self.services.timeouts.cancelled_batch_millis);
        let graceful = deadline.saturating_sub(reserve).saturating_sub(clock.now_millis());
        if !self.wait_for(&receipt, graceful).await? {
            let session = self.state().last_session.clone().filter(|s| s.ended().is_none());
            let left = deadline.saturating_sub(clock.now_millis()).max(0);
            let forced = async {
                if let Some(session) = session {
                    let protected = VendorContext {
                        cancellation: Signal::new(),
                        ..context.clone()
                    };
                    let _ = session
                        .stop(marathon_apple::bin::java_duration_of_millis(left), &protected)
                        .await;
                }
            };
            let (_, waited) = tokio::join!(forced, self.wait_for(&receipt, left));
            waited?;
        }
        let mut left = Vec::new();
        if receipt.batches.iter().any(|b| b.done.completed().is_none()) {
            left.push("its batch has not ended".to_owned());
        }
        if receipt.preparations.iter().any(|p| p.done.completed().is_none()) {
            left.push("a preparation has not ended".to_owned());
        }
        for transfer in &receipt.transfers {
            match transfer.ended() {
                None => left.push("a send has not ended".to_owned()),
                Some(marathon_apple::cmd::TransferCleanup::Unconfirmed(why)) => left.push(format!(
                    "a send's cleanup was not confirmed ({})",
                    why.message.as_deref().unwrap_or_default()
                )),
                Some(marathon_apple::cmd::TransferCleanup::Confirmed) => {}
            }
        }
        if let Some(session) = self.state().last_session.clone().filter(|s| s.stopping()) {
            match session.ended() {
                None => left.push("its xcodebuild session has not ended".to_owned()),
                Some(false) => left.push("its xcodebuild session's command did not close".to_owned()),
                Some(true) => {}
            }
        }
        Ok(if left.is_empty() {
            Termination::Confirmed
        } else {
            Termination::Unconfirmed(left)
        })
    }
    /// Waits up to `millis` for everything in `receipt`, and then for the last xcodebuild session's producer (a batch
    /// opens its session itself); whether all of it ended. Nothing left: no wait, only a look.
    async fn wait_for(&self, receipt: &Receipt, millis: i64) -> VendorResult<bool> {
        let ended = |device: &Self| {
            receipt.batches.iter().all(|b| b.done.completed().is_some())
                && receipt.preparations.iter().all(|p| p.done.completed().is_some())
                && receipt.transfers.iter().all(|t| t.ended().is_some())
                && device
                    .state()
                    .last_session
                    .as_ref()
                    .is_none_or(|s| !s.stopping() || s.ended().is_some())
        };
        if ended(self) {
            return Ok(true);
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
        let me = self.this()?;
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
                        let session = me.state().last_session.clone().filter(|s| s.stopping());
                        if let Some(session) = session {
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
        Ok(finished && ended(self))
    }
    /// D93: the disposal could not confirm that the simulator's work had ended: one WARN naming it and what was left, no
    /// on-dispose lifecycle, no further command from it, and its lease quarantined rather than released.
    fn quarantine(&self, left: &[String], window: i64) {
        let limit = marathon_core::error::duration_for_log(marathon_apple::bin::java_duration_of_millis(window));
        let why = failure(
            FailureKind::IllegalState,
            format!(
                "Device {}'s work was not confirmed to have ended: {}",
                self.serial,
                left.join(", ")
            ),
        );
        self.commands.revoke(why);
        {
            let mut state = self.state();
            state.quarantined = true;
            state.retired = true;
        }
        self.log(
            Level::WARN,
            format!(
                "Device {}'s work was not confirmed to have ended within {limit} ({}); not running its on-dispose lifecycle, sending it no further command and keeping its connection out of use",
                self.serial,
                left.join(", "),
            ),
            None,
        );
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
        let (current, deadline, owner) = {
            let mut state = self.state();
            let owner = state.disposal.is_none();
            let current = state.disposal.get_or_insert_with(|| started.clone()).clone();
            // U179: from here on no preparation starts (`prepare_device` checks `disposal` under this lock), and, D91, no
            // send (`send_stop`). D93: the window is the one the run's stop sealed, or starts with this first disposal.
            let deadline = self.seal_locked(&mut state);
            (current, deadline, owner)
        };
        if owner {
            let device = self.this()?;
            let done = started.clone();
            // Disposal is protected and separately owned even if an async adapter is dropped.
            self.retained.spawn(self.services.runtime.as_ref(), async move {
                let context = VendorContext {
                    cancellation: Signal::new(),
                    ..device.services.context.clone()
                };
                // D84: a panic is published to every caller too; the device is then as disposed as the panic left it.
                let outcome = panic_as_error(async {
                    let disposed: VendorResult<()> = async {
                        // D93: only work confirmed to have ended lets the on-dispose lifecycle run.
                        match device.finish_work(None, deadline, &context).await? {
                            Termination::Confirmed => {
                                if device.connected() {
                                    device
                                        .handle_lifecycle(
                                            &device.services.vendor.lifecycle_configuration.on_dispose,
                                            &context,
                                        )
                                        .await?;
                                }
                            }
                            Termination::Unconfirmed(left) => {
                                device.quarantine(&left, device.services.timeouts.cancelled_batch_millis)
                            }
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
