//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/internal/pub/Track.kt
//! and analytics/MappingTracker.kt, TrackerFactory.kt, internal/sub/DelegatingTrackerInternal.kt,
//! TrackerInternal.kt, TrackerInternalAdapter.kt and ExecutionReportGenerator.kt.

use futures_util::FutureExt;
use marathon_logging::{Component, Level, LogSettings};

use crate::{
    artifact_io::*,
    error::*,
    model::*,
    report::*,
    runtime::*,
    time::WallClock,
    wiring::{ReportLifecycle, Tracker},
};
use jcompat::jtime::Instant;
use marathon_error::FailureKind;
use std::{
    collections::HashSet,
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Notify, mpsc, oneshot},
    time::{Instant as Deadline, sleep_until},
};

/// Runs on the vendor runtime, either inline with the pipeline or on its remote delivery worker.
pub trait TrackerInternal: Send + Sync {
    fn track<'a>(&'a self, event: &'a Event) -> VendorFuture<'a, ()>;
    fn close(&self) -> ServiceFuture<'_, ()>;
}
/// Implement only the event handlers a subscriber needs.
pub trait TrackerInternalAdapter: Send + Sync {
    fn device_connected<'a>(&'a self, _event: &'a DeviceConnectedEvent) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn device_disconnected<'a>(&'a self, _event: &'a DeviceDisconnectedEvent) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn device_preparing<'a>(&'a self, _event: &'a DevicePreparingEvent) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn device_provider_preparing<'a>(&'a self, _event: &'a DeviceProviderPreparingEvent) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn test<'a>(&'a self, _event: &'a TestEvent) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
impl<T: TrackerInternalAdapter> TrackerInternal for T {
    fn track<'a>(&'a self, event: &'a Event) -> VendorFuture<'a, ()> {
        match event {
            Event::DeviceConnected(e) => self.device_connected(e),
            Event::DeviceDisconnected(e) => self.device_disconnected(e),
            Event::DevicePreparing(e) => self.device_preparing(e),
            Event::DeviceProviderPreparing(e) => self.device_provider_preparing(e),
            Event::Test(e) => self.test(e),
        }
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        TrackerInternalAdapter::close(self)
    }
}
/// Where a close's later failures go: the first is returned, and `Marathon::on_finish` logs that one.
pub type CloseLog = Option<(ServiceLog, Component)>;
/// D51: a failed close doesn't stop the later ones (Kotlin: it does, C11). Cancellation still stops
/// them, since the closes after it would only be cancelled too.
fn close_failed(first: &mut Option<OperationError>, error: OperationError, log: &CloseLog) -> OperationResult<()> {
    let OperationError::Failed(failure) = &error else {
        return Err(first.take().unwrap_or(error));
    };
    match (first.is_some(), log) {
        (false, _) => *first = Some(error),
        (true, Some((log, logger))) => log(logger, Level::ERROR, "A later close also failed", Some(failure)),
        (true, None) => {}
    }
    Ok(())
}
#[derive(Clone)]
pub struct DelegatingTrackerInternal {
    delegates: Vec<Arc<dyn TrackerInternal>>,
    log: CloseLog,
}
impl DelegatingTrackerInternal {
    pub fn new(delegates: Vec<Arc<dyn TrackerInternal>>) -> Self {
        Self { delegates, log: None }
    }
    pub fn with_close_log(mut self, log: ServiceLog, logger: Component) -> Self {
        self.log = Some((log, logger));
        self
    }
}
impl TrackerInternal for DelegatingTrackerInternal {
    fn track<'a>(&'a self, event: &'a Event) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            for delegate in &self.delegates {
                delegate.track(event).await?;
            }
            Ok(())
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async move {
            let mut first = None;
            for delegate in &self.delegates {
                if let Err(error) = delegate.close().await {
                    close_failed(&mut first, error, &self.log)?;
                }
            }
            first.map_or(Ok(()), Err)
        })
    }
}
#[derive(Clone)]
pub struct ExecutionReportGenerator {
    events: Arc<Mutex<Vec<Event>>>,
    reporters: Vec<Arc<dyn Reporter>>,
    log: CloseLog,
}
impl ExecutionReportGenerator {
    pub fn new(reporters: Vec<Arc<dyn Reporter>>) -> Self {
        Self {
            events: Arc::default(),
            reporters,
            log: None,
        }
    }
    pub fn with_close_log(mut self, log: ServiceLog, logger: Component) -> Self {
        self.log = Some((log, logger));
        self
    }
    fn snapshot(&self) -> ExecutionReport {
        let events = self.events.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let (mut connected, mut disconnected, mut preparing, mut provider, mut tests) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for event in events {
            match event {
                Event::DeviceConnected(e) => connected.push(e),
                Event::DeviceDisconnected(e) => disconnected.push(e),
                Event::DevicePreparing(e) => preparing.push(e),
                Event::DeviceProviderPreparing(e) => provider.push(e),
                Event::Test(e) => tests.push(e),
            }
        }
        connected.sort_by_key(|e| (e.instant.seconds(), e.instant.nanos()));
        disconnected.sort_by_key(|e| (e.instant.seconds(), e.instant.nanos()));
        preparing.sort_by_key(|e| (e.start.seconds(), e.start.nanos()));
        provider.sort_by_key(|e| (e.start.seconds(), e.start.nanos()));
        tests.sort_by_key(|e| (e.instant.seconds(), e.instant.nanos()));
        ExecutionReport::new(connected, disconnected, preparing, provider, tests)
    }
}
impl TrackerInternal for ExecutionReportGenerator {
    fn track<'a>(&'a self, event: &'a Event) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(event.clone());
            Ok(())
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async move {
            let report = self.snapshot();
            let mut first = None;
            for reporter in &self.reporters {
                if let Err(error) = reporter.generate(&report).await {
                    close_failed(&mut first, error, &self.log)?;
                }
            }
            first.map_or(Ok(()), Err)
        })
    }
}
/// D51: when to leave a backend alone so it can recover from slow failures.
#[derive(Clone, Copy)]
pub struct BackendLimits {
    /// Limit per send or admission wait, and the total drain and close allowance.
    pub send: Duration,
    pub slow_failure: Duration,
    pub cool_off: Duration,
}
impl Default for BackendLimits {
    /// 30 s sits above carbon's own limits (20 s in all) and well under the 60 s per step of the
    /// InfluxDB clients.
    /// One second separates a refusal or a reset, which cost nothing, from a timeout or an
    /// unreachable host. Five minutes lets an unreachable backend recover before trying again.
    fn default() -> Self {
        Self {
            send: Duration::from_secs(30),
            slow_failure: Duration::from_secs(1),
            cool_off: Duration::from_secs(300),
        }
    }
}
/// Distinct failures logged per run; a backend's text can name a test, and that mustn't mean a line per test.
const LOGGED_KINDS: usize = 8;
#[derive(Default)]
struct GuardState {
    kinds: HashSet<(FailureKind, Option<String>)>,
    skip_until: Option<Deadline>,
    announced_skip: bool,
    failed: u64,
    skipped: u64,
}
impl GuardState {
    fn cooling_off(&self) -> bool {
        self.skip_until.is_some_and(|until| Deadline::now() < until)
    }
}
#[derive(Default)]
struct BackendState {
    guard: Mutex<GuardState>,
    changed: Notify,
}
impl BackendState {
    fn lock(&self) -> std::sync::MutexGuard<'_, GuardState> {
        self.guard.lock().unwrap_or_else(|e| e.into_inner())
    }
}
/// D51: metrics are never worth a test run. Kotlin lets a tracker's exception end the queue actor,
/// and a dead queue leaves the run hanging (U168). A remote backend's failure is logged once per kind and
/// the run carries on. Its close still reports: `on_finish` keeps the exit code that a failed close has.
/// A close that runs out of time is only logged.
pub struct BackendGuard {
    inner: Arc<dyn TrackerInternal>,
    log: ServiceLog,
    logger: Component,
    limits: BackendLimits,
    state: Arc<BackendState>,
}
impl BackendGuard {
    pub fn new(inner: Arc<dyn TrackerInternal>, log: ServiceLog, logger: Component, limits: BackendLimits) -> Self {
        Self {
            inner,
            log,
            logger,
            limits,
            state: Arc::default(),
        }
    }
    fn failed(&self, failure: &Failure, slow: bool, counted: bool) {
        let (first_of_kind, announce) = {
            let mut state = self.state.lock();
            state.failed += u64::from(counted);
            if slow {
                state.skip_until = Some(Deadline::now() + self.limits.cool_off);
            }
            let first_of_kind = state.kinds.len() < LOGGED_KINDS
                && state
                    .kinds
                    .insert((failure.kind.dedup_key(), failure.message.to_owned()));
            let announce = slow && !std::mem::replace(&mut state.announced_skip, true);
            (first_of_kind, announce)
        };
        if slow {
            self.state.changed.notify_waiters();
        }
        if first_of_kind {
            let text = "Cannot send test metrics to the analytics backend; the run carries on. Each kind of failure is logged once";
            (self.log)(&self.logger, Level::WARN, text, Some(failure));
        }
        if announce {
            let text = format!(
                "The analytics backend failed slowly, so test metrics are skipped for {} s after each such failure",
                self.limits.cool_off.as_secs()
            );
            (self.log)(&self.logger, Level::WARN, &text, None);
        }
    }
    fn warn_losses(&self, logger: &Component) {
        let (failed, skipped) = {
            let state = self.state.lock();
            (state.failed, state.skipped)
        };
        if failed + skipped > 0 {
            let text = format!(
                "The analytics backend lost test metrics: {failed} results failed to send, and up to {skipped} were skipped"
            );
            (self.log)(logger, Level::WARN, &text, None);
        }
    }
    async fn close_backend(&self) -> OperationResult<()> {
        self.warn_losses(&self.logger);
        self.inner.close().await
    }
    fn warn_close_timeout(&self, logger: &Component) {
        let text = format!(
            "The analytics backend did not close in {} ms; test metrics it still held are lost",
            self.limits.send.as_millis()
        );
        (self.log)(logger, Level::WARN, &text, None);
    }
}
impl TrackerInternal for BackendGuard {
    fn track<'a>(&'a self, event: &'a Event) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let counted = matches!(event, Event::Test(_));
            {
                let mut state = self.state.lock();
                if state.cooling_off() {
                    state.skipped += u64::from(counted);
                    return Ok(());
                }
            }
            let started = tokio::time::Instant::now();
            match tokio::time::timeout(self.limits.send, self.inner.track(event)).await {
                Err(_) => {
                    let text = format!(
                        "No answer from the analytics backend in {} ms",
                        self.limits.send.as_millis()
                    );
                    self.failed(
                        &crate::executor::shared::failure(FailureKind::Timeout, &text),
                        true,
                        counted,
                    );
                    Ok(())
                }
                Ok(Err(error @ VendorError::Cancelled(_))) => Err(error),
                Ok(Err(error)) => {
                    let slow = started.elapsed() >= self.limits.slow_failure;
                    self.failed(error.failure(), slow, counted);
                    Ok(())
                }
                Ok(Ok(())) => Ok(()),
            }
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async move {
            match tokio::time::timeout(self.limits.send, self.close_backend()).await {
                Ok(result) => result,
                Err(_) => {
                    self.warn_close_timeout(&self.logger);
                    Ok(())
                }
            }
        })
    }
}

