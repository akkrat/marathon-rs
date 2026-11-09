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
    pending: IndexMap<Transport, Arc<PendingWorker<Entry>>>,
    losses: IndexMap<String, i32>,
    awaiting_admission: IndexSet<String>,
    closed: Option<String>,
    setup: setup_retry::SetupRetries,
}
/// D44: a target waiting for its worker, with the generation of the claim that handed it over. An attempt takes only
/// what matches its own claims; an entry whose simulator was removed or added back since it was offered is dead.
#[derive(Clone)]
struct Entry {
    target: AppleTarget,
    generation: u64,
}
fn entries(provider: &AppleSimulatorProvider, transport: &Transport, targets: &[AppleTarget]) -> Vec<Entry> {
    let claims = provider.claim(targets.iter().map(|t| target_key(t, transport)));
    targets
        .iter()
        .map(|target| Entry {
            target: target.clone(),
            generation: claims.0.get(&target_key(target, transport)).copied().unwrap_or(0),
        })
        .collect()
}
/// D44: how a retry's entries stand against the claims of `held`, the attempt or the retry that is ending.
enum Standing {
    /// Removed from the devices file, or removed or added back since it was offered.
    Dead,
    /// Held at its generation, or not held at all.
    Own,
    /// Offered after a re-add that `held` predates.
    Newer,
}
fn standing(provider: &AppleSimulatorProvider, transport: &Transport, entry: &Entry, held: &Claims) -> Standing {
    let key = target_key(&entry.target, transport);
    let removed = matches!(entry.target, AppleTarget::Simulator { .. }) && provider.state().removed.contains(&key);
    if removed || entry.generation != provider.generation(&key) {
        return Standing::Dead;
    }
    match held.0.get(&key) {
        Some(generation) if *generation != entry.generation => Standing::Newer,
        _ => Standing::Own,
    }
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
    /// D44: the simulators of this worker that have been in the run and are waiting to return, with when they started
    /// to wait. They aren't late, so the late-join cut-off is not theirs: a lost worker mustn't lose its simulators for
    /// good because the run had got far enough. One still waiting for its admission hasn't joined yet.
    fn returning(&self, provider: &AppleSimulatorProvider, transport: &Transport) -> IndexMap<String, ParkedOn> {
        let mut returning = provider.parked_on(transport);
        let state = self.state();
        returning.retain(|id, _| !state.awaiting_admission.contains(id));
        returning
    }
    fn log_late(
        &self,
        provider: &AppleSimulatorProvider,
        transport: &Transport,
        reason: &str,
        late: usize,
        others_left: bool,
    ) {
        let worker = transport.id().to_owned();
        if others_left {
            if late > 0 {
                provider.log(
                    Level::Info,
                    || format!("Worker {worker}: {late} devices don't join the run late, since {reason}"),
                    None,
                );
            }
        } else {
            provider.log(
                Level::Info,
                || format!("Worker {worker} doesn't join the run late, since {reason}. No longer connecting to it"),
                None,
            );
        }
    }
    /// What the worker retry still works for. True when nothing is left, which ends it.
    ///
    /// D44: a returning simulator waits for its worker for `lateJoinCutoff`, counted from when it started to wait. Zero
    /// means no cut-off here too, so `lateJoin` and the end of the run are the limits then.
    fn prune_pending(
        &self,
        provider: &AppleSimulatorProvider,
        transport: &Transport,
        pending: &PendingWorker<Entry>,
    ) -> VendorResult<bool> {
        let closed = self.late_join_cutoff()?;
        let returning = self.returning(provider, transport);
        // Read only when it matters: a run with no lost simulator reads its clocks as it always has.
        let now = match returning.is_empty() {
            true => 0,
            false => provider.services.timing.elapsed()?,
        };
        let cutoff = provider.services.vendor.worker_connection.late_join_cutoff;
        let limit = match cutoff.is_zero() {
            true => None,
            false => Some(duration_millis(cutoff)?),
        };
        let mut late = 0;
        let mut expired = Vec::new();
        let left = pending.retain(|entry| {
            let target = &entry.target;
            let parked = match target {
                AppleTarget::Simulator { udid } => {
                    let id = apple_id(udid, transport);
                    returning.get(&id).map(|parked| (id, parked))
                }
                _ => None,
            };
            match parked {
                // It left the devices file. If it comes back, that update starts a retry again.
                Some((_, parked)) if parked.removed => false,
                Some((id, parked)) if limit.is_some_and(|limit| now.wrapping_sub(parked.since) > limit) => {
                    expired.push(id);
                    false
                }
                Some(_) => true,
                None if closed.is_some() => {
                    late += 1;
                    false
                }
                None => true,
            }
        });
        for id in &expired {
            if let Some((udid, _)) = id.to_owned().rsplit_once('@') {
                provider.give_up_parked(transport, &[udid], ParkedFor::GivenUp);
            }
            provider.log(
                Level::Error,
                || {
                    format!(
                        "Simulator {} has no connection {} s after it lost its worker, it stays out of the run",
                        id.to_owned(),
                        cutoff.seconds()
                    )
                },
                None,
            );
        }
        if let Some(reason) = &closed
            && (late > 0 || left)
        {
            self.log_late(provider, transport, reason, late, left);
        }
        Ok(!left)
    }
    fn not_admitted(&self, provider: &AppleSimulatorProvider, device: &Simulator) -> VendorResult<bool> {
        let id = apple_id(device.udid(), device.transport());
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
        // D44: a hand-over is a decision too, so stale work hands over nothing.
        let targets = provider.still_claimed(&transport, targets);
        if targets.is_empty() {
            return Ok(());
        }
        let offered = entries(&provider, &transport, &targets);
        let pending = {
            let mut state = self.state();
            if self.retry_cancel.reason().is_some() {
                return Ok(());
            }
            if state.pending.get(&transport).is_some_and(|p| p.offer(&offered)) {
                return Ok(());
            }
            let pending = Arc::new(PendingWorker::new(offered));
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
        let mut claims = provider.claim(targets.iter().map(|t| target_key(t, &transport)));
        self.retries
            .spawn(provider.services.runtime.clone().as_ref(), async move {
                let result = this
                    .retry(&provider, &transport, &pending, started, failure, caller, &mut claims)
                    .await;
                pending.close();
                let mut returning = Vec::new();
                if result.is_ok()
                    && let Some(left) = pending.take()
                {
                    // D44: what the attempt that ended it acted for has been logged as out of the run, so no sibling's
                    // plan takes it up. One offered after a re-add that attempt predates, or a returning simulator that
                    // joined after it started, gets a retry of its own. A dead entry is nobody's.
                    let mut covered = Vec::new();
                    for entry in left {
                        let key = target_key(&entry.target, &transport);
                        match standing(&provider, &transport, &entry, &claims) {
                            Standing::Dead => {}
                            Standing::Newer => returning.push(entry.target),
                            Standing::Own if !claims.0.contains_key(&key) && provider.parked_for_retry(&key) => {
                                returning.push(entry.target)
                            }
                            Standing::Own => covered.push(entry.target),
                        }
                    }
                    with_claims_sync(claims, || {
                        provider.give_up_targets(&transport, &covered, ParkedFor::GivenUp)
                    });
                    let mut seen = IndexSet::new();
                    returning.retain(|target| seen.insert(target_key(target, &transport)));
                }
                {
                    let mut state = this.state();
                    if state.pending.get(&transport).is_some_and(|p| Arc::ptr_eq(p, &pending)) {
                        state.pending.shift_remove(&transport);
                    }
                }
                if !returning.is_empty() {
                    let claims = provider.claim_now(returning.iter().map(|t| target_key(t, &transport)));
                    let failure = missing_failure(returning.len());
                    with_claims_sync(claims, || this.retry_later(transport, returning, started, failure))?;
                }
                result
            });
        Ok(())
    }
    /// `claims` ends as those of the last attempt, which decide for what is left when the retry ends.
    #[allow(clippy::too_many_arguments)]
    async fn retry(
        &self,
        provider: &AppleSimulatorProvider,
        transport: &Transport,
        pending: &PendingWorker<Entry>,
        started: i64,
        mut last: ProvisioningError,
        caller: VendorContext,
        claims: &mut Claims,
    ) -> VendorResult<()> {
        let config = &provider.services.vendor.worker_connection;
        let worker = transport.id().to_owned();
        let mut attempt = 1i32;
        let keys = |pending: &PendingWorker<Entry>| -> Vec<String> {
            pending
                .peek()
                .iter()
                .map(|e| target_key(&e.target, transport))
                .collect()
        };
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
            // D44: what the pause ends decides for what waits now; the attempt acts for what waits when it starts.
            let now = provider.claim_now(keys(pending));
            if with_claims_sync(now, || self.prune_pending(provider, transport, pending))? {
                return Ok(());
            }
            *claims = provider.claim_now(keys(pending));
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
            let result: VendorResult<bool> = with_claims(claims.clone(), async {
                let connected = provider.services.timing.elapsed()?;
                provider.log(Level::Warn, || format!("Connected to worker {worker} on attempt {attempt}, {} ms after the first one, setting up its devices", connected.wrapping_sub(started)), None);
                if self.prune_pending(provider, transport, pending)? { return Ok(false); }
                loop {
                    let Some(taken) = pending.take() else { return Ok(false); };
                    // D44: an entry offered after a re-add this attempt predates stays for the retry's next attempt,
                    // which claims it as it is then. A dead one goes.
                    let held = provider.claim([]);
                    let (mut targets, mut newer) = (Vec::new(), Vec::new());
                    for entry in taken {
                        match standing(provider, transport, &entry, &held) {
                            Standing::Dead => {}
                            Standing::Own => targets.push(entry.target),
                            Standing::Newer => newer.push(entry),
                        }
                    }
                    if targets.is_empty() {
                        if newer.is_empty() { continue; }
                        pending.offer(&newer);
                        return Ok(true);
                    }
                    // D44: what joined after the attempt started is claimed as it is now.
                    let taken = provider.claim(targets.iter().map(|t| target_key(t, transport)));
                    let step = with_claims(taken, async {
                        let unfinished = match provider.setup(transport, executor.clone(), &targets, started, SetupCycle::Late { attempt, connected }, caller.clone()).await {
                            Ok(unfinished) => unfinished,
                            Err(error) if error.is_cancelled() => return Err(error.into_vendor()),
                            Err(error) => {
                                provider.log(Level::Error, || format!("Failed to initialize worker {worker}"), Some(error.output_failure()));
                                self.schedule_setup_retry(provider, transport, targets.clone(), started)?;
                                return Ok(Late::Stop);
                            }
                        };
                        self.schedule_setup_retry(provider, transport, unfinished.failed, started)?;
                        let missing = provider.still_claimed(transport, unfinished.missing);
                        if missing.is_empty() { return Ok(Late::Next); }
                        provider.log(Level::Warn, || format!("Worker {worker}: {} devices could not connect, retrying", missing.len()), None);
                        pending.offer(&entries(provider, transport, &missing));
                        Ok(Late::Retry(missing.len()))
                    }).await?;
                    pending.offer(&newer);
                    match step {
                        Late::Next => continue,
                        Late::Retry(missing) => {
                            last = missing_failure(missing);
                            return Ok(true);
                        }
                        Late::Stop => return Ok(false),
                    }
                }
            }).await;
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
    #[allow(clippy::too_many_arguments)]
    async fn recover(
        &self,
        provider: &AppleSimulatorProvider,
        transport: &Transport,
        lost: &[Simulator],
        loss: Option<i32>,
        readded: bool,
        again: bool,
        caller: VendorContext,
    ) -> VendorResult<()> {
        let result: ProvisioningResult<Vec<FailedSetup>> = async {
            // D44: `again` is the attempt of a simulator added back while an older recovery of it ran.
            if readded || again {
                provider.log(
                    Level::Warn,
                    || format!("Re-provisioning {}, added back to the devices file", lost[0].serial()),
                    None,
                );
                return provider
                    .provisioner
                    .existing([lost[0].udid().to_owned()], transport.clone(), caller.clone())
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
                    return Ok(Vec::new());
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
                let id = apple_id(device.udid(), transport);
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
            provider
                .provisioner
                .existing(udids, transport.clone(), caller.clone())
                .await
        }
        .await;
        // D44: after a worker loss the handle stays disconnected, so without an owner the scan would start again on the
        // next tick. After the other two nothing would ever set it up again.
        let (result, failures) = match result {
            Ok(failures) => (Ok(()), failures),
            Err(error) if error.is_cancelled() => (Err(error), Vec::new()),
            Err(error) => {
                let failures = lost
                    .iter()
                    .map(|d| FailedSetup::Existing {
                        udid: d.udid().to_owned(),
                        unreachable: false,
                        refused: false,
                    })
                    .collect();
                (Err(error), failures)
            }
        };
        // Only a replay clock can fail, and then there is no limit to count from.
        let now = match failures.is_empty() {
            true => 0,
            false => provider.services.timing.elapsed().unwrap_or(0),
        };
        let keys: Vec<_> = lost.iter().map(|d| apple_id(d.udid(), transport)).collect();
        let (failures, again_for) = provider.settle(&keys, |state, current| {
            // In the section that ends `recovering`, so that no scan finds the handle between the two.
            let mut again_for = Vec::new();
            let failures = failures
                .into_iter()
                .filter(|failure| {
                    let FailedSetup::Existing { udid, refused, .. } = failure else {
                        return false;
                    };
                    let Some(old) = lost.iter().find(|d| d.udid() == udid) else {
                        return false;
                    };
                    let id = apple_id(old.udid(), transport);
                    // A registration rejected because the simulator left the file or the run is no failed setup.
                    let usable = state.devices.get(&id).is_some_and(|c| Arc::ptr_eq(&c.handle, old))
                        && !state.removed.contains(&id)
                        && !state.exhausted.contains(&id)
                        && !old.retired();
                    // D44: removed and added back since this recovery started. Its result is about the old claim, so the
                    // simulator gets an attempt of its own, and stays marked as recovering until that starts.
                    if usable && !current.contains(&id) {
                        again_for.push(old.clone());
                        return false;
                    }
                    if usable {
                        state.parked.push(Parked {
                            handle: old.clone(),
                            for_: match *refused {
                                true => ParkedFor::Refused,
                                false => ParkedFor::Retry,
                            },
                            since: now,
                            // A90, as below: only a connected loss's predecessor is retained.
                            retained: loss.is_some() || readded,
                        });
                    }
                    usable
                })
                .collect::<Vec<_>>();
            state.recovering.retain(|d| {
                !lost.iter().any(|old| Arc::ptr_eq(d, old)) || again_for.iter().any(|again| Arc::ptr_eq(d, again))
            });
            // A90: worker-loss replacement predecessors are not retained by the source.
            if loss.is_some() || readded {
                for old in lost {
                    let id = apple_id(old.udid(), transport);
                    if state
                        .devices
                        .get(&id)
                        .is_none_or(|current| !Arc::ptr_eq(&current.handle, old))
                    {
                        state.replaced.push(old.clone());
                    }
                }
            }
            (failures, again_for)
        });
        for device in again_for {
            self.recover_again(provider, transport, device, loss, readded, &caller)?;
        }
        provider
            .isolate(result, || {
                if readded {
                    format!("Failed to re-provision {}", lost[0].serial())
                } else {
                    format!(
                        "Failed to re-provision simulators of {}",
                        transport_text(transport, "Local")
                    )
                }
            })
            .map_err(ProvisioningError::into_vendor)?;
        if let Err(error) = self.hand_over(provider, transport, failures, now, caller).await {
            provider.log(
                Level::Error,
                || {
                    format!(
                        "Failed to hand over the recovery of {}",
                        transport_text(transport, "Local")
                    )
                },
                Some(error.failure().clone()),
            );
        }
        Ok(())
    }
    /// D44: the attempt of a simulator added back while an older recovery of it ran, as the scan would start it, with
    /// claims taken now.
    fn recover_again(
        &self,
        provider: &AppleSimulatorProvider,
        transport: &Transport,
        device: Simulator,
        loss: Option<i32>,
        readded: bool,
        caller: &VendorContext,
    ) -> VendorResult<()> {
        let this = self.me.upgrade().ok_or_else(|| {
            other(
                "java.lang.IllegalStateException",
                "Recovery allocation has been released",
            )
        })?;
        let provider = provider.this()?;
        let transport = transport.clone();
        let caller = caller.clone();
        let claims = provider.claim_now([apple_id(device.udid(), &transport)]);
        self.recoveries
            .spawn(provider.services.runtime.clone().as_ref(), async move {
                with_claims(
                    claims,
                    this.recover(&provider, &transport, &[device], loss, readded, true, caller),
                )
                .await
            });
        Ok(())
    }
    /// D44: who takes what a worker-loss recovery couldn't set up. An unreachable worker has its connection retry, a
    /// worker that answered and failed has D33's budget, and a refusal has nobody: asking again changes nothing, and
    /// repeated failed logins can get marathon's host banned.
    async fn hand_over(
        &self,
        provider: &AppleSimulatorProvider,
        transport: &Transport,
        failures: Vec<FailedSetup>,
        started: i64,
        caller: VendorContext,
    ) -> VendorResult<()> {
        let mut unreachable_targets = Vec::new();
        let mut failed_targets = Vec::new();
        for failure in failures {
            let FailedSetup::Existing {
                udid,
                unreachable,
                refused,
            } = failure
            else {
                continue;
            };
            if unreachable {
                unreachable_targets.push(AppleTarget::Simulator {
                    udid: udid.as_str().into(),
                });
            } else if refused {
                provider.log(
                    Level::Error,
                    || {
                        format!(
                            "Simulator {udid} on worker {}: its connection was refused, it stays out of the run",
                            transport.id().to_owned()
                        )
                    },
                    None,
                );
            } else {
                failed_targets.push(AppleTarget::Simulator {
                    udid: udid.as_str().into(),
                });
            }
        }
        if !unreachable_targets.is_empty() {
            self.missing(transport.clone(), unreachable_targets, started, caller)
                .await?;
        }
        self.schedule_setup_retry(provider, transport, failed_targets, started)
    }
}
fn not_current(state: &State, id: &str, device: &Simulator) -> bool {
    state
        .devices
        .get(id)
        .is_none_or(|current| !Arc::ptr_eq(&current.handle, device))
}
fn duration_millis(duration: jcompat::jtime::Duration) -> VendorResult<i64> {
    duration
        .to_millis()
        .ok_or_else(|| other("java.lang.ArithmeticException", "long overflow"))
}
/// What a late setup on a connected worker leaves its retry to do.
enum Late {
    Next,
    Retry(usize),
    Stop,
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
        let Ok(provider) = self.provider() else {
            return false;
        };
        let offered = entries(&provider, transport, targets);
        let offered = self.state().pending.get(transport).is_some_and(|p| p.offer(&offered));
        if offered {
            provider.log(
                Level::Info,
                || {
                    format!(
                        "Worker {} is not connected yet, its {} new devices will be set up once it connects",
                        transport.id().to_owned(),
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
                            transport.id().to_owned()
                        )
                    },
                    Some(error.output_failure()),
                );
                provider.give_up_targets(&transport, &targets, ParkedFor::GivenUp);
                return Ok(());
            }
            provider.log(
                Level::Warn,
                || {
                    format!(
                        "Failed to connect to worker {} ({}), retrying",
                        transport.id().to_owned(),
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
                        transport.id().to_owned(),
                        targets.len(),
                        if retry { "retrying" } else { "they stay out of the run" }
                    )
                },
                None,
            );
            if retry {
                let error = missing_failure(targets.len());
                self.retry_later(transport, targets, started, error)?;
            } else {
                provider.give_up_targets(&transport, &targets, ParkedFor::GivenUp);
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
    fn forget_setup(&self, id: &str) {
        self.state().setup.forget(id);
    }
    fn admit(&self, id: &str, device: &Simulator) -> ProvisioningResult<()> {
        let provider = self.provider()?;
        // D33: a retried simulator joins as late as one of a late worker, under the same cut-off.
        let joining = self.state().pending.contains_key(device.transport()) || setup_retry::in_retried_setup();
        let late = joining && !provider.state().devices.contains_key(id);
        if late || self.state().awaiting_admission.contains(id) {
            if let Some(reason) = self.late_join_cutoff()? {
                if let Some(previous) = provider.state().devices.get(id) {
                    previous.handle.retire();
                }
                return Err(SetupDropped(format!(
                    "Simulator {} doesn't join the run late, since {reason}",
                    device.serial()
                ))
                .into());
            }
            self.state().awaiting_admission.insert(id.to_string());
            let this = self.me.clone();
            let id = id.to_owned();
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
                let id = apple_id(d.udid(), d.transport());
                let retired = d.retired();
                let disposed = d.disposed();
                {
                    let state = provider.state();
                    if state.recovering.iter().any(|old| Arc::ptr_eq(old, &d))
                        || state.parked.iter().any(|p| Arc::ptr_eq(&p.handle, &d))
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
                // A replacement still waiting to raise its connect is looked at again on the next tick, so that its
                // disconnect follows that connect.
                // D44: `provider.devices()` is a snapshot, and a replacement may have registered since it was taken.
                if not_current(&state, &id, &d)
                    || state.recovering.iter().any(|old| Arc::ptr_eq(old, &d))
                    || state.connecting.iter().any(|pending| Arc::ptr_eq(&pending.handle, &d))
                {
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
                provider.notify(DeviceEvent::Disconnected(marathon_core::vendor::DeviceRef(d.clone())));
            }
            let mut workers = IndexMap::<Transport, Vec<Simulator>>::new();
            for device in disconnected {
                workers.entry(device.transport().clone()).or_default().push(device);
            }
            let mut groups: Vec<_> = workers.into_iter().map(|(t, d)| (t, d, None, false)).collect();
            for d in failed {
                let id = apple_id(d.udid(), d.transport());
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
                // D44: the recovery acts for these simulators at their generations now.
                let claims = provider.claim_now(lost.iter().map(|d| apple_id(d.udid(), &transport)));
                self.recoveries
                    .spawn(provider.services.runtime.clone().as_ref(), async move {
                        with_claims(
                            claims,
                            this.recover(&provider, &transport, &lost, loss, readded, false, caller),
                        )
                        .await
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
