//! Port of core/src/main/kotlin/com/malinskiy/marathon/di/Modules.kt
//! and analytics/internal/pub/Tracker.kt, analytics/external/MetricsProvider.kt.
//!
//! Per-run explicit services. These declarations do not perform Koin's lazy
//! construction side effects; WP 3.10 controls when each service is used.

use marathon_logging::LogSettings;

use crate::{
    model::{DeviceInfo, DevicePoolId, Test, TestResult},
    runtime::{CoreRuntime, ServiceFuture, TimeoutScope, VendorClock, VendorFuture, VendorRuntime},
    time::{ReplayBarriers, ReplayHooks, SchedulerClock, Timer},
    vendor::{DeviceProvider, LogConfigurator, TestBundleIdentifier, TestParser},
};
use jcompat::{jtime::Instant, };
use marathon_config::Configuration;
use std::{
    rc::Rc,
    sync::{Arc, Mutex},
};

/// Calls may perform network queries; always await off core. Caching/observed
/// duration ordering and fallback belong to WP 3.4, not this interface.
/// Query backend failures are absorbed with Kotlin defaults/logs; unhandled
/// failures (including close) use OperationError::Failed, cancellation stays typed.
pub trait MetricsProvider: Send + Sync {
    fn success_rate<'a>(&'a self, test: &'a Test, limit: Instant) -> ServiceFuture<'a, f64>;
    fn execution_time<'a>(&'a self, test: &'a Test, percentile: f64, limit: Instant) -> ServiceFuture<'a, f64>;
    fn observe_execution_times<'a>(&'a self, _results: &'a [TestResult]) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn report_execution_time_coverage<'a>(
        &'a self,
        _tests: &'a [Test],
        _percentile: f64,
        _limit: Instant,
        _usage: &'a str,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn close(&self) -> ServiceFuture<'_, ()>;
}
/// Sequential event delivery can reach a network backend (GraphiteTracker).
/// Await each delegate off core in registration order; do not fire-and-forget.
pub trait Tracker: Send + Sync {
    fn device_connected<'a>(&'a self, pool: &'a DevicePoolId, device: &'a DeviceInfo) -> VendorFuture<'a, ()>;
    fn device_disconnected<'a>(&'a self, pool: &'a DevicePoolId, device: &'a DeviceInfo) -> VendorFuture<'a, ()>;
    fn device_provider_init<'a>(&'a self, serial: &'a str, start: Instant, finish: Instant) -> VendorFuture<'a, ()>;
    fn device_preparing<'a>(&'a self, serial: &'a str, start: Instant, finish: Instant) -> VendorFuture<'a, ()>;
    fn test<'a>(
        &'a self,
        pool: &'a DevicePoolId,
        device: &'a DeviceInfo,
        result: &'a TestResult,
        final_result: bool,
    ) -> VendorFuture<'a, ()>;
}
/// Reporter errors propagate as OperationError::Failed; close short-circuits.
pub trait ReportLifecycle: Send + Sync {
    fn close(&self) -> ServiceFuture<'_, ()>;
}
/// D1's implementation sends nothing, while preserving onFinish's final slot.
pub trait UsageLifecycle: Send + Sync {
    fn close(&self) -> VendorFuture<'_, ()>;
}
pub trait ParseOutput: Send + Sync {
    fn execute<'a>(
        &'a self,
        tests: &'a [Test],
        flaky_tests: &'a [Test],
        output_file_name: Option<&'a str>,
    ) -> VendorFuture<'a, ()>;
}
pub use crate::output::OutputFiles;
#[derive(Clone)]
pub struct VendorDependencies {
    pub logs: crate::logging::LogSender,
    pub wall: crate::logging::WallSource,
    pub configuration: Arc<Configuration>,
    pub io: Arc<dyn VendorRuntime>,
    pub clock: Arc<dyn VendorClock>,
    pub logging: Arc<Mutex<LogSettings>>,
    /// Same output queue as `logs`, with scheduler and wall timestamps captured by
    /// construction. Keep this adapter: `wall` alone cannot reproduce Android
    /// service records' scheduler timestamps, and VendorClock has no now source.
    /// Both paths use the run console and its time zone when drained.
    pub log: crate::artifact_io::ServiceLog,
    pub hooks: Arc<dyn ReplayHooks>,
    pub timer: Arc<dyn Timer>,
    pub tracker: Arc<dyn Tracker>,
    pub files: Arc<dyn OutputFiles>,
    pub barriers: Arc<dyn ReplayBarriers>,
}
/// Fully resolved inputs for Marathon; vendor options have been resolved by CLI.
/// The report lifecycle and event tracker must belong to the same pipeline.
pub struct RunDependencies {
    pub vendor: VendorDependencies,
    pub provider: Arc<dyn DeviceProvider>,
    pub parser: TestParser,
    pub bundle_identifier: Option<Arc<dyn TestBundleIdentifier>>,
    pub log_configurator: Arc<dyn LogConfigurator>,
    pub analytics: Arc<crate::analytics::Analytics>,
    pub reports: Arc<dyn ReportLifecycle>,
    pub usage: Arc<dyn UsageLifecycle>,
    pub parse_output: Arc<dyn ParseOutput>,
    pub core: Rc<dyn CoreRuntime>,
    pub scheduler_clock: Rc<dyn SchedulerClock>,
    pub timeouts: Rc<dyn TimeoutScope>,
}
