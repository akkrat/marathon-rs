//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/NoOpMetricsProvider.kt
use super::*;
use std::collections::BTreeSet;

pub struct NoOpMetricsProvider {
    pub backend_unavailable: bool,
    context: MetricsContext,
}
impl NoOpMetricsProvider {
    pub fn new(backend_unavailable: bool, context: MetricsContext) -> OperationResult<Self> {
        context.register("NoOpMetricsProvider")?;
        Ok(Self {
            backend_unavailable,
            context,
        })
    }
}
impl MetricsLookup for NoOpMetricsProvider {
    fn success_rate(&self, _: &Test, _: Instant) -> f64 {
        0.0
    }
    fn execution_time(&self, _: &Test, _: f64, _: Instant) -> f64 {
        0.0
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
            let (level, reason) = if self.backend_unavailable {
                (Level::Warn, "the analytics backend could not be set up")
            } else {
                (Level::Info, "analytics is disabled")
            };
            let mut message = String::from("Execution time history for ");
            message.push_str(&usage.to_owned());
            message.push_str(&format!(
                ": 0 of {count} tests have it, {reason}, so every test is expected to take 0 ms"
            ));
            self.context.log("NoOpMetricsProvider", level, message, None);
            Ok(())
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
