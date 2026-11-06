//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt:279–369,452–681,845–923

use super::*;
use crate::device::{
    error::{SetupDropped, SetupError},
    pending_worker::PendingWorker,
};
use marathon_apple::device::{
    connection_factory::{ConnectionFuture, Executor},
    connection_failures::is_transient_connection_failure,
};
use marathon_core::runtime::CancellationSignal;
use tokio::sync::Semaphore;

mod setup_retry;

#[derive(Default)]
struct PolicyState {
    provider: Weak<AppleSimulatorProvider>,
    pending: IndexMap<Transport, Arc<PendingWorker>>,
    losses: IndexMap<JavaString, i32>,
    awaiting_admission: IndexSet<JavaString>,
    closed: Option<String>,
    setup: setup_retry::SetupRetries,
}
/// One supervised retry root per run, independent of monitoring's recovery children.
pub struct SimulatorRecovery {
    me: Weak<Self>,
    state: Mutex<PolicyState>,
    recoveries: RetainedTasks,
    retries: RetainedTasks,
    recover_cancel: Arc<Signal>,
    retry_cancel: Arc<Signal>,
    permits: Semaphore,
    max_recoveries: i32,
    recovery_pause_millis: i64,
}
impl SimulatorRecovery {
    pub fn new() -> Arc<Self> {
        Self::with_recovery_settings(3, 30_000)
    }
    pub fn with_recovery_settings(max_recoveries: i32, recovery_pause_millis: i64) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            state: Mutex::default(),
            recoveries: RetainedTasks::default(),
            retries: RetainedTasks::default(),
            recover_cancel: Signal::new(),
            retry_cancel: Signal::new(),
            permits: Semaphore::new(2),
            max_recoveries,
            recovery_pause_millis,
        })
    }
    fn state(&self) -> MutexGuard<'_, PolicyState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn provider(&self) -> VendorResult<Arc<AppleSimulatorProvider>> {
        self.state().provider.upgrade().ok_or_else(|| {
            other(
                "java.lang.IllegalStateException",
                "Provider allocation has been released",
            )
        })
    }
    pub fn late_join_cutoff(&self) -> VendorResult<Option<String>> {
        if let Some(reason) = self.state().closed.clone() {
            return Ok(Some(reason));
        }
        let provider = self.provider()?;
        let cutoff = provider.services.vendor.worker_connection.late_join_cutoff;
        let now = provider
            .services
            .context
            .hooks
            .nanos("ios.AppleSimulatorProvider.lateJoinCutOff")
            .map_err(clock_error)?;
        let reason = if !cutoff.is_zero() && provider.services.timing.elapsed()? > duration_millis(cutoff)? {
            Some(format!("the run started more than {} s ago", cutoff.seconds()))
        } else {
            provider
                .devices()
                .into_iter()
                .find(|d| {
                    d.handle
                        .waiting_since()
                        .is_some_and(|since| now.wrapping_sub(since) > 30_000_000_000)
                })
                .map(|d| format!("{} has been waiting for a batch for more than 30 s", d.handle.serial()))
        };
        if let Some(reason) = reason {
            // Approved P2: the first observed reason wins, including concurrent callers.
            let mut state = self.state();
            Ok(Some(state.closed.get_or_insert(reason).clone()))
        } else {
            Ok(self.state().closed.clone())
        }
    }
    fn cutoff(&self, provider: &AppleSimulatorProvider, transport: &Transport) -> VendorResult<bool> {
        let Some(reason) = self.late_join_cutoff()? else {
            return Ok(false);
        };
        provider.log(
            Level::Info,
            || {
                format!(
                    "Worker {} doesn't join the run late, since {reason}. No longer connecting to it",
                    transport.id().to_string_lossy()
                )
            },
            None,
        );
        Ok(true)
    }
    fn not_admitted(&self, provider: &AppleSimulatorProvider, device: &Simulator) -> VendorResult<bool> {
        let id = apple_id(&device.udid().into(), device.transport());
        if !self.state().awaiting_admission.contains(&id) {
            return Ok(false);
        }
        let Some(reason) = self.late_join_cutoff()? else {
            return Ok(false);
        };
        provider.log(
            Level::Info,
            || {
                format!(
                    "Simulator {} doesn't join the run late, since {reason}",
                    device.serial()
                )
            },
            None,
        );
        device.retire();
        Ok(true)
    }
    fn retry_later(
        &self,
        transport: Transport,
        targets: Vec<AppleTarget>,
        started: i64,
        failure: ProvisioningError,
    ) -> VendorResult<()> {
        let provider = self.provider()?;
        let pending = {
            let mut state = self.state();
            if self.retry_cancel.reason().is_some() {
                return Ok(());
            }
            if state.pending.get(&transport).is_some_and(|p| p.offer(&targets)) {
                return Ok(());
            }
            let pending = Arc::new(PendingWorker::new(targets));
            state.pending.insert(transport.clone(), pending.clone());
            pending
        };
        let this = self.me.upgrade().ok_or_else(|| {
            other(
                "java.lang.IllegalStateException",
                "Recovery allocation has been released",
            )
        })?;
        let caller = VendorContext {
            cancellation: self.retry_cancel.clone(),
            ..provider.services.context.clone()
        };
        self.retries
            .spawn(provider.services.runtime.clone().as_ref(), async move {
                let result = this
                    .retry(&provider, &transport, &pending, started, failure, caller)
                    .await;
                pending.close();
                let mut state = this.state();
                if state.pending.get(&transport).is_some_and(|p| Arc::ptr_eq(p, &pending)) {
                    state.pending.shift_remove(&transport);
                }
                result
            });
        Ok(())
    }
    async fn retry(
        &self,
        provider: &AppleSimulatorProvider,
        transport: &Transport,
        pending: &PendingWorker,
        started: i64,
        mut last: ProvisioningError,
        caller: VendorContext,
    ) -> VendorResult<()> {
        let config = &provider.services.vendor.worker_connection;
        let worker = transport.id().to_string_lossy();
        let mut attempt = 1i32;
        loop {
            let Some(pause) = config.pause_after(attempt) else {
                provider.log(Level::Error, || format!("Failed to connect to worker {worker} after {attempt} attempts ({}), its devices stay out of the run", describe_failure(&last.failure())), None);
                return Ok(());
            };
            if attempt == config.attempts {
                provider.log(Level::Warn, || format!("Failed to connect to worker {worker} after {attempt} attempts ({}), retrying every {} s while the run is active", describe_failure(&last.failure()), pause.seconds()), None);
            }
            caller
                .clock
                .delay(duration_millis(pause)?, caller.cancellation.clone())
                .await?;
            if self.cutoff(provider, transport)? {
                return Ok(());
            }
            attempt = attempt.wrapping_add(1);
            provider.log(
                Level::Debug,
                || format!("Connecting to worker {worker}, attempt {attempt}"),
                None,
            );
            let acquired = async {
                let _permit = tokio::select! { biased;
                    reason = caller.cancellation.cancelled() => return Err(VendorError::Cancelled(reason).into()),
                    permit = self.permits.acquire() => permit.map_err(|e| other("java.lang.IllegalStateException", e.to_string()))?,
                };
                provider.services.connections.connect_worker(transport.clone(), caller.clone()).await
            }.await;
            let executor = match acquired {
                Ok(executor) => executor,
                Err(error) => {
                    let transient = is_transient_connection_failure(&error);
                    let error = ProvisioningError::from(error);
                    if error.is_cancelled() {
                        return Err(error.into_vendor());
                    }
                    if !transient {
                        provider.log(
                            Level::Error,
                            || format!("Failed to connect to worker {worker}, attempt {attempt}, not retrying"),
                            Some(error.output_failure()),
                        );
                        return Ok(());
                    }
                    provider.log(
                        Level::Debug,
                        || {
                            format!(
                                "Failed to connect to worker {worker}, attempt {attempt}: {}",
                                describe_failure(&error.failure())
                            )
                        },
                        None,
                    );
                    last = error;
                    continue;
                }
            };
            let result: VendorResult<bool> = async {
                let connected = provider.services.timing.elapsed()?;
                provider.log(Level::Warn, || format!("Connected to worker {worker} on attempt {attempt}, {} ms after the first one, setting up its devices", connected.wrapping_sub(started)), None);
                if self.cutoff(provider, transport)? { return Ok(false); }
                loop {
                    let Some(targets) = pending.take() else { return Ok(false); };
                    let targets: Vec<_> = targets.into_iter().filter(|target| !matches!(target, AppleTarget::Simulator { udid } if provider.state().removed.contains(&apple_id(udid, transport)))).collect();
                    if targets.is_empty() { continue; }
                    let unfinished = match provider.setup(transport, executor.clone(), &targets, started, SetupCycle::Late { attempt, connected }, caller.clone()).await {
                        Ok(unfinished) => unfinished,
                        Err(error) if error.is_cancelled() => return Err(error.into_vendor()),
                        Err(error) => {
                            provider.log(Level::Error, || format!("Failed to initialize worker {worker}"), Some(error.output_failure()));
                            self.schedule_setup_retry(provider, transport, targets, started)?;
                            return Ok(false);
                        }
                    };
                    self.schedule_setup_retry(provider, transport, unfinished.failed, started)?;
                    let missing = unfinished.missing;
                    if missing.is_empty() { continue; }
                    last = missing_failure(missing.len());
                    provider.log(Level::Warn, || format!("Worker {worker}: {} devices could not connect, retrying", missing.len()), None);
                    pending.offer(&missing);
                    return Ok(true);
                }
            }.await;
            let released = provider.services.connections.release(executor).await;
            // Kotlin's finally releases even a denied admission or a failed setup.
            let retry = finish_retry_attempt(result, released, |error| {
                provider.log(
                    Level::Error,
                    || format!("Failed to release connection to worker {worker}"),
                    Some(error.failure().clone()),
                );
            })?;
            if !retry {
                return Ok(());
            }
        }
    }
    async fn recover(
        &self,
        provider: &AppleSimulatorProvider,
        transport: &Transport,
        lost: &[Simulator],
        loss: Option<i32>,
        readded: bool,
        caller: VendorContext,
    ) -> VendorResult<()> {
        let result: ProvisioningResult<()> = async {
            if readded {
                provider.log(
                    Level::Warn,
                    || format!("Re-provisioning {}, added back to the devices file", lost[0].serial()),
                    None,
                );
                return provider
                    .provisioner
                    .existing([lost[0].udid().to_owned()], transport.clone(), caller)
                    .await;
            }
            let disposed = join_all(lost.iter().map(|d| d.dispose_and_join())).await;
            for result in disposed {
                result?;
            }
            let timeout = jcompat::jtime::duration_of(70, 0)
                .ok_or_else(|| other("java.lang.IllegalArgumentException", "Invalid session timeout"))?;
            let sessions = join_all(lost.iter().map(|d| d.await_session_closed(timeout, &caller))).await;
            for (device, result) in lost.iter().zip(sessions) {
                if !result? {
                    device.retire();
                }
            }
            if let Some(loss) = loss {
                if loss > self.max_recoveries {
                    provider.log(
                        Level::Warn,
                        || {
                            format!(
                                "Simulator {} has been lost {loss} times, not re-provisioning it again",
                                lost[0].serial()
                            )
                        },
                        None,
                    );
                    return Ok(());
                }
                caller
                    .clock
                    .delay(
                        self.recovery_pause_millis.wrapping_mul(i64::from(loss.wrapping_sub(1))),
                        caller.cancellation.clone(),
                    )
                    .await?;
            }
            let mut udids = Vec::new();
            for device in lost {
                if device.retired() || self.not_admitted(provider, device)? {
                    continue;
                }
                let id = apple_id(&device.udid().into(), transport);
                let removed = {
                    let mut state = provider.state();
                    let removed = state.removed.contains(&id);
                    if removed {
                        state.skipped_removed.insert(id);
                    }
                    removed
                };
                if removed {
                    continue;
                }
                provider.log(Level::Warn, || format!("Re-provisioning {}", device.serial()), None);
                udids.push(device.udid().to_owned());
            }
            provider.provisioner.existing(udids, transport.clone(), caller).await
        }
        .await;
        {
            let mut state = provider.state();
            state.recovering.retain(|d| !lost.iter().any(|old| Arc::ptr_eq(d, old)));
            // A90: worker-loss replacement predecessors are not retained by the source.
            if loss.is_some() || readded {
                for old in lost {
                    let id = apple_id(&old.udid().into(), transport);
                    if state
                        .devices
                        .get(&id)
                        .is_none_or(|current| !Arc::ptr_eq(&current.handle, old))
                    {
                        state.replaced.push(old.clone());
                    }
                }
            }
        }
        provider
            .isolate(result, || {
                if readded {
                    format!("Failed to re-provision {}", lost[0].serial())
                } else {
                    format!(
                        "Failed to re-provision simulators of {}",
                        transport_text(transport, &provider.local_identity)
                    )
                }
            })
            .map_err(ProvisioningError::into_vendor)
    }
}
fn duration_millis(duration: jcompat::jtime::Duration) -> VendorResult<i64> {
    duration
        .to_millis()
        .ok_or_else(|| other("java.lang.ArithmeticException", "long overflow"))
}
fn missing_failure(count: usize) -> ProvisioningError {
    other(
        "java.lang.IllegalStateException",
        format!("{count} devices could not connect"),
    )
    .into()
}
impl ProviderRecovery for SimulatorRecovery {
    fn attach(&self, provider: Weak<AppleSimulatorProvider>) {
        self.state().provider = provider;
    }
    fn connect_worker(
        &self,
        transport: Transport,
        connections: Arc<dyn ProvisioningConnections>,
        caller: VendorContext,
    ) -> ConnectionFuture<'_, Executor> {
        Box::pin(async move { connections.connect_worker(transport, caller).await })
    }
    fn offer(&self, transport: &Transport, targets: &[AppleTarget]) -> bool {
        let offered = self.state().pending.get(transport).is_some_and(|p| p.offer(targets));
        if offered && let Ok(provider) = self.provider() {
            provider.log(
                Level::Info,
                || {
                    format!(
                        "Worker {} is not connected yet, its {} new devices will be set up once it connects",
                        transport.id().to_string_lossy(),
                        targets.len()
                    )
                },
                None,
            );
        }
        offered
    }
    fn connect_failed(
        &self,
        transport: Transport,
        targets: Vec<AppleTarget>,
        started: i64,
        error: ProvisioningError,
        _: VendorContext,
    ) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let provider = self.provider()?;
            let transient = matches!(&error.original, SetupError::Connection(e) if is_transient_connection_failure(e));
            if !transient || provider.services.vendor.worker_connection.pause_after(1).is_none() {
                provider.log(
                    Level::Error,
                    || {
                        format!(
                            "Failed to initialize connection to worker {}",
                            transport.id().to_string_lossy()
                        )
                    },
                    Some(error.output_failure()),
                );
                return Ok(());
            }
            provider.log(
                Level::Warn,
                || {
                    format!(
                        "Failed to connect to worker {} ({}), retrying",
                        transport.id().to_string_lossy(),
                        describe_failure(&error.failure())
                    )
                },
                None,
            );
            self.retry_later(transport, targets, started, error)
        })
    }
    fn missing(
        &self,
        transport: Transport,
        targets: Vec<AppleTarget>,
        started: i64,
        _: VendorContext,
    ) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let provider = self.provider()?;
            let retry = provider.services.vendor.worker_connection.pause_after(1).is_some();
            provider.log(
                if retry { Level::Warn } else { Level::Error },
                || {
                    format!(
                        "Worker {}: {} devices could not connect, {}",
                        transport.id().to_string_lossy(),
                        targets.len(),
                        if retry { "retrying" } else { "they stay out of the run" }
                    )
                },
                None,
            );
            if retry {
                let error = missing_failure(targets.len());
                self.retry_later(transport, targets, started, error)?;
            }
            Ok(())
        })
    }
    fn setup_failed(&self, transport: Transport, targets: Vec<AppleTarget>, started: i64) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let provider = self.provider()?;
            self.schedule_setup_retry(&provider, &transport, targets, started)
        })
    }
    fn forget_setup(&self, id: &JavaString) {
        self.state().setup.forget(id);
    }
    fn admit(&self, id: &JavaString, device: &Simulator) -> ProvisioningResult<()> {
        let provider = self.provider()?;
        // D33: a retried simulator joins as late as one of a late worker, under the same cut-off.
        let late = {
            let state = self.state();
            state.pending.contains_key(device.transport()) || state.setup.running(device.transport())
        } && !provider.state().devices.contains_key(id);
        if late || self.state().awaiting_admission.contains(id) {
            if let Some(reason) = self.late_join_cutoff()? {
                if let Some(previous) = provider.state().devices.get(id) {
                    previous.handle.retire();
                }
                return Err(SetupDropped(
                    format!(
                        "Simulator {} doesn't join the run late, since {reason}",
                        device.serial()
                    )
                    .into(),
                )
                .into());
            }
            self.state().awaiting_admission.insert(id.clone());
            let this = self.me.clone();
            let id = id.clone();
            let serial = device.serial().to_owned();
            device.set_admission(Some(Arc::new(move || {
                let this = this.upgrade().ok_or_else(|| {
                    other(
                        "java.lang.IllegalStateException",
                        "Recovery allocation has been released",
                    )
                })?;
                let reason = this.late_join_cutoff()?;
                if let Some(reason) = &reason {
                    let provider = this.provider()?;
                    provider.log(
                        Level::Info,
                        || format!("Simulator {serial} doesn't join the run late, since {reason}"),
                        None,
                    );
                } else {
                    this.state().awaiting_admission.shift_remove(&id);
                }
                Ok(reason)
            })));
        }
        Ok(())
    }
    fn reconnect(&self, provider: Arc<AppleSimulatorProvider>, caller: VendorContext) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let mut disconnected = Vec::new();
            let mut failed = Vec::new();
            let mut readded = Vec::new();
            for device in provider.devices() {
                let d = device.handle;
                let id = apple_id(&d.udid().into(), d.transport());
                let retired = d.retired();
                let disposed = d.disposed();
                {
                    let state = provider.state();
                    if state.recovering.iter().any(|old| Arc::ptr_eq(old, &d))
                        || state.exhausted.contains(&id)
                        || retired
                        || state.removed.contains(&id) && disposed
                    {
                        continue;
                    }
                }
                // An executor's connected check can suspend its caller in the source;
                // registration/removal must remain available while that check runs.
                let connected = d.connected();
                let lost = d.lost();
                let mut state = provider.state();
                if state.recovering.iter().any(|old| Arc::ptr_eq(old, &d)) {
                    continue;
                }
                if !connected {
                    disconnected.push(d.clone());
                } else if lost {
                    failed.push(d.clone());
                } else if disposed && state.skipped_removed.shift_remove(&id) {
                    readded.push(d.clone());
                } else {
                    continue;
                }
                state.recovering.push(d);
            }
            // Every allocation is marked before any shared-channel disconnect is published.
            for d in disconnected.iter().chain(&failed) {
                provider
                    .channel
                    .try_send(DeviceEvent::Disconnected(marathon_core::vendor::DeviceRef(d.clone())))
                    .map_err(|e| {
                        VendorError::Other(e.cause.unwrap_or_else(|| {
                            failure(
                                "kotlinx.coroutines.channels.ClosedSendChannelException",
                                "Channel was closed",
                            )
                        }))
                    })?;
            }
            let mut workers = IndexMap::<Transport, Vec<Simulator>>::new();
            for device in disconnected {
                workers.entry(device.transport().clone()).or_default().push(device);
            }
            let mut groups: Vec<_> = workers.into_iter().map(|(t, d)| (t, d, None, false)).collect();
            for d in failed {
                let id = apple_id(&d.udid().into(), d.transport());
                let loss = {
                    let mut state = self.state();
                    let count = state.losses.entry(id.clone()).or_default();
                    *count = count.wrapping_add(1);
                    *count
                };
                provider.set_exhausted(id, loss > self.max_recoveries);
                groups.push((d.transport().clone(), vec![d], Some(loss), false));
            }
            groups.extend(
                readded
                    .into_iter()
                    .map(|d| (d.transport().clone(), vec![d], None, true)),
            );
            for (transport, lost, loss, readded) in groups {
                let this = self.me.upgrade().ok_or_else(|| {
                    other(
                        "java.lang.IllegalStateException",
                        "Recovery allocation has been released",
                    )
                })?;
                let provider = provider.clone();
                let caller = VendorContext {
                    cancellation: Arc::new(LinkedSignal {
                        parent: caller.cancellation.clone(),
                        local: self.recover_cancel.clone(),
                    }),
                    ..caller.clone()
                };
                self.recoveries
                    .spawn(provider.services.runtime.clone().as_ref(), async move {
                        this.recover(&provider, &transport, &lost, loss, readded, caller).await
                    });
            }
            Ok(())
        })
    }
    fn terminate(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.recover_cancel.cancel(requested("Provider terminated"));
            for task in self.recoveries.take() {
                let _ = task.join().await;
            }
            self.retry_cancel.cancel(requested("Provider terminated"));
            for task in self.retries.take() {
                let _ = task.join().await;
            }
            Ok(())
        })
    }
}

// Preserve the original failure when cleanup also fails.
fn finish_retry_attempt(
    result: VendorResult<bool>,
    released: VendorResult<()>,
    log_release_error: impl FnOnce(&VendorError),
) -> VendorResult<bool> {
    match result {
        Err(error) => {
            if let Err(release_error) = released {
                log_release_error(&release_error);
            }
            Err(error)
        }
        Ok(retry) => released.map(|()| retry),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_preserves_original_error_when_release_fails() {
        let release_error = other("java.io.IOException", "release failed");
        for original in [
            other("java.io.IOException", "setup failed"),
            VendorError::Cancelled(requested("Provider terminated")),
        ] {
            let mut logged = None;
            let result = finish_retry_attempt(Err(original.clone()), Err(release_error.clone()), |error| {
                logged = Some(error.clone());
            });
            assert_eq!(result, Err(original));
            assert_eq!(logged, Some(release_error.clone()));
        }
    }

    #[test]
    fn retry_propagates_release_error_after_success() {
        let error = other("java.io.IOException", "release failed");
        assert_eq!(
            finish_retry_attempt(Ok(true), Err(error.clone()), |_| panic!("unexpected log")),
            Err(error)
        );
    }
}
