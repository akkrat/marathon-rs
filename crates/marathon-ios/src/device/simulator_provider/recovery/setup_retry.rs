//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt:372,429–440
//! D33: Kotlin leaves a simulator whose setup failed tracked as connected, so nothing sets it up again (A81).
//! The retry is timed rather than driven by snapshots, since a static devices file only ever has one.

use super::*;
use crate::device::device_tracker::target_id;

tokio::task_local! {
    /// Marks the task of a retry attempt. Registration runs in that task, so this tells a retried simulator from a
    /// snapshot's on the same worker. Ids couldn't: a profile's simulator has none until the attempt creates it.
    static RETRIED_SETUP: ();
}
pub(super) fn in_retried_setup() -> bool {
    RETRIED_SETUP.try_with(|_| ()).is_ok()
}
#[derive(Clone)]
struct Waiting {
    target: AppleTarget,
    key: JavaString,
    generation: u64,
}
/// The count of failed rounds is the provider's `setup_failures`, next to the generations that guard it (D44).
#[derive(Default)]
pub(super) struct SetupRetries {
    due: IndexMap<Transport, Vec<Waiting>>,
    running: IndexSet<Transport>,
}
impl SetupRetries {
    /// Ends a worker's runner unless a batch arrived that nobody else will take. The batch saw the runner running,
    /// and this is under the lock it added itself under.
    fn stop_runner(&mut self, transport: &Transport) -> bool {
        if self.due.get(transport).is_some_and(|due| !due.is_empty()) {
            return false;
        }
        self.running.shift_remove(transport);
        true
    }
    /// A removed simulator starts over if it is added back: the devices file changed, and that is bounded by its owner.
    /// The removal also cleared its count and moved its generation on.
    pub(super) fn forget(&mut self, id: &JavaString) {
        for due in self.due.values_mut() {
            due.retain(|waiting| waiting.key != *id);
        }
    }
}
/// D44: what a removal hasn't taken from a retry since it was scheduled.
fn current_generation(provider: &AppleSimulatorProvider, due: Vec<Waiting>) -> Vec<Waiting> {
    due.into_iter()
        .filter(|waiting| waiting.generation == provider.generation(&waiting.key))
        .collect()
}
fn label(target: &AppleTarget) -> String {
    match target {
        AppleTarget::SimulatorProfile(profile) => profile_text(profile),
        target => target_id(target).to_string_lossy(),
    }
}
/// Whether this target still needs setting up. A registered simulator belongs to its recovery unless it is free to be
/// set up again, or its recovery failed and handed it over (D44), and one that left the devices file is gone.
fn wanted(
    provider: &AppleSimulatorProvider,
    transport: &Transport,
    targets: Vec<Waiting>,
) -> VendorResult<Vec<Waiting>> {
    let current = provider.current(transport).map_err(ProvisioningError::into_vendor)?;
    let state = provider.state();
    Ok(targets
        .into_iter()
        .filter(|waiting| match &waiting.target {
            AppleTarget::Simulator { udid } => {
                !state.removed.contains(&waiting.key)
                    && (!state.devices.contains_key(&waiting.key)
                        || current.eligible_disposed.contains(&udid.to_string_lossy()))
            }
            AppleTarget::SimulatorProfile(_) => true,
            AppleTarget::Physical { .. } | AppleTarget::Host => false,
        })
        .collect())
}
impl SimulatorRecovery {
    /// Counts one failed round per target and starts its pause. After the last round the target is left as Kotlin
    /// leaves it: tracked, out of the run, until a snapshot removes it and adds it back.
    pub(super) fn schedule_setup_retry(
        &self,
        provider: &AppleSimulatorProvider,
        transport: &Transport,
        targets: Vec<AppleTarget>,
        started: i64,
    ) -> VendorResult<()> {
        // With attempts: 1 and lateJoin: false the owner asked for no retries of a worker's devices at all.
        if targets.is_empty() {
            return Ok(());
        }
        if provider.services.vendor.worker_connection.pause_after(1).is_none() {
            provider.give_up_targets(transport, &targets, ParkedFor::GivenUp);
            return Ok(());
        }
        // D44: the retry acts for what the failing operation claimed, at its generation.
        let claims = provider.claim(targets.iter().map(|target| apple_id(&target_id(target), transport)));
        let waiting = targets
            .into_iter()
            .map(|target| {
                let key = apple_id(&target_id(&target), transport);
                let generation = claims.0.get(&key).copied().unwrap_or(0);
                Waiting {
                    target,
                    key,
                    generation,
                }
            })
            .collect();
        let waiting: Vec<Waiting> = wanted(provider, transport, waiting)?;
        if self.retry_cancel.reason().is_some() {
            return Ok(());
        }
        let mut rounds = IndexMap::<i32, Vec<Waiting>>::new();
        let mut exhausted = Vec::new();
        let keys: Vec<_> = waiting.iter().map(|waiting| waiting.key.clone()).collect();
        provider.settle(&keys, |state, current| {
            let mut counted = IndexMap::<JavaString, i32>::new();
            for waiting in waiting.into_iter().filter(|waiting| current.contains(&waiting.key)) {
                let round = match counted.get(&waiting.key) {
                    Some(round) => *round,
                    None => {
                        let failures = state.setup_failures.entry(waiting.key.clone()).or_default();
                        *failures = failures.wrapping_add(1);
                        counted.insert(waiting.key.clone(), *failures);
                        if *failures > self.max_recoveries {
                            exhausted.push((waiting.target.clone(), *failures));
                        }
                        *failures
                    }
                };
                if round <= self.max_recoveries {
                    rounds.entry(round).or_default().push(waiting);
                }
            }
        });
        let worker = transport.id().to_string_lossy();
        let spent: Vec<_> = exhausted.iter().map(|(target, _)| target.clone()).collect();
        provider.give_up_targets(transport, &spent, ParkedFor::GivenUp);
        for (target, failures) in exhausted {
            provider.log(
                Level::Error,
                || {
                    format!(
                        "Simulator {} on worker {worker}: setup failed {failures} times, it stays out of the run",
                        label(&target)
                    )
                },
                None,
            );
        }
        let Some(this) = self.me.upgrade() else {
            return Err(other(
                "java.lang.IllegalStateException",
                "Recovery allocation has been released",
            ));
        };
        for (round, batch) in rounds {
            let pause = self.recovery_pause_millis.wrapping_mul(i64::from(round));
            provider.log(
                Level::Warn,
                || {
                    format!(
                        "Worker {worker}: setup of {} failed, setting up again in {} s (retry {round} of {})",
                        batch
                            .iter()
                            .map(|waiting| label(&waiting.target))
                            .collect::<Vec<_>>()
                            .join(", "),
                        pause / 1000,
                        self.max_recoveries
                    )
                },
                None,
            );
            let caller = VendorContext {
                cancellation: self.retry_cancel.clone(),
                ..provider.services.context.clone()
            };
            let this = this.clone();
            let provider = provider.this()?;
            let transport = transport.clone();
            self.retries
                .spawn(provider.services.runtime.clone().as_ref(), async move {
                    caller.clock.delay(pause, caller.cancellation.clone()).await?;
                    this.setup_retry_due(&provider, &transport, batch, started, caller)
                        .await
                });
        }
        Ok(())
    }
    /// One runner per worker takes what is due, so a worker has at most one retry attempt at a time however many
    /// simulators are waiting, and other workers don't wait for it.
    async fn setup_retry_due(
        &self,
        provider: &Arc<AppleSimulatorProvider>,
        transport: &Transport,
        batch: Vec<Waiting>,
        started: i64,
        caller: VendorContext,
    ) -> VendorResult<()> {
        let batch = current_generation(provider, batch);
        {
            let mut state = self.state();
            state.setup.due.entry(transport.clone()).or_default().extend(batch);
            if !state.setup.running.insert(transport.clone()) {
                return Ok(());
            }
        }
        loop {
            let error = match self.drain_setup_retries(provider, transport, started, &caller).await {
                Ok(()) => return Ok(()),
                Err(error) => error,
            };
            // Each pass takes what is due, so an error that repeats still ends this.
            let stopped = {
                let mut state = self.state();
                if matches!(error, VendorError::Cancelled(_)) {
                    state.setup.running.shift_remove(transport);
                    true
                } else {
                    state.setup.stop_runner(transport)
                }
            };
            if stopped {
                return Err(error);
            }
            provider.log(
                Level::Error,
                || format!("Failed to retry setups on worker {}", transport.id().to_string_lossy()),
                Some(error.failure().clone()),
            );
        }
    }
    async fn drain_setup_retries(
        &self,
        provider: &Arc<AppleSimulatorProvider>,
        transport: &Transport,
        started: i64,
        caller: &VendorContext,
    ) -> VendorResult<()> {
        loop {
            let due = {
                let mut state = self.state();
                let due = state.setup.due.shift_remove(transport).unwrap_or_default();
                if due.is_empty() {
                    // Under the lock that adds to `due`, so a later batch sees that nobody runs and takes over.
                    state.setup.running.shift_remove(transport);
                    return Ok(());
                }
                due
            };
            let due = wanted(provider, transport, due)?;
            if due.is_empty() {
                continue;
            }
            let due = self.not_past_cutoff(provider, transport, due)?;
            if due.is_empty() {
                continue;
            }
            self.setup_retry_attempt(provider, transport, due, started, caller)
                .await?;
        }
    }
    /// D44: a simulator returning to the run isn't late, and this retry's budget bounds it anyway.
    fn not_past_cutoff(
        &self,
        provider: &AppleSimulatorProvider,
        transport: &Transport,
        due: Vec<Waiting>,
    ) -> VendorResult<Vec<Waiting>> {
        let Some(reason) = self.late_join_cutoff()? else {
            return Ok(due);
        };
        let returning = self.returning(provider, transport);
        let total = due.len();
        let due: Vec<_> = due.into_iter().filter(|w| returning.contains_key(&w.key)).collect();
        self.log_late(provider, transport, &reason, total - due.len(), !due.is_empty());
        Ok(due)
    }
    /// D44: an attempt acts for its targets at the generations they were scheduled with.
    async fn setup_retry_attempt(
        &self,
        provider: &Arc<AppleSimulatorProvider>,
        transport: &Transport,
        due: Vec<Waiting>,
        started: i64,
        caller: &VendorContext,
    ) -> VendorResult<()> {
        let claims = Claims(due.iter().map(|w| (w.key.clone(), w.generation)).collect());
        with_claims(
            claims,
            self.setup_retry_claimed(provider, transport, due, started, caller),
        )
        .await
    }
    async fn setup_retry_claimed(
        &self,
        provider: &Arc<AppleSimulatorProvider>,
        transport: &Transport,
        due: Vec<Waiting>,
        started: i64,
        caller: &VendorContext,
    ) -> VendorResult<()> {
        let worker = transport.id().to_string_lossy();
        let acquired = async {
            let _permit = tokio::select! { biased;
                reason = caller.cancellation.cancelled() => return Err(VendorError::Cancelled(reason).into()),
                permit = self.permits.acquire() => permit.map_err(|e| other("java.lang.IllegalStateException", e.to_string()))?,
            };
            provider.services.connections.connect_worker(transport.clone(), caller.clone()).await
        }
        .await;
        let executor = match acquired {
            Ok(executor) => executor,
            Err(error) => {
                let error = ProvisioningError::from(error);
                if error.is_cancelled() {
                    return Err(error.into_vendor());
                }
                // The worker is what failed now: its own retry policy decides, as for a first connection.
                // D44: only for what this retry still owns; one removed and added back meanwhile has a retry of its own.
                let targets: Vec<_> = current_generation(provider, due)
                    .into_iter()
                    .map(|waiting| waiting.target)
                    .collect();
                if targets.is_empty() {
                    return Ok(());
                }
                return self
                    .connect_failed(transport.clone(), targets, started, error, caller.clone())
                    .await;
            }
        };
        // A removal during the connection takes the target out before anything is planned for it.
        let due = match wanted(provider, transport, current_generation(provider, due)) {
            Ok(due) => due,
            Err(error) => {
                let _ = provider.services.connections.release(executor).await;
                return Err(error);
            }
        };
        let targets: Vec<AppleTarget> = due.iter().map(|waiting| waiting.target.clone()).collect();
        let result = if targets.is_empty() {
            Ok(UnfinishedSetup::default())
        } else {
            RETRIED_SETUP
                .scope(
                    (),
                    provider.setup(
                        transport,
                        executor.clone(),
                        &targets,
                        started,
                        SetupCycle::Retried,
                        caller.clone(),
                    ),
                )
                .await
        };
        // A failed release costs this attempt's lease, not the retries of what it didn't set up.
        if let Err(error) = provider.services.connections.release(executor).await {
            provider.log(
                Level::Error,
                || format!("Failed to release connection to worker {worker}"),
                Some(error.failure().clone()),
            );
        }
        let unfinished = match result {
            Ok(unfinished) => unfinished,
            Err(error) if error.is_cancelled() => return Err(error.into_vendor()),
            Err(error) => {
                provider.log(
                    Level::Error,
                    || format!("Failed to initialize worker {worker}"),
                    Some(error.output_failure()),
                );
                UnfinishedSetup {
                    missing: Vec::new(),
                    failed: targets,
                }
            }
        };
        // A target removed during the attempt lost its budget and generation with it, whatever it was added back as.
        let owned = |targets: Vec<AppleTarget>| -> Vec<AppleTarget> {
            targets
                .into_iter()
                .filter(|target| {
                    let key = apple_id(&target_id(target), transport);
                    due.iter()
                        .any(|waiting| waiting.key == key && waiting.generation == provider.generation(&key))
                })
                .collect()
        };
        let missing = owned(unfinished.missing);
        if !missing.is_empty() {
            self.missing(transport.clone(), missing, started, caller.clone())
                .await?;
        }
        let failed = owned(unfinished.failed);
        self.schedule_setup_retry(provider, transport, failed, started)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runner_stopping_on_an_error_leaves_running_if_a_batch_arrived_meanwhile() {
        let transport = Transport::Local;
        let mut retries = SetupRetries::default();
        retries.running.insert(transport.clone());
        retries.due.entry(transport.clone()).or_default().push(Waiting {
            target: AppleTarget::Simulator { udid: "a".into() },
            key: "a@local".into(),
            generation: 0,
        });
        assert!(!retries.stop_runner(&transport));
        assert!(retries.running.contains(&transport));
        retries.due.shift_remove(&transport);
        assert!(retries.stop_runner(&transport));
        assert!(!retries.running.contains(&transport));
    }
}
