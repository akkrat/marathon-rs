//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/ArtifactProcessor.kt

mod core;
use crate::cmd::{remote::ssh::sshj::config::SshLogging, retained_tasks::RetainedTasks};
pub use core::AdmissionStats;
use core::{AdmissionCore, Effects, Item, Waiter};
use indexmap::IndexMap;
use jcompat::logging::{Level, Logger};
use marathon_config::vendor::XcresultQueueConfiguration;
use marathon_core::{
    error::{Cancellation, Failure, VendorError, VendorResult},
    executor::shared::{SharedDeferred, Signal, failure},
    runtime::{CancellationSignal, Deferred, VendorFuture, VendorRuntime},
    time::MonotonicClock,
    vendor::VendorContext,
};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::Semaphore;

pub const DRAIN_TIMEOUT_MILLIS: i64 = 300_000;
pub const CANCEL_TIMEOUT_MILLIS: i64 = 60_000;
pub type Undelivered = Arc<dyn Fn(&str) -> Result<(), Failure> + Send + Sync>;
pub type ArtifactAction = Arc<dyn Fn(VendorContext) -> VendorFuture<'static, ()> + Send + Sync>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidLimits(pub String);
#[derive(Clone)]
struct Work {
    batch: String,
    action: ArtifactAction,
    undelivered: Undelivered,
    context: VendorContext,
}
#[derive(PartialEq, Eq)]
enum SourceState {
    Registered,
    Sweeping,
    Reported,
}
struct Source {
    callback: Option<Undelivered>,
    state: SourceState,
}
struct State {
    queue: AdmissionCore<Work>,
    next_id: u64,
    running: IndexMap<u64, Item<Work>>,
    sources: IndexMap<String, IndexMap<String, Source>>,
    undelivered: Vec<String>,
}
/// Explicitly owned by a run. Construct and call off core through the vendor bridge.
/// Drop performs no cancellation or observable cleanup; providers call close_and_join.
pub struct ArtifactProcessor {
    state: Mutex<State>,
    runtime: Arc<dyn VendorRuntime>,
    monotonic: Arc<dyn MonotonicClock>,
    transfers: Semaphore,
    supervisor: Arc<Signal>,
    retained: RetainedTasks,
    logging: SshLogging,
    logger: Logger,
}
impl ArtifactProcessor {
    pub fn new(
        capacity: i32,
        max_concurrent: i32,
        worker_capacity: i32,
        runtime: Arc<dyn VendorRuntime>,
        monotonic: Arc<dyn MonotonicClock>,
        logging: SshLogging,
    ) -> Result<Arc<Self>, InvalidLimits> {
        if capacity < 1 || max_concurrent < 1 || !(1..=capacity).contains(&worker_capacity) {
            return Err(InvalidLimits(format!("Invalid artifact queue limits: capacity={capacity}, maxConcurrent={max_concurrent}, workerCapacity={worker_capacity}").as_str().into()));
        }
        let logger = logging.logger("com.malinskiy.marathon.apple.ArtifactProcessor");
        Ok(Arc::new(Self {
            state: Mutex::new(State {
                queue: AdmissionCore::new(capacity, max_concurrent, worker_capacity),
                next_id: 0,
                running: IndexMap::new(),
                sources: IndexMap::new(),
                undelivered: vec![],
            }),
            runtime,
            monotonic,
            transfers: Semaphore::new(max_concurrent as usize),
            supervisor: Signal::new(),
            retained: RetainedTasks::default(),
            logging,
            logger,
        }))
    }
    pub fn from_configuration(
        config: &XcresultQueueConfiguration,
        runtime: Arc<dyn VendorRuntime>,
        monotonic: Arc<dyn MonotonicClock>,
        logging: SshLogging,
    ) -> Result<Arc<Self>, InvalidLimits> {
        Self::new(
            config.capacity,
            config.concurrency,
            config.effective_worker_capacity(),
            runtime,
            monotonic,
            logging,
        )
    }
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn now(&self, label: &'static str) -> VendorResult<i64> {
        self.monotonic
            .nanos(label)
            .map_err(|e| VendorError::Other(failure("marathon.ReplayError", &e.message.to_owned())))
    }
    fn log(&self, level: Level, message: String, throwable: Option<&Failure>) {
        if self.logging.enabled(&self.logger, level) {
            (self.logging.sink)(&self.logger, level, message.to_owned(), throwable.cloned());
        }
    }
    pub async fn submit(
        self: &Arc<Self>,
        owner: String,
        batch: String,
        worker: String,
        on_undelivered: Option<Undelivered>,
        action: ArtifactAction,
        context: VendorContext,
    ) -> VendorResult<bool> {
        let callback = on_undelivered.unwrap_or_else(|| {
            let weak = Arc::downgrade(self);
            let (owner, batch) = (owner.clone(), batch.clone());
            Arc::new(move |reason| {
                if let Some(this) = weak.upgrade() {
                    this.log(
                        Level::Error,
                        text(&[
                            "Artifact for ",
                            &owner.to_owned(),
                            ", batch ",
                            &batch.to_owned(),
                            " was not processed: ",
                            &reason.to_owned(),
                        ]),
                        None,
                    );
                    let mut diagnostic = owner.clone();
                    diagnostic.push_str(", batch ");
                    diagnostic.push_str(&batch.clone());
                    diagnostic.push_str(": ");
                    diagnostic.push_str(&reason.to_owned());
                    this.report_undelivered(&owner, None, diagnostic);
                }
                Ok(())
            })
        });
        let arrived_at = self.now("apple.ArtifactProcessor.clock")?;
        let decided = SharedDeferred::new();
        let (id, immediate) = {
            let mut state = self.state();
            let id = state.next_id;
            state.next_id = state.next_id.wrapping_add(1);
            let waiter = Waiter {
                item: Item {
                    id,
                    owner: owner.clone(),
                    worker: worker.clone(),
                    payload: Work {
                        batch: batch.clone(),
                        action,
                        undelivered: callback.clone(),
                        context: context.clone(),
                    },
                    enqueued_at: 0,
                },
                arrived_at,
                blocked_by_run: None,
                decided: decided.clone(),
            };
            let immediate = if state.queue.closed {
                Some(None)
            } else if state.queue.admissible(&worker) {
                Some(Some(
                    state.queue.admit(waiter, self.now("apple.ArtifactProcessor.clock")?).2,
                ))
            } else {
                state.queue.wait(waiter);
                None
            };
            (id, immediate)
        };
        let admission = match immediate {
            Some(None) => {
                callback("artifact queue is closed").map_err(VendorError::Other)?;
                return Ok(false);
            }
            Some(Some(admission)) => {
                if admission.starts_worker {
                    self.start(worker.clone());
                }
                admission
            }
            None => match decided.wait_in(context.cancellation.clone()).await {
                Ok(decision) => match *decision {
                    Some(admission) => admission,
                    None => return Ok(false),
                },
                Err(error) => {
                    let cancelled = self.state().queue.cancel_waiter(id);
                    if cancelled.is_some() {
                        callback("admission was cancelled").map_err(VendorError::Other)?;
                    }
                    return Err(error);
                }
            },
        };
        let reason = match admission.blocked_by_run {
            None => String::new(),
            Some(true) => " waiting for run capacity".into(),
            Some(false) => format!(" waiting for worker {}", worker.to_owned()),
        };
        self.log(
            Level::Debug,
            format!(
                "Artifact admission for {}, batch {} took {} ms{}; outstanding={}, on worker {}={}",
                owner.to_owned(),
                batch.to_owned(),
                admission.wait_millis,
                reason,
                admission.depth,
                worker.to_owned(),
                admission.worker_depth
            )
            .as_str()
            .into(),
            None,
        );
        Ok(true)
    }
    fn start(self: &Arc<Self>, worker: String) {
        let this = self.clone();
        self.retained.spawn_cancellable(
            self.runtime.as_ref(),
            Box::new(move |signal| {
                Box::pin(async move {
                    let signal: Arc<dyn CancellationSignal> = Arc::new(WorkerSignal {
                        task: signal,
                        supervisor: this.supervisor.clone(),
                    });
                    if let Some(reason) = signal.reason() {
                        return Err(VendorError::Cancelled(reason));
                    }
                    this.process_worker(worker, signal).await
                })
            }),
        );
    }
    async fn process_worker(self: &Arc<Self>, worker: String, signal: Arc<dyn CancellationSignal>) -> VendorResult<()> {
        loop {
            let work = {
                let mut state = self.state();
                let Some(work) = state.queue.take(&worker) else {
                    return Ok(());
                };
                let work = Arc::new(work);
                // Keep an independent copy for shutdown's first-remover ownership.
                state.running.insert(
                    work.id,
                    Item {
                        id: work.id,
                        owner: work.owner.clone(),
                        worker: work.worker.clone(),
                        payload: work.payload.clone(),
                        enqueued_at: work.enqueued_at,
                    },
                );
                work
            };
            let outcome = async {
                if let Some(reason) = signal.reason() { return Err(VendorError::Cancelled(reason)); }
                let permit = tokio::select! {
                    biased;
                    reason = signal.cancelled() => return Err(VendorError::Cancelled(reason)),
                    permit = self.transfers.acquire() => permit.map_err(|e| VendorError::Other(failure("java.lang.IllegalStateException", &e.to_string())))?,
                };
                let wait = self.now("apple.ArtifactProcessor.clock")?.wrapping_sub(work.enqueued_at) / 1_000_000;
                self.log(Level::Debug, format!("Artifact queue wait for {}, batch {}: {wait} ms", work.owner.to_owned(), work.payload.batch.to_owned()).as_str().into(), None);
                let mut context = work.payload.context.clone();
                context.cancellation = signal.clone();
                let result = (work.payload.action)(context).await;
                drop(permit);
                result
            }.await;
            let owned = self.state().running.shift_remove(&work.id).is_some();
            if !owned {
                self.log(
                    Level::Warn,
                    format!(
                        "Artifact work for {}, batch {} returned after shutdown reported it as undelivered",
                        work.owner.to_owned(),
                        work.payload.batch.to_owned()
                    )
                    .as_str()
                    .into(),
                    outcome.as_ref().err().map(VendorError::failure),
                );
                return Ok(());
            }
            let report = if let Err(error) = outcome {
                self.log(
                    Level::Warn,
                    format!(
                        "Artifact processing failed for {}, batch {}",
                        work.owner.to_owned(),
                        work.payload.batch.to_owned()
                    )
                    .as_str()
                    .into(),
                    Some(error.failure()),
                );
                let mut reason = String::from("processing did not complete: ");
                reason.push_str(&error.failure().to_java_string());
                (work.payload.undelivered)(&reason).map_err(VendorError::Other)
            } else {
                Ok(())
            };
            self.complete(&work).await?;
            report?;
        }
    }
    async fn complete(self: &Arc<Self>, work: &Item<Work>) -> VendorResult<()> {
        let now = self.now("apple.ArtifactProcessor.clock")?;
        let effects = self.state().queue.complete(&work.owner, &work.worker, now);
        self.effects(effects);
        Ok(())
    }
    fn effects(self: &Arc<Self>, effects: Effects) {
        for (worker, _, info) in &effects.admitted {
            if info.starts_worker {
                self.start(worker.clone());
            }
        }
        for done in effects.barriers {
            done.complete(Ok(()));
        }
        for (_, decided, info) in effects.admitted {
            decided.complete(Ok(Some(info)));
        }
    }
    pub async fn await_owner(&self, owner: &str, context: &VendorContext) -> VendorResult<()> {
        let started = self.now("apple.ArtifactProcessor.await")?;
        let done = self.state().queue.owner_done(owner);
        if let Some(done) = done {
            done.wait_in(context.cancellation.clone()).await?;
        }
        let elapsed = self.now("apple.ArtifactProcessor.await")?.wrapping_sub(started) / 1_000_000;
        self.log(
            Level::Debug,
            format!("Artifact barrier for {} took {elapsed} ms", owner.to_owned())
                .as_str()
                .into(),
            None,
        );
        Ok(())
    }
    pub fn protect(&self, owner: String, source: String, callback: Undelivered) -> Result<(), Failure> {
        let late = {
            let mut state = self.state();
            state
                .sources
                .entry(owner)
                .or_default()
                .entry(source)
                .or_insert_with(|| Source {
                    callback: Some(callback.clone()),
                    state: SourceState::Registered,
                });
            state.queue.closed
        };
        if late {
            callback("artifact queue is closed")?;
        }
        Ok(())
    }
    pub fn release(&self, owner: &str, source: &str) {
        let mut state = self.state();
        if let Some(owned) = state.sources.get_mut(owner)
            && owned.get(source).is_some_and(|s| s.state == SourceState::Registered)
        {
            owned.shift_remove(source);
        }
    }
    pub fn report_undelivered(&self, owner: &str, source: Option<&str>, diagnostic: String) -> bool {
        let mut state = self.state();
        if let Some(source) = source {
            let entry = state
                .sources
                .entry(owner.to_owned())
                .or_default()
                .entry(source.to_owned())
                .or_insert(Source {
                    callback: None,
                    state: SourceState::Registered,
                });
            if entry.state == SourceState::Reported {
                return false;
            }
            entry.state = SourceState::Reported;
        }
        state.undelivered.push(diagnostic);
        true
    }
    /// Insertion order becomes the wire-visible `find ... ! -name` argument order.
    pub fn kept_sources(&self, owner: &str) -> Vec<String> {
        self.state()
            .sources
            .get(owner)
            .map(|s| s.keys().cloned().collect())
            .unwrap_or_default()
    }
    pub fn admission_stats(&self) -> AdmissionStats {
        let snapshot = self.state().queue.stats_snapshot();
        AdmissionCore::<Work>::summarize(snapshot)
    }
    fn report_safely(&self, reason: &str, callback: &Undelivered) {
        if let Err(error) = callback(reason) {
            self.log(
                Level::Error,
                text(&["Reporting an undelivered artifact failed: ", &reason.to_owned()]),
                Some(&error),
            );
        }
    }
    pub async fn close_and_join(
        self: &Arc<Self>,
        timeout: i64,
        cancel_timeout: i64,
        context: &VendorContext,
    ) -> VendorResult<Vec<String>> {
        let (finished, rejected) = {
            let mut state = self.state();
            (state.queue.done.clone(), state.queue.close())
        };
        for waiter in &rejected {
            waiter.decided.complete(Ok(None));
        }
        let started = self.now("apple.ArtifactProcessor.closeAndJoin")?;
        if context
            .clock
            .with_timeout_or_null(
                context.cancellation.clone(),
                timeout,
                Box::new(|signal| Box::pin(async { finished.wait_in(signal).await.map(|_| ()) })),
            )
            .await?
            .is_none()
        {
            let pending = self.state().queue.pending();
            let pending = pending
                .iter()
                .map(|(o, n)| format!("{}: {n} bundles", o.to_owned()))
                .collect::<Vec<_>>()
                .join(", ");
            self.log(
                Level::Warn,
                format!("Artifact drain timed out; cancelling pending work and retaining remote bundles: [{pending}]")
                    .as_str()
                    .into(),
                None,
            );
        }
        self.supervisor.cancel(Cancellation::Requested(failure(
            "kotlinx.coroutines.JobCancellationException",
            "Job was cancelled",
        )));
        let tasks = self.retained.snapshot();
        for task in &tasks {
            task.cancel(Cancellation::Requested(failure(
                "kotlinx.coroutines.JobCancellationException",
                "Job was cancelled",
            )));
        }
        context.clock.with_timeout_or_null(context.cancellation.clone(), cancel_timeout, Box::new(move |signal| Box::pin(async move {
            for task in tasks { tokio::select! { biased; reason = signal.cancelled() => return Err(VendorError::Cancelled(reason)), _ = task.join() => {} } }
            Ok(())
        }))).await?;
        let (stuck, residual) = {
            let mut state = self.state();
            (
                std::mem::take(&mut state.running).into_values().collect::<Vec<_>>(),
                state.queue.residual(),
            )
        };
        if !stuck.is_empty() {
            let names = stuck
                .iter()
                .map(|w| format!("{}, batch {}", w.owner.to_owned(), w.payload.batch.to_owned()))
                .collect::<Vec<_>>()
                .join(", ");
            self.log(Level::Warn, format!("Artifact work did not stop within {cancel_timeout} ms of cancellation; finalizing without it: [{names}]").as_str().into(), None);
        }
        for work in stuck {
            self.report_safely(
                &format!("still running {cancel_timeout} ms after shutdown cancelled it").as_str(),
                &work.payload.undelivered,
            );
            self.complete(&work).await?;
        }
        for work in residual {
            self.report_safely("not started before shutdown", &work.payload.undelivered);
            self.complete(&work).await?;
        }
        context
            .clock
            .with_timeout_or_null(
                context.cancellation.clone(),
                cancel_timeout,
                Box::new(|signal| Box::pin(async { finished.wait_in(signal).await.map(|_| ()) })),
            )
            .await?;
        for waiter in rejected {
            self.report_safely("artifact queue is closed", &waiter.item.payload.undelivered);
        }
        let unresolved = {
            let mut state = self.state();
            state
                .sources
                .values_mut()
                .flat_map(|s| s.values_mut())
                .filter(|s| s.state == SourceState::Registered)
                .filter_map(|s| {
                    s.state = SourceState::Sweeping;
                    s.callback.clone()
                })
                .collect::<Vec<_>>()
        };
        for callback in unresolved {
            self.report_safely("never handed to the artifact queue", &callback);
        }
        let elapsed = self.now("apple.ArtifactProcessor.closeAndJoin")?.wrapping_sub(started) / 1_000_000;
        self.log(
            Level::Debug,
            format!("Artifact finalization took {elapsed} ms").as_str().into(),
            None,
        );
        let stats = self.admission_stats();
        let (capacity, concurrency, worker_capacity) = {
            let state = self.state();
            (
                state.queue.capacity,
                state.queue.concurrency,
                state.queue.worker_capacity,
            )
        };
        if stats.admitted > 0 {
            self.log(Level::Info, format!("Artifact queue admission: {} bundles admitted, {} after waiting ({} for run capacity, {} for their worker); wait max {} ms, p95 {} ms, total {} ms; capacity={capacity}, concurrency={concurrency}, workerCapacity={worker_capacity}", stats.admitted, stats.waited, stats.waited_for_run, stats.waited - stats.waited_for_run, stats.max_wait_millis, stats.p95_wait_millis, stats.total_wait_millis).as_str().into(), None);
        }
        let retained = self.state().undelivered.clone();
        if !retained.is_empty() {
            self.log(Level::Error, format!("{} artifact bundle(s) were not delivered; their remote sources are kept on the workers until a later run prepares the same device:\n{}", retained.len(), retained.iter().map(String::clone).collect::<Vec<_>>().join("\n")).as_str().into(), None);
        }
        Ok(retained)
    }
}
fn text(parts: &[&str]) -> String {
    parts.concat().as_str().into()
}
struct WorkerSignal {
    task: Arc<dyn CancellationSignal>,
    supervisor: Arc<Signal>,
}
impl CancellationSignal for WorkerSignal {
    fn reason(&self) -> Option<Cancellation> {
        self.task.reason().or_else(|| self.supervisor.reason())
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Cancellation> + Send + '_>> {
        Box::pin(async {
            tokio::select! { biased; reason = self.task.cancelled() => reason, reason = self.supervisor.cancelled() => reason }
        })
    }
}
#[cfg(test)]
mod tests;
