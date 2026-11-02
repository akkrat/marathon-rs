//! Port of core/src/test/kotlin/com/malinskiy/marathon/analytics/metrics/remote/influx/MetricsProviderImplTest.kt (scripted mocks).
#![allow(dead_code)]
use super::strategy_support::*;
use jcompat::{jtime::duration_of, logging::MarathonLogging};
use marathon_config::Defaults;
use marathon_core::{
    analytics::{remote::*, *},
    error::*,
    executor::{Signal, shared::failure},
    logging::LogQueue,
    model::*,
    runtime::*,
    strategy::MetricsKey,
    time::*,
};
use std::{
    collections::VecDeque,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
};

pub fn run(future: impl Future<Output = ()>) {
    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime.block_on(future),
        Err(error) => panic!("test runtime: {error}"),
    }
}
#[derive(Default)]
pub struct Clock {
    pub delays: Mutex<Vec<i64>>,
    pub nanos: AtomicI64,
    pub labels: Mutex<Vec<&'static str>>,
}
impl VendorClock for Clock {
    fn delay(&self, millis: i64, cancellation: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if let Some(reason) = cancellation.reason() {
                return Err(VendorError::Cancelled(reason));
            }
            self.delays.lock().unwrap().push(millis);
            self.nanos.fetch_add(millis * 1_000_000, Ordering::SeqCst);
            Ok(())
        })
    }
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        _: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, ()> {
        body(parent)
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        _: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, Option<()>> {
        Box::pin(async { body(parent).await.map(Some) })
    }
}
impl MonotonicClock for Clock {
    fn nanos(&self, label: &'static str) -> ClockResult<i64> {
        self.labels.lock().unwrap().push(label);
        Ok(self.nanos.load(Ordering::SeqCst))
    }
}
pub struct Harness {
    pub context: MetricsContext,
    pub logs: LogQueue,
    pub clock: Arc<Clock>,
}
impl Harness {
    pub fn new() -> Self {
        let logs = LogQueue::default();
        let clock = Arc::new(Clock::default());
        let context = MetricsContext {
            clock: clock.clone(),
            monotonic: clock.clone(),
            cancellation: Signal::new(),
            registry: Arc::new(Mutex::new(MarathonLogging::default())),
            logs: logs.sender(),
            stamp: Arc::new(|| (0, 0)),
        };
        Self { context, logs, clock }
    }
    pub fn provider(&self, source: Arc<dyn RemoteDataSource>, millis: i64, min: Option<i32>) -> Analytics {
        Analytics::new(MetricsProviderImpl::new(source, defaults(millis, min), self.context.clone()).unwrap())
    }
}
pub fn defaults(millis: i64, min: Option<i32>) -> Defaults {
    Defaults {
        success_rate: 0.0,
        duration: duration_of(0, millis * 1_000_000).unwrap(),
        observed_duration_min_tests: min,
    }
}
#[derive(Default)]
pub struct Source {
    pub calls: Mutex<Vec<MetricsKey>>,
    pub times: Mutex<Vec<ExecutionTime>>,
    pub rates: Mutex<Vec<SuccessRate>>,
    pub errors: Mutex<VecDeque<Option<OperationError>>>,
    pub failure: bool,
    pub close_error: bool,
}
impl Source {
    fn check(&self, key: MetricsKey) -> OperationResult<()> {
        self.calls.lock().unwrap().push(key);
        if let Some(Some(error)) = self.errors.lock().unwrap().pop_front() {
            return Err(error);
        }
        if self.failure {
            return Err(failure("java.lang.RuntimeException", "unreadable").into());
        }
        Ok(())
    }
}
impl RemoteDataSource for Source {
    fn request_all_success_rates(&self, limit: jcompat::jtime::Instant) -> ServiceFuture<'_, Vec<SuccessRate>> {
        Box::pin(async move {
            self.check(MetricsKey::SuccessRate { limit })?;
            Ok(self.rates.lock().unwrap().clone())
        })
    }
    fn request_all_execution_times(
        &self,
        percentile: f64,
        limit: jcompat::jtime::Instant,
    ) -> ServiceFuture<'_, Vec<ExecutionTime>> {
        Box::pin(async move {
            self.check(MetricsKey::ExecutionTime { percentile, limit })?;
            Ok(self.times.lock().unwrap().clone())
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async {
            if self.close_error {
                Err(failure("java.io.IOException", "close").into())
            } else {
                Ok(())
            }
        })
    }
}
pub fn result(test: &Test, status: TestStatus, duration: i64) -> TestResult {
    let mut result = generate_test_result(test, status);
    result.start_time = 1000;
    result.end_time = 1000_i64.wrapping_add(duration);
    result
}
pub fn time_row(test: &Test, value: f64) -> ExecutionTime {
    ExecutionTime {
        test_name: test.to_safe_test_name(),
        percentile: value,
    }
}
pub fn rate_row(test: &Test, value: f64) -> SuccessRate {
    SuccessRate {
        test_name: test.to_safe_test_name(),
        mean: value,
    }
}
pub fn key(p: f64) -> MetricsKey {
    MetricsKey::ExecutionTime {
        percentile: p,
        limit: limit(),
    }
}