// At about 34 events/s this backlog fits D51's 30 s drain allowance for any backend.
// More buffering would delay backpressure and leave a larger backlog at shutdown.
const REMOTE_QUEUE_CAPACITY: usize = 1000;

struct RemoteAdmission {
    shutdown: oneshot::Sender<Deadline>,
    sender: mpsc::Sender<Event>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rejected {
    WaitExpired,
    CoolingOff,
    Closing,
    WorkerStopped,
}
impl Rejected {
    fn reason(self) -> &'static str {
        match self {
            Self::WaitExpired => "the queue wait expired",
            Self::CoolingOff => "the backend is cooling off",
            Self::Closing => "admission has closed",
            Self::WorkerStopped => "the delivery task stopped",
        }
    }
}

#[derive(Default)]
struct RejectedCounts {
    wait_expired: u64,
    cooling_off: u64,
    closing: u64,
    worker_stopped: u64,
}
impl RejectedCounts {
    fn total(&self) -> u64 {
        self.wait_expired + self.cooling_off + self.closing + self.worker_stopped
    }
    fn add(&mut self, reason: Rejected) -> bool {
        let first = self.total() == 0;
        match reason {
            Rejected::WaitExpired => self.wait_expired += 1,
            Rejected::CoolingOff => self.cooling_off += 1,
            Rejected::Closing => self.closing += 1,
            Rejected::WorkerStopped => self.worker_stopped += 1,
        }
        first
    }
}

