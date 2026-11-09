//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/Analytics.kt
//! and analytics/external/AnalyticsFactory.kt.
use marathon_error::FailureKind;
mod coverage;
mod event_pipeline;
pub use event_pipeline::*;
mod factory;
mod noop;
mod provider;
pub mod remote;
pub use coverage::report_execution_time_coverage;
pub use factory::{AnalyticsFactory, MetricsProviderFactory, MetricsSetup};
pub use noop::NoOpMetricsProvider;
pub use provider::MetricsProviderImpl;

use crate::{
    error::{Failure, OperationResult},
    executor::shared::failure,
    logging::{LogRecord, LogSender},
    model::{Test, TestResult},
    runtime::{CancellationSignal, ServiceFuture, VendorClock, VendorRuntime},
    strategy::{MetricsKey, MetricsLookup},
    time::MonotonicClock,
    wiring::MetricsProvider,
};
use jcompat::{
    jtime::Instant,
    logging::{Level, MarathonLogging},
};
use std::sync::{Arc, Mutex};

/// Built off core, before configureLogging. Stamp reads must be nonblocking;
/// supply the run's scheduler/wall clocks (or replay clocks), in milliseconds.
#[derive(Clone)]
pub struct MetricsContext {
    pub clock: Arc<dyn VendorClock>,
    /// Where a failed history query's refresh runs (D34).
    pub vendor: Arc<dyn VendorRuntime>,
    pub monotonic: Arc<dyn MonotonicClock>,
    pub cancellation: Arc<dyn CancellationSignal>,
    pub registry: Arc<Mutex<MarathonLogging>>,
    pub logs: LogSender,
    pub stamp: Arc<dyn Fn() -> (i64, i64) + Send + Sync>,
}
impl MetricsContext {
    fn register(&self, name: &str) -> OperationResult<()> {
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| failure(FailureKind::IllegalState, "Poisoned logging registry"))?;
        registry.logger(name, None);
        Ok(())
    }
    fn timings(&self) -> OperationResult<()> {
        self.registry
            .lock()
            .map_err(|_| failure(FailureKind::IllegalState, "Poisoned logging registry"))?
            .timings();
        Ok(())
    }
    fn log(&self, logger: &str, level: Level, message: String, cause: Option<Failure>) {
        let (at_millis, wall_millis) = (self.stamp)();
        self.logs.push(LogRecord {
            logger: logger.to_owned(),
            at_millis,
            wall_millis,
            level,
            message,
            cause,
        });
    }
}

/// A run-wide provider. Share this Arc across Marathon and every pool; queue
/// construction awaits prefetch off core, then uses the in-memory view, which the
/// strategies pin when they read it (D34).
pub struct Analytics {
    backend: Backend,
}
enum Backend {
    Remote(Arc<MetricsProviderImpl>),
    NoOp(Arc<NoOpMetricsProvider>),
    #[cfg(feature = "test-support")]
    Lookup(Arc<dyn MetricsLookup>),
}
impl Analytics {
    pub fn new(provider: MetricsProviderImpl) -> Self {
        Self {
            backend: Backend::Remote(Arc::new(provider)),
        }
    }
    pub fn noop(provider: NoOpMetricsProvider) -> Self {
        Self {
            backend: Backend::NoOp(Arc::new(provider)),
        }
    }
    #[cfg(feature = "test-support")]
    pub fn from_lookup(lookup: Arc<dyn MetricsLookup>) -> Self {
        Self {
            backend: Backend::Lookup(lookup),
        }
    }
    #[cfg(feature = "test-support")]
    pub fn is_noop(&self) -> bool {
        matches!(self.backend, Backend::NoOp(_))
    }
    pub async fn prefetch(&self, keys: &[MetricsKey]) -> OperationResult<()> {
        match &self.backend {
            Backend::Remote(provider) => provider.prefetch(keys).await,
            _ => Ok(()),
        }
    }
    pub fn lookup(&self) -> Arc<dyn MetricsLookup> {
        match &self.backend {
            Backend::Remote(provider) => provider.clone(),
            Backend::NoOp(provider) => provider.clone(),
            #[cfg(feature = "test-support")]
            Backend::Lookup(provider) => provider.clone(),
        }
    }
}
impl MetricsProvider for Analytics {
    fn success_rate<'a>(&'a self, test: &'a Test, limit: Instant) -> ServiceFuture<'a, f64> {
        Box::pin(async move {
            self.prefetch(&[MetricsKey::SuccessRate { limit }]).await?;
            Ok(self.lookup().success_rate(test, limit))
        })
    }
    fn execution_time<'a>(&'a self, test: &'a Test, percentile: f64, limit: Instant) -> ServiceFuture<'a, f64> {
        match &self.backend {
            Backend::Remote(provider) => MetricsProvider::execution_time(provider.as_ref(), test, percentile, limit),
            Backend::NoOp(provider) => MetricsProvider::execution_time(provider.as_ref(), test, percentile, limit),
            #[cfg(feature = "test-support")]
            Backend::Lookup(provider) => Box::pin(async move { Ok(provider.execution_time(test, percentile, limit)) }),
        }
    }
    fn observe_execution_times<'a>(&'a self, results: &'a [TestResult]) -> ServiceFuture<'a, ()> {
        match &self.backend {
            Backend::Remote(provider) => provider.observe_execution_times(results),
            _ => Box::pin(async { Ok(()) }),
        }
    }
    fn report_execution_time_coverage<'a>(
        &'a self,
        tests: &'a [Test],
        percentile: f64,
        limit: Instant,
        usage: &'a str,
    ) -> ServiceFuture<'a, ()> {
        match &self.backend {
            Backend::Remote(provider) => provider.report_execution_time_coverage(tests, percentile, limit, usage),
            Backend::NoOp(provider) => provider.report_execution_time_coverage(tests, percentile, limit, usage),
            #[cfg(feature = "test-support")]
            Backend::Lookup(_) => Box::pin(async { Ok(()) }),
        }
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        match &self.backend {
            Backend::Remote(provider) => provider.close(),
            _ => Box::pin(async { Ok(()) }),
        }
    }
}
