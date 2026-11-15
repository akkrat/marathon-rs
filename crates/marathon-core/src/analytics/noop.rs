//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/NoOpMetricsProvider.kt
use super::*;
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicBool, Ordering},
};

pub struct NoOpMetricsProvider {
    pub backend_unavailable: bool,
    context: MetricsContext,
    /// D71: the run's one warning that there is no success-rate history.
    warned_unavailable: AtomicBool,
}
impl NoOpMetricsProvider {
    pub fn new(backend_unavailable: bool, context: MetricsContext) -> OperationResult<Self> {
        context.register("marathon_core::analytics::noop")?;
        Ok(Self {
            backend_unavailable,
            context,
            warned_unavailable: AtomicBool::new(false),
        })
    }
    fn reason(&self) -> &'static str {
        if self.backend_unavailable {
            "the analytics backend could not be set up"
        } else {
            "analytics is disabled"
        }
    }
}
impl MetricsLookup for NoOpMetricsProvider {
    fn success_rate(&self, _: &Test, _: Instant) -> f64 {
        0.0
    }
    fn execution_time(&self, _: &Test, _: f64, _: Instant) -> f64 {
        0.0
    }
    /// D71: the 0 above is a placeholder, not history.
    fn success_rate_available(&self, _: Instant, consequence: &str) -> bool {
        if !self.warned_unavailable.swap(true, Ordering::Relaxed) {
            let message = format!("Test history unavailable ({}); {consequence}", self.reason());
            self.context
                .log("marathon_core::analytics::noop", Level::WARN, message, None);
        }
        false
    }
}
impl MetricsProvider for NoOpMetricsProvider {
    fn success_rate<'a>(&'a self, _: &'a Test, _: Instant) -> ServiceFuture<'a, f64> {
        Box::pin(async { Ok(0.0) })
    }
    fn execution_time<'a>(&'a self, _: &'a Test, _: f64, _: Instant) -> ServiceFuture<'a, f64> {
        Box::pin(async { Ok(0.0) })
    }
    fn report_execution_time_coverage<'a>(
        &'a self,
        tests: &'a [Test],
        _: f64,
        _: Instant,
        usage: &'a str,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            let count = tests.iter().map(Test::to_safe_test_name).collect::<BTreeSet<_>>().len();
            if count == 0 {
                return Ok(());
            }
            let level = if self.backend_unavailable {
                Level::WARN
            } else {
                Level::INFO
            };
            let reason = self.reason();
            let mut message = String::from("Execution time history for ");
            message.push_str(usage);
            message.push_str(&format!(
                ": 0 of {count} tests have it, {reason}, so every test is expected to take 0 ms"
            ));
            self.context.log("marathon_core::analytics::noop", level, message, None);
            Ok(())
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