/// D86: one ordered delivery task per backend. The report generator still takes each event inline.
struct RemoteTracker {
    admission: Mutex<Option<RemoteAdmission>>,
    task: Arc<dyn VendorTask>,
    state: Arc<BackendState>,
    limits: BackendLimits,
    rejected: Mutex<RejectedCounts>,
    log: ServiceLog,
    logger: Component,
}

impl RemoteTracker {
    fn new(io: &dyn VendorRuntime, guard: BackendGuard, capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        let (shutdown, request) = oneshot::channel();
        let limits = guard.limits;
        let state = guard.state.clone();
        let log = guard.log.clone();
        let logger = marathon_logging::LogSettings::default().component(module_path!());
        let task = io.spawn(Box::pin(Self::deliver(guard, receiver, request, logger.clone())));
        Self {
            admission: Mutex::new(Some(RemoteAdmission { shutdown, sender })),
            task,
            state,
            limits,
            rejected: Mutex::default(),
            log,
            logger,
        }
    }

    fn with_admission<T>(
        &self,
        action: impl FnOnce(&mpsc::Sender<Event>, &mut GuardState) -> Result<T, Rejected>,
    ) -> Result<T, Rejected> {
        let admission = self.admission.lock().unwrap_or_else(|e| e.into_inner());
        let sender = &admission.as_ref().ok_or(Rejected::Closing)?.sender;
        if sender.is_closed() {
            return Err(Rejected::WorkerStopped);
        }
        // Keep either transition from racing an enqueue, including an enqueue with a reserved permit.
        let mut state = self.state.lock();
        if state.cooling_off() {
            return Err(Rejected::CoolingOff);
        }
        action(sender, &mut state)
    }

    async fn admit(&self, event: &Event) -> Result<(), Rejected> {
        let waiting = self.with_admission(|sender, _| match sender.try_reserve() {
            Ok(permit) => {
                permit.send(event.clone());
                Ok(None)
            }
            Err(mpsc::error::TrySendError::Full(())) => Ok(Some((sender.clone(), Deadline::now() + self.limits.send))),
            Err(mpsc::error::TrySendError::Closed(())) => Err(Rejected::WorkerStopped),
        })?;
        let Some((sender, deadline)) = waiting else {
            return Ok(());
        };
        let reserve = sender.reserve_owned();
        let deadline = sleep_until(deadline);
        tokio::pin!(reserve, deadline);
        loop {
            let changed = self.state.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            self.with_admission(|_, _| Ok(()))?;
            tokio::select! {
                biased;
                _ = &mut changed => {}
                // A slot ready at the deadline can still admit this event.
                permit = &mut reserve => return self.with_admission(|_, _| {
                    permit.map_err(|_| Rejected::WorkerStopped)?.send(event.clone());
                    Ok(())
                }),
                _ = &mut deadline => {
                    // The first expiry opens the same circuit as a slow send and releases other waiters.
                    let result = self.with_admission(|_, state| {
                        state.skip_until = Some(Deadline::now() + self.limits.cool_off);
                        Err(Rejected::WaitExpired)
                    });
                    if result == Err(Rejected::WaitExpired) {
                        self.state.changed.notify_waiters();
                    }
                    return result;
                }
            }
        }
    }

    fn record_rejection(&self, reason: Rejected) {
        let first = self.rejected.lock().unwrap_or_else(|e| e.into_inner()).add(reason);
        if first {
            let text = format!(
                "The analytics backend is dropping test metric events because {}",
                reason.reason()
            );
            (self.log)(&self.logger, Level::WARN, &text, None);
        }
    }

