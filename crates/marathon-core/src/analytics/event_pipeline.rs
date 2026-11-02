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
use std::sync::{Arc, Mutex};

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
#[derive(Clone)]
pub struct DelegatingTrackerInternal {
    delegates: Vec<Arc<dyn TrackerInternal>>,
}
impl DelegatingTrackerInternal {
    pub fn new(delegates: Vec<Arc<dyn TrackerInternal>>) -> Self {
        Self { delegates }
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
            for delegate in &self.delegates {
                delegate.close().await?;
            }
            Ok(())
        })
    }
}
#[derive(Clone)]
pub struct ExecutionReportGenerator {
    events: Arc<Mutex<Vec<Event>>>,
    reporters: Vec<Arc<dyn Reporter>>,
}
impl ExecutionReportGenerator {
    pub fn new(reporters: Vec<Arc<dyn Reporter>>) -> Self {
        Self {
            events: Arc::default(),
            reporters,
        }
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
            for reporter in &self.reporters {
                reporter.generate(&report).await?;
            }
            Ok(())
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
                let generator = ExecutionReportGenerator::new(self.reporters.into_iter().map(|r| r.reporter).collect());
                let mut delegates: Vec<Arc<dyn TrackerInternal>> = vec![Arc::new(generator)];
                if let Some(backend) = self.backend {
                    let request = match backend.request.clone() {
                        BackendRequest::Graphite { port } => BackendRequest::Graphite {
                            port: Some(port.unwrap_or(2003)),
                        },
                        r => r,
                    };
                    match backend.factory.create(request).await.map_err(vendor_error)? {
                        Ok(Some(tracker)) => delegates.push(tracker),
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
                let internal: Arc<dyn TrackerInternal> = Arc::new(DelegatingTrackerInternal::new(delegates));
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
