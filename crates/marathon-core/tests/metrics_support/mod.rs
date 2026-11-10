//! Port of core/src/test/kotlin/com/malinskiy/marathon/analytics/metrics/remote/influx/MetricsProviderImplTest.kt (scripted mocks).
#![allow(dead_code)]
use marathon_logging::LogSettings;

use super::strategy_support::*;
use jcompat::jtime::duration_of;
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
use marathon_error::FailureKind;
use std::{
    collections::VecDeque,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    },
};

pub fn run(future: impl Future<Output = ()>) {
    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime.block_on(future),
        Err(error) => panic!("test runtime: {error}"),
    }
}
/// A retry delay returns at once. A refresh delay (D34) waits for [`Clock::release_refreshes`]:
/// time stands still unless the test moves it past a refresh, so a test that never does sees
/// the queries of a run in which no refresh is due yet, as the JVM recordings do.
pub struct Clock {
    pub delays: Mutex<Vec<i64>>,
    pub nanos: AtomicI64,
    pub labels: Mutex<Vec<&'static str>>,
    /// Refresh delays that have started, whether or not they have ended.
    pub refresh_delays: Mutex<Vec<i64>>,
    refreshes: tokio::sync::Semaphore,
    refresh_started: tokio::sync::Notify,
    /// How many times `close` has started waiting for a cancelled refresh.
    pub close_bounds: AtomicUsize,
    close_bound: tokio::sync::Semaphore,
}
const RETRY_DELAY_MILLIS: i64 = 50;
/// `close`'s wait for a cancelled refresh, which only [`Clock::expire_close_bound`] ends. Its value
/// is not in the refresh schedule, so this clock can tell the two apart.
const CLOSE_BOUND_MILLIS: i64 = 3_000;
impl Default for Clock {
    fn default() -> Self {
        Self {
            delays: Mutex::default(),
            nanos: AtomicI64::default(),
            labels: Mutex::default(),
            refresh_delays: Mutex::default(),
            refreshes: tokio::sync::Semaphore::new(0),
            refresh_started: tokio::sync::Notify::new(),
            close_bounds: AtomicUsize::new(0),
            close_bound: tokio::sync::Semaphore::new(0),
        }
    }
}
impl Clock {
    pub fn release_refreshes(&self, rounds: usize) {
        self.refreshes.add_permits(rounds);
    }
    pub fn expire_close_bound(&self) {
        self.close_bound.add_permits(1);
    }
    /// Waits until `count` refresh delays have started.
    pub async fn refresh_delay_started(&self, count: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                let started = self.refresh_started.notified();
                if self.refresh_delays.lock().unwrap().len() >= count {
                    return;
                }
                started.await;
            }
        })
        .await
        .expect("the refresh delay never started");
    }
}
impl VendorClock for Clock {
    fn delay(&self, millis: i64, cancellation: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if let Some(reason) = cancellation.reason() {
                return Err(VendorError::Cancelled(reason));
            }
            if millis == CLOSE_BOUND_MILLIS {
                self.close_bounds.fetch_add(1, Ordering::SeqCst);
                let permit = self.close_bound.acquire();
                let cancelled = cancellation.cancelled();
                tokio::select! {
                    permit = permit => permit.expect("never closed").forget(),
                    reason = cancelled => return Err(VendorError::Cancelled(reason)),
                }
                return Ok(());
            }
            if millis != RETRY_DELAY_MILLIS {
                self.refresh_delays.lock().unwrap().push(millis);
                self.refresh_started.notify_waiters();
                let permit = self.refreshes.acquire();
                let cancelled = cancellation.cancelled();
                tokio::select! {
                    permit = permit => permit.expect("never closed").forget(),
                    reason = cancelled => return Err(VendorError::Cancelled(reason)),
                }
                self.nanos.fetch_add(millis * 1_000_000, Ordering::SeqCst);
                return Ok(());
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
/// Tasks on the test's own tokio runtime, with `cancel` honoured as in production.
#[derive(Default)]
pub struct Runtime {
    pub running: Arc<AtomicUsize>,
}
struct Task {
    signal: Arc<Signal>,
    done: tokio::sync::watch::Sender<bool>,
}
impl VendorTask for Task {
    fn cancel(&self, reason: Cancellation) {
        self.signal.cancel(reason);
    }
    fn join(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let _ = self.done.subscribe().wait_for(|done| *done).await;
            Ok(())
        })
    }
}
impl VendorRuntime for Runtime {
    fn spawn_cancellable(&self, body: VendorTaskBody) -> Arc<dyn VendorTask> {
        let task = Arc::new(Task {
            signal: Signal::new(),
            done: tokio::sync::watch::channel(false).0,
        });
        let output = task.clone();
        let running = self.running.clone();
        running.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            let _ = body(output.signal.clone()).await;
            running.fetch_sub(1, Ordering::SeqCst);
            output.done.send_replace(true);
        });
        task
    }
    fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn VendorTask> {
        self.spawn_cancellable(Box::new(move |_| body))
    }
    fn spawn_blocking(&self, _: Box<dyn FnOnce() -> VendorResult<()> + Send>) -> Arc<dyn VendorTask> {
        unreachable!("the metrics provider does not block")
    }
}
pub struct Harness {
    pub context: MetricsContext,
    pub logs: LogQueue,
    pub clock: Arc<Clock>,
    pub running: Arc<AtomicUsize>,
}
impl Harness {
    pub fn new() -> Self {
        let logs = LogQueue::default();
        let clock = Arc::new(Clock::default());
        let runtime = Runtime::default();
        let running = runtime.running.clone();
        let context = MetricsContext {
            clock: clock.clone(),
            vendor: Arc::new(runtime),
            monotonic: clock.clone(),
            cancellation: Signal::new(),
            registry: Arc::new(Mutex::new(LogSettings::default())),
            logs: logs.sender(),
            stamp: Arc::new(|| (0, 0)),
        };
        Self {
            context,
            logs,
            clock,
            running,
        }
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
            return Err(failure(FailureKind::Runtime, "unreadable").into());
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
                Err(failure(FailureKind::Io, "close").into())
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
