//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt:372,429–440
//! D33: Kotlin leaves a simulator whose setup failed tracked as connected, so nothing sets it up again (A81).
//! The retry is timed rather than driven by snapshots, since a static devices file only ever has one.

use super::*;
use crate::device::device_tracker::target_id;

#[derive(Clone)]
struct Waiting {
    target: AppleTarget,
    key: JavaString,
    generation: u64,
}
#[derive(Default)]
pub(super) struct SetupRetries {
    /// Failed rounds by target, for the run. A failure of a retry and one of a late worker's setup count alike, so a
    /// hand-over between this retry and the worker retry can't refill the budget. All instances of a profile share
    /// one key: instances that fail in the same cycle count once, and are retried together.
    failures: IndexMap<JavaString, i32>,
    /// Bumped by each removal, so that a retry from before it can't act on the simulator after it was added back.
    generations: IndexMap<JavaString, u64>,
    due: IndexMap<Transport, Vec<Waiting>>,
    running: IndexSet<Transport>,
}
impl SetupRetries {
    pub(super) fn running(&self, transport: &Transport) -> bool {
        self.running.contains(transport)
    }
    /// A removed simulator starts over if it is added back: the devices file changed, and that is bounded by its owner.
    pub(super) fn forget(&mut self, id: &JavaString) {
        self.failures.shift_remove(id);
        *self.generations.entry(id.clone()).or_default() += 1;
        for due in self.due.values_mut() {
            due.retain(|waiting| waiting.key != *id);
        }
    }
    fn generation(&self, key: &JavaString) -> u64 {
        self.generations.get(key).copied().unwrap_or(0)
    }
}
fn label(target: &AppleTarget) -> String {
    match target {
        AppleTarget::SimulatorProfile(profile) => profile_text(profile),
        target => target_id(target).to_string_lossy(),
    }
}
/// Whether this target still needs setting up. A registered simulator belongs to its recovery unless it is disposed
/// and free to be set up again, and one that left the devices file is gone.
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
        if targets.is_empty() || provider.services.vendor.worker_connection.pause_after(1).is_none() {
            return Ok(());
        }
        let waiting = {
            let state = self.state();
            targets
                .into_iter()
                .map(|target| {
                    let key = apple_id(&target_id(&target), transport);
                    let generation = state.setup.generation(&key);
                    Waiting {
                        target,
                        key,
                        generation,
                    }
                })
                .collect()
        };
        let waiting = wanted(provider, transport, waiting)?;
        let mut rounds = IndexMap::<i32, Vec<Waiting>>::new();
        let mut counted = IndexMap::<JavaString, i32>::new();
        let mut exhausted = Vec::new();
        {
            let mut state = self.state();
            if self.retry_cancel.reason().is_some() {
                return Ok(());
            }
            for waiting in waiting {
                let round = match counted.get(&waiting.key) {
                    Some(round) => *round,
                    None => {
                        let failures = state.setup.failures.entry(waiting.key.clone()).or_default();
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
        }
        let worker = transport.id().to_string_lossy();
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
        {
            let mut state = self.state();
            let batch: Vec<_> = batch
                .into_iter()
                .filter(|waiting| waiting.generation == state.setup.generation(&waiting.key))
                .collect();
            state.setup.due.entry(transport.clone()).or_default().extend(batch);
            if !state.setup.running.insert(transport.clone()) {
                return Ok(());
            }
        }
        let result = self.drain_setup_retries(provider, transport, started, &caller).await;
        if result.is_err() {
            self.state().setup.running.shift_remove(transport);
        }
        result
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
            if self.cutoff(provider, transport)? {
                continue;
            }
            self.setup_retry_attempt(provider, transport, due, started, caller)
                .await?;
        }
    }
    async fn setup_retry_attempt(
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
                let targets = due.into_iter().map(|waiting| waiting.target).collect();
                return self
                    .connect_failed(transport.clone(), targets, started, error, caller.clone())
                    .await;
            }
        };
        // A removal during the connection takes the target out before anything is planned for it.
        let due = match wanted(provider, transport, due) {
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
            provider
                .setup(
                    transport,
                    executor.clone(),
                    &targets,
                    started,
                    SetupCycle::Retried,
                    caller.clone(),
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
        if !unfinished.missing.is_empty() {
            self.missing(transport.clone(), unfinished.missing, started, caller.clone())
                .await?;
        }
        // A target removed during the attempt lost its budget and generation with it, whatever it was added back as.
        let failed = {
            let state = self.state();
            unfinished
                .failed
                .into_iter()
                .filter(|target| {
                    let key = apple_id(&target_id(target), transport);
                    due.iter()
                        .any(|waiting| waiting.key == key && waiting.generation == state.setup.generation(&key))
                })
                .collect()
        };
        self.schedule_setup_retry(provider, transport, failed, started)
    }
}