    async fn deliver(
        guard: BackendGuard,
        mut receiver: mpsc::Receiver<Event>,
        request: oneshot::Receiver<Deadline>,
        logger: Component,
    ) -> VendorResult<()> {
        let deadline = async {
            sleep_until(request.await.ok()?).await;
            Some(())
        };
        let mut close_started = false;
        let delivery = async {
            while let Some(event) = receiver.recv().await {
                // The guard handles ordinary send failures. A cancelled send belongs to this worker,
                // and cancellation of the test run still leaves the worker responsible for closing.
                let _ = guard.track(&event).await;
            }
            close_started = true;
            guard.close_backend().await.map_err(vendor_error)
        };
        tokio::select! {
            biased;
            expired = deadline => {
                // Loss of the owner without a close request also drops the in-flight future.
                if expired.is_some() {
                    if !close_started {
                        guard.warn_losses(&logger);
                    }
                    guard.warn_close_timeout(&logger);
                }
                Ok(())
            }
            result = delivery => result,
        }
    }
}

impl Drop for RemoteTracker {
    fn drop(&mut self) {
        // Tracker clones can submit after close returns. The last owner has the complete count.
        let counts = self.rejected.get_mut().unwrap_or_else(|e| e.into_inner());
        let total = counts.total();
        if total > 0 {
            let text = format!(
                "The analytics backend dropped {total} test metric events before admission: {} wait expired, {} cooling off, {} admission closed, {} delivery task stopped",
                counts.wait_expired, counts.cooling_off, counts.closing, counts.worker_stopped,
            );
            (self.log)(&self.logger, Level::WARN, &text, None);
        }
    }
}

impl TrackerInternal for RemoteTracker {
    fn track<'a>(&'a self, event: &'a Event) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            // The production remote trackers only implement test metrics.
            if matches!(event, Event::Test(_))
                && let Err(reason) = self.admit(event).await
            {
                self.record_rejection(reason);
            }
            Ok(())
        })
    }

    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async move {
            let admission = self.admission.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(RemoteAdmission { sender, shutdown }) = admission {
                // Set the one drain-and-close deadline before making the FIFO ready to finish.
                let _ = shutdown.send(Deadline::now() + self.limits.send);
                drop(sender);
                self.state.changed.notify_waiters();
            }
            self.task.join().await.map_err(operation_error)
        })
    }
}
#[derive(Clone)]
pub struct MappingTracker {
    clock: Arc<dyn WallClock>,
    internal: Arc<dyn TrackerInternal>,
}
impl MappingTracker {
    pub fn new(clock: Arc<dyn WallClock>, internal: Arc<dyn TrackerInternal>) -> Self {
        Self { clock, internal }
    }
}
impl Tracker for MappingTracker {
    fn device_connected<'a>(&'a self, pool: &'a DevicePoolId, device: &'a DeviceInfo) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let event = Event::DeviceConnected(DeviceConnectedEvent {
                instant: self
                    .clock
                    .instant("core.MappingTracker.deviceConnected")
                    .map_err(clock_error)?,
                pool_id: pool.clone(),
                device: device.clone(),
            });
            self.internal.track(&event).await
        })
    }
    fn device_disconnected<'a>(&'a self, pool: &'a DevicePoolId, device: &'a DeviceInfo) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let event = Event::DeviceDisconnected(DeviceDisconnectedEvent {
                instant: self
                    .clock
                    .instant("core.MappingTracker.deviceDisconnected")
                    .map_err(clock_error)?,
                pool_id: pool.clone(),
                device: device.clone(),
            });
            self.internal.track(&event).await
        })
    }
    fn device_provider_init<'a>(&'a self, serial: &'a str, start: Instant, finish: Instant) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let event = Event::DeviceProviderPreparing(DeviceProviderPreparingEvent {
                start,
                finish,
                serial_number: serial.to_string(),
            });
            self.internal.track(&event).await
        })
    }
    fn device_preparing<'a>(&'a self, serial: &'a str, start: Instant, finish: Instant) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let event = Event::DevicePreparing(DevicePreparingEvent {
                start,
                finish,
                serial_number: serial.to_string(),
            });
            self.internal.track(&event).await
        })
    }
    fn test<'a>(
        &'a self,
        pool: &'a DevicePoolId,
        device: &'a DeviceInfo,
        result: &'a TestResult,
        final_result: bool,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let event = Event::Test(TestEvent {
                instant: self.clock.instant("core.MappingTracker.test").map_err(clock_error)?,
                pool_id: pool.clone(),
                device: device.clone(),
                test_result: Arc::new(result.clone()),
                final_result,
            });
            self.internal.track(&event).await
        })
    }
}
async fn catch_tracking<T>(work: impl Future<Output = VendorResult<T>>) -> VendorResult<T> {
    AssertUnwindSafe(work)
        .catch_unwind()
        .await
        .unwrap_or_else(|payload| Err(VendorError::Other(crate::executor::panic_failure(payload))))
}

#[derive(Clone)]
pub struct DeviceTimingLog {
    log: ServiceLog,
    component: Component,
}
impl DeviceTimingLog {
    pub fn new(log: ServiceLog, registry: &Mutex<LogSettings>) -> Self {
        let component = registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .component(module_path!());
        Self { log, component }
    }

    pub fn warn(&self, operation: &str, serial: &str, failure: &Failure) {
        let message = format!("Could not record {operation} timing for device {serial}");
        (self.log)(&self.component, Level::WARN, &message, Some(failure));
    }
}

