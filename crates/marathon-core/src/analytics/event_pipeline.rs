//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/internal/pub/Track.kt
//! and analytics/MappingTracker.kt, TrackerFactory.kt, internal/sub/DelegatingTrackerInternal.kt,
//! TrackerInternal.kt, TrackerInternalAdapter.kt and ExecutionReportGenerator.kt.

use crate::{
    artifact_io::*,
    error::*,
    model::*,
    report::*,
    runtime::*,
    time::WallClock,
    wiring::{ReportLifecycle, Tracker},
};
use jcompat::{
    jtime::Instant,
    logging::{Level, Logger},
    writers::JavaString,
};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Runs inline on the I/O worker selected by Track or EventPipeline::close.
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
pub type CloseLog = Option<(ServiceLog, Logger)>;
/// D51: a failed close doesn't stop the later ones (Kotlin: it does, C11). Cancellation still stops
/// them, since the closes after it would only be cancelled too.
fn close_failed(first: &mut Option<OperationError>, error: OperationError, log: &CloseLog) -> OperationResult<()> {
    let OperationError::Failed(failure) = &error else {
        return Err(first.take().unwrap_or(error));
    };
    match (first.is_some(), log) {
        (false, _) => *first = Some(error),
        (true, Some((log, logger))) => log(logger, Level::Error, &"A later close also failed".into(), Some(failure)),
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
    pub fn with_close_log(mut self, log: ServiceLog, logger: Logger) -> Self {
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
    pub fn with_close_log(mut self, log: ServiceLog, logger: Logger) -> Self {
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
/// D51: when to leave a backend alone. A send takes the queue actor's time, so one that took this
/// long to fail would otherwise cost that much again for every test.
#[derive(Clone, Copy)]
pub struct BackendLimits {
    /// No send holds the queue longer than this, whatever the backend's own timeouts are, and no
    /// close holds the end of the run longer.
    pub send: Duration,
    pub slow_failure: Duration,
    pub cool_off: Duration,
}
impl Default for BackendLimits {
    /// 30 s sits above carbon's own limits (20 s in all) and well under the 60 s per step of the
    /// InfluxDB clients, whose writes Kotlin batches off the caller and which here can hold it.
    /// One second separates a refusal or a reset, which cost nothing, from a timeout or an
    /// unreachable host. Five minutes keeps the worst send to under a tenth of the queue's time,
    /// and the only cost is history for that window.
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
    kinds: HashSet<(JavaString, Option<JavaString>)>,
    skip_until: Option<tokio::time::Instant>,
    announced_skip: bool,
    failed: u64,
    skipped: u64,
}
/// D51: metrics are never worth a test run. Kotlin lets a tracker's exception end the queue actor,
/// and a dead queue leaves the run hanging (U168). A remote backend's failure is logged once per kind and
/// the run carries on. Its close still reports: `on_finish` keeps the exit code that a failed close has.
/// A close that runs out of time is only logged.
pub struct BackendGuard {
    inner: Arc<dyn TrackerInternal>,
    log: ServiceLog,
    logger: Logger,
    limits: BackendLimits,
    state: Mutex<GuardState>,
}
impl BackendGuard {
    pub fn new(inner: Arc<dyn TrackerInternal>, log: ServiceLog, logger: Logger, limits: BackendLimits) -> Self {
        Self {
            inner,
            log,
            logger,
            limits,
            state: Mutex::default(),
        }
    }
    fn state(&self) -> std::sync::MutexGuard<'_, GuardState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn failed(&self, failure: &Failure, slow: bool, counted: bool) {
        let (first_of_kind, announce) = {
            let mut state = self.state();
            state.failed += u64::from(counted);
            if slow {
                state.skip_until = Some(tokio::time::Instant::now() + self.limits.cool_off);
            }
            let first_of_kind = state.kinds.len() < LOGGED_KINDS
                && state.kinds.insert((failure.class.clone(), failure.message.clone()));
            let announce = slow && !std::mem::replace(&mut state.announced_skip, true);
            (first_of_kind, announce)
        };
        if first_of_kind {
            let text = "Cannot send test metrics to the analytics backend; the run carries on. Each kind of failure is logged once".into();
            (self.log)(&self.logger, Level::Warn, &text, Some(failure));
        }
        if announce {
            let text = format!(
                "The analytics backend failed slowly, so test metrics are skipped for {} s after each such failure",
                self.limits.cool_off.as_secs()
            );
            (self.log)(&self.logger, Level::Warn, &text.into(), None);
        }
    }
}
impl TrackerInternal for BackendGuard {
    fn track<'a>(&'a self, event: &'a Event) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let counted = matches!(event, Event::Test(_));
            {
                let mut state = self.state();
                if state
                    .skip_until
                    .is_some_and(|until| tokio::time::Instant::now() < until)
                {
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
                        &crate::executor::shared::failure("java.util.concurrent.TimeoutException", &text),
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
            let (failed, skipped) = {
                let state = self.state();
                (state.failed, state.skipped)
            };
            if failed + skipped > 0 {
                let text = format!(
                    "The analytics backend lost test metrics: {failed} results failed to send, and up to {skipped} were skipped"
                );
                (self.log)(&self.logger, Level::Warn, &text.into(), None);
            }
            // A close's last flush is a send too, and a server that trickles its answer never trips
            // the clients' idle timeouts. Dropping the close is how it stops: InfluxDB's client then
            // stops its worker. Kotlin waits for as long as the server keeps answering.
            match tokio::time::timeout(self.limits.send, self.inner.close()).await {
                Ok(result) => result,
                Err(_) => {
                    let text = format!(
                        "The analytics backend did not close in {} ms; test metrics it still held are lost",
                        self.limits.send.as_millis()
                    );
                    (self.log)(&self.logger, Level::Warn, &text.into(), None);
                    Ok(())
                }
            }
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
    fn device_provider_init<'a>(
        &'a self,
        serial: &'a JavaString,
        start: Instant,
        finish: Instant,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let event = Event::DeviceProviderPreparing(DeviceProviderPreparingEvent {
                start,
                finish,
                serial_number: serial.clone(),
            });
            self.internal.track(&event).await
        })
    }
    fn device_preparing<'a>(&'a self, serial: &'a JavaString, start: Instant, finish: Instant) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let event = Event::DevicePreparing(DevicePreparingEvent {
                start,
                finish,
                serial_number: serial.clone(),
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
#[derive(Clone)]
pub struct Track {
    io: Arc<dyn VendorRuntime>,
    clock: Arc<dyn WallClock>,
    delegates: Arc<Mutex<Vec<Arc<dyn Tracker>>>>,
}
impl Track {
    pub fn new(io: Arc<dyn VendorRuntime>, clock: Arc<dyn WallClock>) -> Self {
        Self {
            io,
            clock,
            delegates: Arc::default(),
        }
    }
    pub fn add(&self, tracker: Arc<dyn Tracker>) {
        self.delegates.lock().unwrap_or_else(|e| e.into_inner()).push(tracker);
    }
    /// The body remains in the caller's vendor future so cancellation drops it.
    pub async fn track_device_preparing<'a, T: Send + 'a>(
        &self,
        serial: impl FnOnce() -> JavaString + Send + 'a,
        body: VendorFuture<'a, T>,
    ) -> VendorResult<T> {
        self.prepare(serial, body, false).await
    }
    pub async fn track_provider_device_preparing<'a, T: Send + 'a>(
        &self,
        serial: impl FnOnce() -> JavaString + Send + 'a,
        body: VendorFuture<'a, T>,
    ) -> VendorResult<T> {
        self.prepare(serial, body, true).await
    }
    async fn prepare<'a, T: Send + 'a>(
        &self,
        serial: impl FnOnce() -> JavaString + Send + 'a,
        body: VendorFuture<'a, T>,
        provider: bool,
    ) -> VendorResult<T> {
        let label = if provider {
            "core.Track.trackProviderDevicePreparing"
        } else {
            "core.Track.trackDevicePreparing"
        };
        let clock = self.clock.clone();
        let start = on_io(
            self.io.as_ref(),
            Box::pin(async move { clock.instant(label).map_err(clock_error) }),
        )
        .await?;
        let result = body.await?;
        let clock = self.clock.clone();
        let finish = on_io(
            self.io.as_ref(),
            Box::pin(async move { clock.instant(label).map_err(clock_error) }),
        )
        .await?;
        // Android may replace the adb serial while preparation boots the device.
        let serial = serial();
        if provider {
            self.device_provider_init(&serial, start, finish).await?;
        } else {
            self.device_preparing(&serial, start, finish).await?;
        }
        Ok(result)
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
    fn device_provider_init<'a>(
        &'a self,
        serial: &'a JavaString,
        start: Instant,
        finish: Instant,
    ) -> VendorFuture<'a, ()> {
        let this = self.clone();
        let serial = serial.clone();
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
    fn device_preparing<'a>(&'a self, serial: &'a JavaString, start: Instant, finish: Instant) -> VendorFuture<'a, ()> {
        let this = self.clone();
        let serial = serial.clone();
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
    Influx { url: JavaString },
    Graphite { port: Option<i32> },
}
/// Inner Err represents precisely the source's caught Exception boundary.
/// Outer Err retains uncaught failures and cancellation. Client implementations
/// own exception classification; ordinary track/close errors never use fallback.
/// Influx inner errors cover only createDb; subscriber construction errors are outer errors.
pub trait BackendFactory: Send + Sync {
    fn create(&self, request: BackendRequest) -> ServiceFuture<'_, Result<Option<Arc<dyn TrackerInternal>>, Failure>>;
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
    pub logger: Logger,
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
                        Ok(Some(tracker)) => delegates.push(Arc::new(BackendGuard::new(
                            tracker,
                            self.log.clone(),
                            self.logger.clone(),
                            BackendLimits::default(),
                        ))),
                        Ok(None) => {}
                        Err(failure) => match backend.request {
                            BackendRequest::Influx { url } => {
                                let mut text = JavaString::from("Failed to reach InfluxDB at ");
                                text.append(url);
                                let (log, logger) = (self.log.clone(), self.logger.clone());
                                blocking(self.io.as_ref(), move || {
                                    log(&logger, Level::Warn, &text, Some(&failure));
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
