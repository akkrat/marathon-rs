//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt

use super::*;
use marathon_apple::{
    apple_device_test_runner::{AppleDeviceTestRunner, RunError},
    cmd::{
        failure,
        remote::ssh::sshj::error::{CONNECTION_EXCEPTION, ILLEGAL_STATE_EXCEPTION, TRANSPORT_EXCEPTION, is_instance},
    },
    device::batch_failure_policy::BatchLoss,
};
use marathon_config::vendor::GrantLifecycle;
use marathon_core::executor::shared::Signal;
use marathon_error::FailureKind;

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
            if state.disposal.is_some() {
                true
            } else {
                state.running = Some(job.clone());
                false
            }
        };
        if rejected {
            return Err(BatchLoss::new(failure(
                FailureKind::IllegalState,
                format!("Device {} has been disposed", self.serial),
            ))
            .into_vendor());
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
            let outcome = device
                .run_batch(configuration, pool, batch, output, &ctx, caller_cancellation)
                .await;
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
        for line in &lines {
            self.add_line_listener(line.clone())?;
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
        for line in &lines {
            self.remove_line_listener(line)?;
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
    async fn join_cancelled(&self, job: &Arc<BatchJob>, context: &VendorContext) -> VendorResult<()> {
        job.signal.cancel(marathon_core::error::Cancellation::Requested(failure(
            FailureKind::JobCancelled,
            "LazyDeferredCoroutine was cancelled",
        )));
        let done = job.done.clone();
        // Joining observes completion, not the job's result; the timeout only bounds this wait.
        let finished = context
            .clock
            .with_timeout_or_null(
                Signal::new(),
                self.services.timeouts.cancelled_batch_millis,
                Box::new(move |signal| {
                    Box::pin(async move {
                        match done.wait_in(signal).await {
                            Err(VendorError::Cancelled(reason)) if done.completed().is_none() => {
                                Err(VendorError::Cancelled(reason))
                            }
                            _ => Ok(()),
                        }
                    })
                }),
            )
            .await?
            .is_some();
        if !finished {
            self.log(
                Level::WARN,
                format!(
                    "Cancelled batch on {} has not finished within {}, taking the device out of the run",
                    self.serial,
                    marathon_apple::bin::java_duration_of_millis(self.services.timeouts.cancelled_batch_millis)
                ),
                None,
            );
            self.retire();
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
        let (current, running, owner) = {
            let mut state = self.state();
            let owner = state.disposal.is_none();
            let current = state.disposal.get_or_insert_with(|| started.clone()).clone();
            (current, state.running.clone(), owner)
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
                let outcome: VendorResult<()> = async {
                    if let Some(running) = running {
                        device.join_cancelled(&running, &context).await?;
                    }
                    if device.connected() {
                        device
                            .handle_lifecycle(&device.services.vendor.lifecycle_configuration.on_dispose, &context)
                            .await?;
                    }
                    Ok(())
                }
                .await;
                if let Err(error) = outcome {
                    device.log(
                        Level::DEBUG,
                        "Error closing command executor",
                        Some(error.failure().clone()),
                    );
                }
                done.complete(Ok(()));
                Ok(())
            });
        }
        current.wait().await?;
        Ok(())
    }
}