/// D51: losing a preparation metric must not turn successful device work into a failed attempt.
/// The body stays in the caller's future, outside both instrumentation boundaries.
pub async fn track_device_operation<T, Read, TrackEvent>(
    cancellation: &dyn CancellationSignal,
    read_clock: impl Fn() -> Read,
    body: impl Future<Output = VendorResult<T>>,
    track: impl FnOnce(Instant, Instant) -> TrackEvent,
    warn: impl FnOnce(&Failure),
) -> VendorResult<T>
where
    Read: Future<Output = VendorResult<Instant>>,
    TrackEvent: Future<Output = VendorResult<()>>,
{
    let start = catch_tracking(async { read_clock().await }).await;
    let result = body.await;
    let tracked = match start {
        Ok(start) if result.is_ok() => {
            catch_tracking(async {
                let finish = read_clock().await?;
                track(start, finish).await
            })
            .await
        }
        Ok(_) => return result,
        Err(error) => Err(error),
    };
    if let Err(error) = tracked {
        // The tracker's own cancellation is a metrics failure. The owner's request still applies.
        if result.is_ok()
            && let Some(reason) = cancellation.reason()
        {
            return Err(VendorError::Cancelled(reason));
        }
        let _ = catch_unwind(AssertUnwindSafe(|| warn(error.failure())));
    }
    result
}

