//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/MetricsProviderImpl.kt
//! (D34 through the real clients: a failed history query is not final; Kotlin caches it for the
//! whole run).
use marathon_logging::Level;

use marathon_error::{FailureKind, InjectedKind};
mod support;
use jcompat::{jtime::Instant, };
use marathon_analytics::AnalyticsClients;
use marathon_config::{AnalyticsConfiguration, GraphiteConfiguration};
use marathon_core::{
    analytics::{
        remote::{ExecutionTime, RemoteDataSource, SuccessRate},
        *,
    },
    error::*,
    executor::shared::failure,
    logging::LogRecord,
    runtime::*,
    strategy::MetricsLookup,
    wiring::MetricsProvider,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};
use support::*;

const RETRY_MILLIS: i64 = 50;
const INFLUX_BATCH_TIMER_MILLIS: i64 = 1_000;
const CLOSE_BOUND_MILLIS: i64 = 3_000;
/// The default duration of [`defaults`], which answers until a refresh lands.
const DEFAULT_MILLIS: f64 = 300000.0;

/// A first query's retries pass at once, and a refresh delay lasts until the test advances the
/// clock past it. The InfluxDB client's batch timer never fires, since nothing here is written.
/// Nor does `close`'s bound: the test sees an answer as soon as it is published, so `close` can
/// find the refresh still ending, and these clients always stop within it.
struct Clock {
    refresh_delays: Mutex<Vec<i64>>,
    advances: tokio::sync::Semaphore,
}
impl Clock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            refresh_delays: Mutex::default(),
            advances: tokio::sync::Semaphore::new(0),
        })
    }
    fn advance_past_a_refresh_delay(&self) {
        self.advances.add_permits(1);
    }
    async fn refresh_delays_started(&self, count: usize) {
        until(|| self.refresh_delays.lock().unwrap().len() == count).await;
    }
}
impl VendorClock for Clock {
    fn delay(&self, millis: i64, cancellation: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            match millis {
                RETRY_MILLIS => return Ok(()),
                INFLUX_BATCH_TIMER_MILLIS | CLOSE_BOUND_MILLIS => {
                    return Err(VendorError::Cancelled(cancellation.cancelled().await));
                }
                _ => self.refresh_delays.lock().unwrap().push(millis),
            }
            tokio::select! {
                reason = cancellation.cancelled() => Err(VendorError::Cancelled(reason)),
                permit = self.advances.acquire() => {
                    permit.map_err(|e| VendorError::Other(failure(FailureKind::Injected(InjectedKind::TestClock), &e.to_string())))?.forget();
                    Ok(())
                }
            }
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
        Box::pin(async move { body(parent).await.map(Some) })
    }
}
/// `run`'s watchdog ends a test whose condition never comes true.
async fn until(condition: impl Fn() -> bool) {
    while !condition() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

#[derive(Clone, Copy, Debug)]
enum Outage {
    /// The backend is up and refuses the query.
    Http500,
    /// The connection ends with no response.
    Dropped,
}
/// Serves a JVM recording, whose last exchange is the query. While the backend is down only the
/// query fails: setup has already passed by the time a backend goes away in these runs.
async fn recorded_backend(
    scenario: &str,
    outage: Outage,
) -> OperationResult<(Peer, Arc<AtomicBool>, Arc<AtomicUsize>)> {
    let rows: Vec<_> = exchanges(scenario)
        .into_iter()
        .filter(|row| row["method"].is_string())
        .collect();
    let query = rows.last().expect("a recorded query").clone();
    let down = Arc::new(AtomicBool::new(true));
    let queries = Arc::new(AtomicUsize::new(0));
    let (is_down, counted) = (down.clone(), queries.clone());
    let peer = Peer::new(move |request| {
        let matches = |row: &&serde_json::Value| {
            row["method"] == request.method.as_str()
                && row["path"] == request.path.as_str()
                && row["body"] == request.body.as_str()
        };
        // Anything the JVM did not send is a difference.
        let row = rows
            .iter()
            .find(matches)
            .unwrap_or_else(|| panic!("not in the recording: {request:?}"));
        if matches(&&query) {
            counted.fetch_add(1, Ordering::SeqCst);
            if is_down.load(Ordering::SeqCst) {
                return match outage {
                    Outage::Http500 => Reply {
                        status: 500,
                        body: "{\"error\":\"down\"}".into(),
                        headers: vec![],
                    },
                    Outage::Dropped => Reply::dropped(),
                };
            }
        }
        Reply {
            status: row["status"].as_u64().unwrap() as u16,
            body: row["response"].as_str().unwrap().into(),
            headers: vec![],
        }
    })
    .await?;
    Ok((peer, down, queries))
}
/// The real client on real sockets and the vendor runtime, under a clock the test advances:
/// three failed attempts, one failed re-query, and then one that answers.
async fn recovers(env: Env, backend: &'static str, outage: Outage) -> OperationResult<()> {
    let scenario = format!("history-{backend}-p50");
    let answer = observations(&scenario)[0]["value"].as_f64().unwrap();
    let (peer, down, queries) = recorded_backend(&scenario, outage).await?;
    let clock = Clock::new();
    let mut context = env.context.clone();
    context.clock = clock.clone();
    let clients = AnalyticsClients::new(context.clone(), env.io.clone()).with_graphite_endpoint(peer.url.clone());
    let config = match backend {
        "v1" => AnalyticsConfiguration::InfluxDb(v1(&peer.url)),
        "v2" => AnalyticsConfiguration::InfluxDb2(v2(&peer.url)),
        _ => AnalyticsConfiguration::Graphite(GraphiteConfiguration {
            host: "127.0.0.1".into(),
            port: Some(2003),
            prefix: Some("prefix".into()),
            defaults: defaults(),
            read_only: false,
        }),
    };
    let provider = MetricsProviderFactory::new(context, Arc::new(clients))?
        .create_for(&config, false)
        .await?;
    // The instant the recording's query asks for, two hours before its anchor.
    let limit = jcompat::jtime::instant_of(ANCHOR / 1000 - 120 * 60, 0).unwrap();

    assert_eq!(provider.execution_time(&test(), 50.0, limit).await?, DEFAULT_MILLIS);
    assert_eq!(queries.load(Ordering::SeqCst), 3);
    clock.refresh_delays_started(1).await;

    clock.advance_past_a_refresh_delay();
    clock.refresh_delays_started(2).await;
    assert_eq!(queries.load(Ordering::SeqCst), 4);
    assert_eq!(provider.lookup().execution_time(&test(), 50.0, limit), DEFAULT_MILLIS);

    down.store(false, Ordering::SeqCst);
    clock.advance_past_a_refresh_delay();
    until(|| provider.lookup().execution_time(&test(), 50.0, limit) == answer).await;
    assert_eq!(queries.load(Ordering::SeqCst), 5);

    // Once it has answered, the key is final and the refresh over.
    provider.close().await?;
    assert_eq!(*clock.refresh_delays.lock().unwrap(), [5000, 10000]);
    assert_eq!(queries.load(Ordering::SeqCst), 5);
    peer.shutdown().await?;
    Ok(())
}
fn lines(logs: &[LogRecord], level: Level, text: &str) -> usize {
    logs.iter()
        .filter(|r| r.level == level && r.message.to_owned().contains(text))
        .count()
}
fn recovery_case(backend: &'static str, outage: Outage) {
    let logs = run(move |env| Box::pin(recovers(env, backend, outage)));
    let context = format!("{backend} {outage:?}");
    assert_eq!(lines(&logs, Level::INFO, "failed in"), 2, "{context}");
    assert_eq!(lines(&logs, Level::WARN, "Cannot fetch execution time"), 2, "{context}");
    assert_eq!(lines(&logs, Level::INFO, "returned 1 tests in"), 1, "{context}");
    assert_eq!(
        lines(&logs, Level::INFO, "returned 1 tests after failing earlier"),
        1,
        "{context}"
    );
}
#[test]
fn influxdb_comes_back_through_the_refresh_after_http_errors() {
    recovery_case("v1", Outage::Http500);
}
#[test]
fn influxdb_comes_back_through_the_refresh_after_dropped_connections() {
    recovery_case("v1", Outage::Dropped);
}
#[test]
fn influxdb2_comes_back_through_the_refresh_after_http_errors() {
    recovery_case("v2", Outage::Http500);
}
#[test]
fn influxdb2_comes_back_through_the_refresh_after_dropped_connections() {
    recovery_case("v2", Outage::Dropped);
}
#[test]
fn graphite_comes_back_through_the_refresh_after_http_errors() {
    recovery_case("graphite", Outage::Http500);
}
#[test]
fn graphite_comes_back_through_the_refresh_after_dropped_connections() {
    recovery_case("graphite", Outage::Dropped);
}

/// The real clock's bound, with the refresh delays cut short so that the test lasts only as long
/// as the bound it is about.
struct RealBound(Arc<dyn VendorClock>);
impl VendorClock for RealBound {
    fn delay(&self, millis: i64, cancellation: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
        let millis = if millis == CLOSE_BOUND_MILLIS { millis } else { 0 };
        self.0.delay(millis, cancellation)
    }
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, ()> {
        self.0.with_timeout(parent, millis, body)
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, Option<()>> {
        self.0.with_timeout_or_null(parent, millis, body)
    }
}
/// Fails its first query, and then blocks a worker of the vendor runtime until released.
struct Blocking {
    calls: AtomicUsize,
    blocked: AtomicBool,
    ended: AtomicBool,
    release: Mutex<mpsc::Receiver<()>>,
    closed: AtomicBool,
}
impl RemoteDataSource for Blocking {
    fn request_all_success_rates(&self, _: Instant) -> ServiceFuture<'_, Vec<SuccessRate>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn request_all_execution_times(&self, _: f64, _: Instant) -> ServiceFuture<'_, Vec<ExecutionTime>> {
        Box::pin(async {
            if self.calls.fetch_add(1, Ordering::SeqCst) < 3 {
                return Err(failure(FailureKind::Io, "down").into());
            }
            self.blocked.store(true, Ordering::SeqCst);
            let _ = self.release.lock().unwrap().recv_timeout(Duration::from_secs(60));
            self.ended.store(true, Ordering::SeqCst);
            Ok(vec![ExecutionTime {
                test_name: test().to_safe_test_name(),
                percentile: 7000.0,
            }])
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async {
            self.closed.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}
/// The production runtime, keeping each task's handle so that a test can wait for its end.
struct KeepsTasks {
    runtime: Arc<dyn VendorRuntime>,
    tasks: Mutex<Vec<Arc<dyn VendorTask>>>,
}
impl VendorRuntime for KeepsTasks {
    fn spawn_cancellable(&self, body: VendorTaskBody) -> Arc<dyn VendorTask> {
        let task = self.runtime.spawn_cancellable(body);
        self.tasks.lock().unwrap().push(task.clone());
        task
    }
    fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn VendorTask> {
        self.runtime.spawn(body)
    }
    fn spawn_blocking(&self, body: Box<dyn FnOnce() -> VendorResult<()> + Send>) -> Arc<dyn VendorTask> {
        self.runtime.spawn_blocking(body)
    }
}
#[test]
fn close_gets_past_a_blocked_vendor_worker_on_the_real_runtime_and_clock() {
    let logs = run(|env| {
        Box::pin(async move {
            let (release, released) = mpsc::channel();
            let source = Arc::new(Blocking {
                calls: AtomicUsize::new(0),
                blocked: AtomicBool::new(false),
                ended: AtomicBool::new(false),
                release: Mutex::new(released),
                closed: AtomicBool::new(false),
            });
            let mut context = env.context.clone();
            context.clock = Arc::new(RealBound(context.clock.clone()));
            let vendor = Arc::new(KeepsTasks {
                runtime: context.vendor.clone(),
                tasks: Mutex::default(),
            });
            context.vendor = vendor.clone();
            let provider = MetricsProviderImpl::new(source.clone(), defaults(), context)?;
            let limit = jcompat::jtime::instant_of(0, 0).unwrap();
            MetricsProvider::execution_time(&provider, &test(), 50.0, limit).await?;
            until(|| source.blocked.load(Ordering::SeqCst)).await;

            let started = std::time::Instant::now();
            provider.close().await?;
            // A real timer cannot fire early, so this proves close did wait for the query.
            assert!(started.elapsed() >= Duration::from_millis(CLOSE_BOUND_MILLIS as u64));
            assert!(source.closed.load(Ordering::SeqCst));
            assert!(!source.ended.load(Ordering::SeqCst));

            release
                .send(())
                .map_err(|e| failure(FailureKind::Injected(InjectedKind::TestBlocking), &e.to_string()))?;
            let refresh = vendor.tasks.lock().unwrap()[0].clone();
            let _ = refresh.join().await;
            assert!(source.ended.load(Ordering::SeqCst));
            // Its answer came after the cancel, so the key keeps the default.
            assert_eq!(
                MetricsLookup::execution_time(&provider, &test(), 50.0, limit),
                DEFAULT_MILLIS
            );
            Ok(())
        })
    });
    // The first query's three lines and close's warning: nothing from the query that ended later.
    let messages: Vec<_> = logs.iter().map(|r| r.message.to_owned()).collect();
    assert_eq!(messages.len(), 4, "{messages:?}");
    assert_eq!(lines(&logs, Level::DEBUG, "No execution time found"), 1);
    assert_eq!(lines(&logs, Level::WARN, "Cannot fetch execution time"), 1);
    assert_eq!(lines(&logs, Level::INFO, "failed in"), 1);
    assert_eq!(lines(&logs, Level::WARN, "did not stop within 3000 ms"), 1);
}