#[derive(Clone)]
pub struct Track {
    io: Arc<dyn VendorRuntime>,
    clock: Arc<dyn WallClock>,
    delegates: Arc<Mutex<Vec<Arc<dyn Tracker>>>>,
    device_timing_log: Option<DeviceTimingLog>,
}
impl Track {
    pub fn new(io: Arc<dyn VendorRuntime>, clock: Arc<dyn WallClock>) -> Self {
        Self {
            io,
            clock,
            delegates: Arc::default(),
            device_timing_log: None,
        }
    }
    pub fn with_device_timing_log(mut self, log: DeviceTimingLog) -> Self {
        self.device_timing_log = Some(log);
        self
    }
    pub fn add(&self, tracker: Arc<dyn Tracker>) {
        self.delegates.lock().unwrap_or_else(|e| e.into_inner()).push(tracker);
    }
    /// The body remains in the caller's vendor future so cancellation drops it.
    pub async fn track_device_preparing<'a, T: Send + 'a>(
        &self,
        cancellation: &dyn CancellationSignal,
        serial: impl Fn() -> String + Send + Sync + 'a,
        body: VendorFuture<'a, T>,
    ) -> VendorResult<T> {
        self.prepare(cancellation, serial, body, false).await
    }
    pub async fn track_provider_device_preparing<'a, T: Send + 'a>(
        &self,
        cancellation: &dyn CancellationSignal,
        serial: impl Fn() -> String + Send + Sync + 'a,
        body: VendorFuture<'a, T>,
    ) -> VendorResult<T> {
        self.prepare(cancellation, serial, body, true).await
    }
    async fn prepare<'a, T: Send + 'a>(
        &self,
        cancellation: &dyn CancellationSignal,
        serial: impl Fn() -> String + Send + Sync + 'a,
        body: VendorFuture<'a, T>,
        provider: bool,
    ) -> VendorResult<T> {
        let (label, operation) = if provider {
            ("core.Track.trackProviderDevicePreparing", "setup")
        } else {
            ("core.Track.trackDevicePreparing", "preparation")
        };
        let serial = &serial;
        track_device_operation(
            cancellation,
            || {
                let clock = self.clock.clone();
                on_io(
                    self.io.as_ref(),
                    Box::pin(async move { clock.instant(label).map_err(clock_error) }),
                )
            },
            body,
            |start, finish| async move {
                // Preparation can update the serial used in its event.
                let serial = serial();
                if provider {
                    self.device_provider_init(&serial, start, finish).await
                } else {
                    self.device_preparing(&serial, start, finish).await
                }
            },
            |error| {
                if let Some(log) = &self.device_timing_log {
                    log.warn(operation, &serial(), error);
                }
            },
        )
        .await
    }
}
impl Tracker for Track {
    fn device_connected<'a>(&'a self, pool: &'a DevicePoolId, device: &'a DeviceInfo) -> VendorFuture<'a, ()> {
        let this = self.clone();
        let (pool, device) = (pool.clone(), device.clone());
        Box::pin(async move {
            on_io(
                self.io.as_ref(),
                Box::pin(async move {
                    let delegates = this.delegates.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    for delegate in delegates {
                        delegate.device_connected(&pool, &device).await?;
                    }
                    Ok(())
                }),
            )
            .await
        })
    }
    fn device_disconnected<'a>(&'a self, pool: &'a DevicePoolId, device: &'a DeviceInfo) -> VendorFuture<'a, ()> {
        let this = self.clone();
        let (pool, device) = (pool.clone(), device.clone());
        Box::pin(async move {
            on_io(
                self.io.as_ref(),
                Box::pin(async move {
                    let delegates = this.delegates.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    for delegate in delegates {
                        delegate.device_disconnected(&pool, &device).await?;
                    }
                    Ok(())
                }),
            )
            .await
        })
    }
    fn device_provider_init<'a>(&'a self, serial: &'a str, start: Instant, finish: Instant) -> VendorFuture<'a, ()> {
        let this = self.clone();
        let serial = serial.to_owned();
        Box::pin(async move {
            on_io(
                self.io.as_ref(),
                Box::pin(async move {
                    let delegates = this.delegates.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    for delegate in delegates {
                        delegate.device_provider_init(&serial, start, finish).await?;
                    }
                    Ok(())
                }),
            )
            .await
        })
    }
    fn device_preparing<'a>(&'a self, serial: &'a str, start: Instant, finish: Instant) -> VendorFuture<'a, ()> {
        let this = self.clone();
        let serial = serial.to_owned();
        Box::pin(async move {
            on_io(
                self.io.as_ref(),
                Box::pin(async move {
                    let delegates = this.delegates.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    for delegate in delegates {
                        delegate.device_preparing(&serial, start, finish).await?;
                    }
                    Ok(())
                }),
            )
            .await
        })
    }
    fn test<'a>(
        &'a self,
        pool: &'a DevicePoolId,
        device: &'a DeviceInfo,
        result: &'a TestResult,
        final_result: bool,
    ) -> VendorFuture<'a, ()> {
        let this = self.clone();
        let (pool, device, result) = (pool.clone(), device.clone(), result.clone());
        Box::pin(async move {
            on_io(
                self.io.as_ref(),
                Box::pin(async move {
                    let delegates = this.delegates.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    for delegate in delegates {
                        delegate.test(&pool, &device, &result, final_result).await?;
                    }
                    Ok(())
                }),
            )
            .await
        })
    }
}
/// Same allocation backs the run's event tracker and report lifecycle.
#[derive(Clone)]
pub struct EventPipeline {
    io: Arc<dyn VendorRuntime>,
    pub track: Arc<Track>,
    internal: Arc<dyn TrackerInternal>,
}
impl EventPipeline {
    pub fn tracker(&self) -> Arc<dyn Tracker> {
        self.track.clone()
    }
    pub fn reports(&self) -> Arc<dyn ReportLifecycle> {
        Arc::new(self.clone())
    }
}
impl ReportLifecycle for EventPipeline {
    fn close(&self) -> ServiceFuture<'_, ()> {
        let internal = self.internal.clone();
        Box::pin(async move {
            on_io(
                self.io.as_ref(),
                Box::pin(async move { internal.close().await.map_err(vendor_error) }),
            )
            .await
            .map_err(operation_error)
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReporterSlot {
    DeviceInfo,
    Billing,
    JUnit,
    Timeline,
    Raw,
    TestJson,
    Allure,
    Html,
    Stdout,
}
pub struct ReporterRegistration {
    pub slot: ReporterSlot,
    pub reporter: Arc<dyn Reporter>,
}
#[derive(Clone)]
pub enum BackendRequest {
    Influx { url: String },
    Graphite { port: Option<i32> },
}
/// Inner Err represents precisely the source's caught Exception boundary.
/// Outer Err retains uncaught failures and cancellation. Client implementations
/// own exception classification; ordinary track/close errors never use fallback.
/// Influx inner errors cover only createDb; subscriber construction errors are outer errors.
pub trait BackendFactory: Send + Sync {
    fn create(&self, request: BackendRequest) -> ServiceFuture<'_, Result<Option<Arc<dyn TrackerInternal>>, Failure>>;
    /// D51 limits used by the tracker for this backend.
    fn limits(&self) -> BackendLimits {
        BackendLimits::default()
    }
}
pub struct BackendRegistration {
    pub request: BackendRequest,
    pub factory: Arc<dyn BackendFactory>,
}
pub struct TrackerFactory {
    pub io: Arc<dyn VendorRuntime>,
    pub track: Arc<Track>,
    pub clock: Arc<dyn WallClock>,
    pub reporters: Vec<ReporterRegistration>,
    pub backend: Option<BackendRegistration>,
    pub logger: Component,
    pub log: ServiceLog,
}
impl TrackerFactory {
    pub async fn create(mut self) -> crate::error::OperationResult<EventPipeline> {
        let io = self.io.clone();
        on_io(
            io.as_ref(),
            Box::pin(async move {
                self.reporters.sort_by_key(|r| r.slot);
                let generator = ExecutionReportGenerator::new(self.reporters.into_iter().map(|r| r.reporter).collect())
                    .with_close_log(self.log.clone(), self.logger.clone());
                let mut delegates: Vec<Arc<dyn TrackerInternal>> = vec![Arc::new(generator)];
                if let Some(backend) = self.backend {
                    let request = match backend.request.clone() {
                        BackendRequest::Graphite { port } => BackendRequest::Graphite {
                            port: Some(port.unwrap_or(2003)),
                        },
                        r => r,
                    };
                    match backend.factory.create(request).await.map_err(vendor_error)? {
                        Ok(Some(tracker)) => delegates.push(Arc::new(RemoteTracker::new(
                            self.io.as_ref(),
                            BackendGuard::new(tracker, self.log.clone(), self.logger.clone(), backend.factory.limits()),
                            REMOTE_QUEUE_CAPACITY,
                        ))),
                        Ok(None) => {}
                        Err(failure) => match backend.request {
                            BackendRequest::Influx { url } => {
                                let mut text = String::from("Failed to reach InfluxDB at ");
                                text.push_str(&url);
                                let (log, logger) = (self.log.clone(), self.logger.clone());
                                blocking(self.io.as_ref(), move || {
                                    log(&logger, Level::WARN, &text, Some(&failure));
                                    Ok(())
                                })
                                .await?;
                            }
                            BackendRequest::Graphite { .. } => return Err(VendorError::Other(failure)),
                        },
                    }
                }
                let internal: Arc<dyn TrackerInternal> = Arc::new(
                    DelegatingTrackerInternal::new(delegates).with_close_log(self.log.clone(), self.logger.clone()),
                );
                self.track
                    .add(Arc::new(MappingTracker::new(self.clock, internal.clone())));
                Ok(EventPipeline {
                    io: self.io,
                    track: self.track,
                    internal,
                })
            }),
        )
        .await
        .map_err(operation_error)
    }
}

#[cfg(test)]
mod kind_tests {
    use super::*;
    use marathon_error::{AnalyticsKind, InfluxV1Kind, RetryDisposition};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Unused;
    impl TrackerInternalAdapter for Unused {}

    #[test]
    fn backend_deduplication_keeps_class_and_message_equivalence() {
        let logged = Arc::new(AtomicUsize::new(0));
        let count = logged.clone();
        let log: ServiceLog = Arc::new(move |_, _, _, _| {
            count.fetch_add(1, Ordering::Relaxed);
        });
        let guard = BackendGuard::new(
            Arc::new(Unused),
            log,
            marathon_logging::LogSettings::default().component("marathon_core::analytics::event_pipeline"),
            BackendLimits::default(),
        );
        let failure = |problem, retry, message: &str| {
            Failure::from_kind(
                FailureKind::Analytics(AnalyticsKind::InfluxV1 { problem, retry }),
                Some(message.into()),
            )
        };
        guard.failed(
            &failure(
                InfluxV1Kind::CacheMaxMemorySizeExceeded,
                RetryDisposition::Retry,
                "same",
            ),
            false,
            true,
        );
        guard.failed(
            &failure(
                InfluxV1Kind::CacheMaxMemorySizeExceeded,
                RetryDisposition::Never,
                "same",
            ),
            false,
            true,
        );
        assert_eq!(logged.load(Ordering::Relaxed), 1);
        guard.failed(
            &failure(InfluxV1Kind::AuthorizationFailed, RetryDisposition::Never, "same"),
            false,
            true,
        );
        guard.failed(
            &failure(InfluxV1Kind::AuthorizationFailed, RetryDisposition::Never, "different"),
            false,
            true,
        );
        assert_eq!(logged.load(Ordering::Relaxed), 3);
    }
}

#[cfg(test)]
mod remote_delivery_tests {
    use super::*;
    use crate::executor::vendor_on_current_runtime;
    use jcompat::jtime::instant_of;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::sync::watch;

    struct GatedBackend {
        entered: Notify,
        sends: AtomicUsize,
        released: watch::Receiver<bool>,
        panics: bool,
    }

    impl TrackerInternalAdapter for GatedBackend {
        fn test<'a>(&'a self, _: &'a TestEvent) -> VendorFuture<'a, ()> {
            Box::pin(async move {
                self.sends.fetch_add(1, Ordering::Relaxed);
                self.entered.notify_one();
                let _ = self.released.clone().wait_for(|ready| *ready).await;
                assert!(!self.panics, "injected delivery task panic");
                Ok(())
            })
        }
    }

    fn test_event() -> Event {
        let device = DeviceInfo {
            operating_system: OperatingSystem { version: "1".into() },
            serial_number: "serial-1".into(),
            model: "device".into(),
            manufacturer: "test".into(),
            network_state: NetworkState::Connected,
            device_features: vec![],
            healthy: true,
        };
        Event::Test(TestEvent {
            instant: instant_of(0, 0).unwrap(),
            pool_id: DevicePoolId { name: "pool".into() },
            device: device.clone(),
            test_result: Arc::new(TestResult {
                test: Test {
                    pkg: "pkg".into(),
                    clazz: "Clazz".into(),
                    method: "test".into(),
                    meta_properties: vec![],
                },
                device,
                test_batch_id: "batch".into(),
                status: TestStatus::Passed,
                start_time: 0,
                end_time: 1,
                stacktrace: None,
                attachments: vec![],
            }),
            final_result: true,
        })
    }

    type Warnings = Arc<Mutex<Vec<String>>>;

    async fn full_queue(panics: bool) -> (RemoteTracker, Arc<GatedBackend>, watch::Sender<bool>, Warnings) {
        let (release, released) = watch::channel(false);
        let backend = Arc::new(GatedBackend {
            entered: Notify::new(),
            sends: AtomicUsize::new(0),
            released,
            panics,
        });
        let warnings: Warnings = Arc::default();
        let recorded = warnings.clone();
        let guard = BackendGuard::new(
            backend.clone(),
            Arc::new(move |logger, level, text, _| {
                if logger.name() == "marathon_core::analytics::event_pipeline" {
                    assert_eq!(level, Level::WARN);
                    recorded.lock().unwrap().push(text.into());
                }
            }),
            marathon_logging::LogSettings::default().component("TrackerFactory"),
            BackendLimits::default(),
        );
        let tracker = RemoteTracker::new(&vendor_on_current_runtime(), guard, 1);
        let event = test_event();
        tracker.track(&event).await.unwrap();
        backend.entered.notified().await;
        tracker.track(&event).await.unwrap();
        (tracker, backend, release, warnings)
    }

    fn assert_rejections(warnings: &Warnings, first: &str, summary: &str) {
        assert_eq!(
            *warnings.lock().unwrap(),
            [
                format!("The analytics backend is dropping test metric events because {first}"),
                format!("The analytics backend dropped {summary}"),
            ],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn admission_expiry_starts_cool_off_and_releases_other_waiters_d86() {
        let (mut tracker, backend, release, warnings) = full_queue(false).await;
        // Give admission the earlier deadline so this exercises its own expiry, rather than the send's.
        tracker.limits.send = Duration::from_secs(10);
        let event = test_event();
        let start = Deadline::now();
        {
            let mut first = tracker.track(&event);
            assert!(futures_util::poll!(first.as_mut()).is_pending());
            tokio::time::advance(Duration::from_secs(5)).await;
            let mut second = tracker.track(&event);
            assert!(futures_util::poll!(second.as_mut()).is_pending());
            tokio::time::timeout(Duration::from_secs(60), async {
                first.await.unwrap();
                second.await.unwrap();
                tracker.track(&event).await.unwrap();
            })
            .await
            .unwrap();
        }
        assert_eq!(start.elapsed(), Duration::from_secs(10));
        assert_eq!(
            warnings.lock().unwrap().len(),
            1,
            "the first drop is logged during the run"
        );
        release.send_replace(true);
        tracker.close().await.unwrap();
        assert_eq!(backend.sends.load(Ordering::Relaxed), 1);
        drop(tracker);
        assert_rejections(
            &warnings,
            "the queue wait expired",
            "3 test metric events before admission: 1 wait expired, 2 cooling off, 0 admission closed, 0 delivery task stopped",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_permit_ready_at_the_admission_deadline_wins_d86() {
        let (mut tracker, backend, release, warnings) = full_queue(false).await;
        let limit = Duration::from_secs(10);
        tracker.limits.send = limit;
        let event = test_event();
        let started = Deadline::now();
        {
            let mut blocked = tracker.track(&event);
            assert!(futures_util::poll!(blocked.as_mut()).is_pending());
            // Leave the waiter unpolled until its timer has elapsed and the worker has freed a slot.
            sleep_until(started + limit).await;
            release.send_replace(true);
            backend.entered.notified().await;
            assert_eq!(started.elapsed(), limit);
            blocked.await.unwrap();
        }
        tracker.close().await.unwrap();
        assert_eq!(backend.sends.load(Ordering::Relaxed), 3);
        drop(tracker);
        assert!(warnings.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn closing_wakes_waiters_and_counts_submissions_even_after_close_returns_d86() {
        let (tracker, backend, release, warnings) = full_queue(false).await;
        let event = test_event();
        let start = Deadline::now();
        {
            let mut blocked = tracker.track(&event);
            assert!(futures_util::poll!(blocked.as_mut()).is_pending());
            let mut closing = tracker.close();
            assert!(futures_util::poll!(closing.as_mut()).is_pending());
            blocked.await.unwrap();
            tracker.track(&event).await.unwrap();
            release.send_replace(true);
            closing.await.unwrap();
        }
        tracker.track(&event).await.unwrap();
        tracker.close().await.unwrap();
        assert_eq!(start.elapsed(), Duration::ZERO);
        assert_eq!(backend.sends.load(Ordering::Relaxed), 2);
        assert_eq!(warnings.lock().unwrap().len(), 1);
        drop(tracker);
        assert_rejections(
            &warnings,
            "admission has closed",
            "3 test metric events before admission: 0 wait expired, 0 cooling off, 3 admission closed, 0 delivery task stopped",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn panicking_delivery_wakes_waiters_and_counts_later_submissions_d86() {
        let (tracker, _, release, warnings) = full_queue(true).await;
        let event = test_event();
        let start = Deadline::now();
        {
            let mut blocked = tracker.track(&event);
            assert!(futures_util::poll!(blocked.as_mut()).is_pending());
            release.send_replace(true);
            blocked.await.unwrap();
        }
        tracker.track(&event).await.unwrap();
        assert!(tracker.close().await.is_err());
        assert_eq!(start.elapsed(), Duration::ZERO);
        drop(tracker);
        assert_rejections(
            &warnings,
            "the delivery task stopped",
            "2 test metric events before admission: 0 wait expired, 0 cooling off, 0 admission closed, 2 delivery task stopped",
        );
    }

    struct SlowBackend {
        send: Duration,
        close_started: Arc<AtomicBool>,
    }

    impl TrackerInternalAdapter for SlowBackend {
        fn device_provider_preparing<'a>(&'a self, _: &'a DeviceProviderPreparingEvent) -> VendorFuture<'a, ()> {
            Box::pin(async move {
                tokio::time::sleep(self.send).await;
                Ok(())
            })
        }

        fn close(&self) -> ServiceFuture<'_, ()> {
            Box::pin(async move {
                self.close_started.store(true, Ordering::Relaxed);
                std::future::pending().await
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn draining_and_closing_use_one_deadline_d86() {
        let limit = Duration::from_secs(10);
        for (send, reaches_close) in [(7, true), (100, false)] {
            let close_started = Arc::new(AtomicBool::new(false));
            let backend = Arc::new(SlowBackend {
                send: Duration::from_secs(send),
                close_started: close_started.clone(),
            });
            let warns = Arc::new(Mutex::new(Vec::new()));
            let recorded = warns.clone();
            let guard = BackendGuard::new(
                backend.clone(),
                Arc::new(move |_, level, text, _| recorded.lock().unwrap().push((level, text.to_owned()))),
                marathon_logging::LogSettings::default().component(module_path!()),
                BackendLimits {
                    send: limit,
                    ..BackendLimits::default()
                },
            );
            {
                let mut state = guard.state.lock();
                state.failed = 2;
                state.skipped = 3;
            }
            let (sender, receiver) = mpsc::channel(1);
            let t = instant_of(0, 0).unwrap();
            sender
                .try_send(Event::DeviceProviderPreparing(DeviceProviderPreparingEvent {
                    serial_number: "test".into(),
                    start: t,
                    finish: t,
                }))
                .unwrap();
            let (close, request) = oneshot::channel();
            let started = Deadline::now();
            close.send(started + limit).unwrap();
            drop(sender);
            let logger = marathon_logging::LogSettings::default().component("marathon_core::analytics::event_pipeline");
            tokio::time::timeout(
                Duration::from_secs(60),
                RemoteTracker::deliver(guard, receiver, request, logger),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(started.elapsed(), limit);
            assert_eq!(close_started.load(Ordering::Relaxed), reaches_close);
            assert_eq!(Arc::strong_count(&backend), 1, "delivery released the backend");
            assert_eq!(
                *warns.lock().unwrap(),
                [
                    (
                        Level::WARN,
                        "The analytics backend lost test metrics: 2 results failed to send, and up to 3 were skipped"
                            .into(),
                    ),
                    (
                        Level::WARN,
                        "The analytics backend did not close in 10000 ms; test metrics it still held are lost".into(),
                    ),
                ],
            );
        }
    }
}
