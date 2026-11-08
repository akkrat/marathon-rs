//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/MetricsProviderImpl.kt
//! (D34: a failed history query is not final; Kotlin caches it for the whole run).
mod metrics_support;
mod strategy_support;
use jcompat::{jtime::Instant, logging::Level};
use marathon_config::schema::{BatchingStrategyConfiguration, SortingStrategyConfiguration};
use marathon_core::{
    analytics::{Analytics, remote::*},
    error::*,
    executor::{
        Signal,
        shared::{failure, requested},
    },
    model::*,
    runtime::{CancellationSignal, ServiceFuture},
    strategy::{BatchingStrategy, FlakinessStrategy, MetricsKey, MetricsLookup, SortingStrategy, TestQueue},
    wiring::MetricsProvider,
};
use metrics_support::*;
use std::{
    cmp::Ordering,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering},
    },
};
use strategy_support::*;
use tokio::sync::Notify;

const SCHEDULE: [i64; 8] = [5_000, 10_000, 20_000, 40_000, 80_000, 160_000, 300_000, 300_000];

fn down() -> OperationError {
    failure("java.lang.RuntimeException", "unreadable").into()
}
/// A first query's three attempts all fail, and then the backend answers.
fn down_for_first_query(source: &Source) {
    *source.errors.lock().unwrap() = [Some(down()), Some(down()), Some(down())].into();
}
fn timings(h: &Harness) -> Vec<String> {
    h.logs
        .records()
        .iter()
        .filter(|r| r.logger == "Timings")
        .map(|r| {
            let message = r.message.to_owned();
            message[..message.rfind(" in ").unwrap()].to_owned()
        })
        .collect()
}
fn lines_with(h: &Harness, level: Level, text: &str) -> usize {
    h.logs
        .records()
        .iter()
        .filter(|r| r.level == level && r.message.to_owned().contains(text))
        .count()
}
async fn until(what: &str, condition: impl Fn() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while !condition() {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} never happened"));
}
async fn until_idle(h: &Harness) {
    until("the refresh ending", || h.running.load(AtomicOrdering::SeqCst) == 0).await;
}
const QUERY: &str = "Execution time history query (percentile 80.0 since 2026-01-01T00:00:00Z)";

#[test]
fn a_failed_query_recovers_on_a_later_refresh() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source::default());
        let test = default_test();
        down_for_first_query(&source);
        *source.times.lock().unwrap() = vec![time_row(&test, 7000.0)];
        let p = h.provider(source.clone(), 300000, None);
        // The first lookup is Kotlin's: three attempts, then the defaults.
        assert_eq!(p.execution_time(&test, 80.0, limit()).await.unwrap(), 300000.0);
        assert_eq!(source.calls.lock().unwrap().len(), 3);
        h.clock.refresh_delay_started(1).await;
        assert_eq!(*h.clock.refresh_delays.lock().unwrap(), vec![5000]);

        h.clock.release_refreshes(1);
        until_idle(&h).await;
        assert_eq!(p.lookup().execution_time(&test, 80.0, limit()), 7000.0);
        assert_eq!(p.execution_time(&test, 80.0, limit()).await.unwrap(), 7000.0);
        assert_eq!(source.calls.lock().unwrap().len(), 4);
        assert_eq!(
            timings(&h),
            vec![format!("{QUERY} failed"), format!("{QUERY} returned 1 tests")]
        );
        assert_eq!(lines_with(&h, Level::Warn, "Cannot fetch execution time"), 1);
        assert_eq!(lines_with(&h, Level::Info, "after failing earlier"), 1);
        assert_eq!(lines_with(&h, Level::Info, "from now on use it"), 1);
        // The recovery ends the refresh: once read, a successful value never changes.
        *source.times.lock().unwrap() = vec![time_row(&test, 1.0)];
        h.clock.release_refreshes(8);
        p.prefetch(&[key(80.0)]).await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(p.execution_time(&test, 80.0, limit()).await.unwrap(), 7000.0);
        assert_eq!(source.calls.lock().unwrap().len(), 4);
        assert_eq!(h.clock.refresh_delays.lock().unwrap().len(), 1);
    });
}
#[test]
fn an_empty_answer_is_a_success_so_it_is_cached_and_never_refreshed() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source::default());
        let p = h.provider(source.clone(), 300000, None);
        h.clock.release_refreshes(8);
        assert_eq!(
            p.execution_time(&default_test(), 80.0, limit()).await.unwrap(),
            300000.0
        );
        assert_eq!(h.running.load(AtomicOrdering::SeqCst), 0);
        // And an empty answer to a refresh is just as final.
        let other = key(90.0);
        *source.errors.lock().unwrap() = [Some(down()), Some(down()), Some(down())].into();
        p.prefetch(&[other]).await.unwrap();
        until_idle(&h).await;
        assert_eq!(source.calls.lock().unwrap().len(), 1 + 3 + 1);
        assert_eq!(*h.clock.refresh_delays.lock().unwrap(), vec![5000]);
        // One line still ends the key's warnings, and it does not promise a change it can't bring.
        assert_eq!(
            lines_with(
                &h,
                Level::Info,
                "returned 0 tests after failing earlier. There is no history to use, so nothing changes"
            ),
            1
        );
        assert_eq!(lines_with(&h, Level::Info, "from now on use it"), 0);
        p.prefetch(&[key(80.0), other]).await.unwrap();
        assert_eq!(source.calls.lock().unwrap().len(), 1 + 3 + 1);
    });
}
#[test]
fn a_backend_that_stays_down_gets_a_capped_schedule_and_a_bounded_total() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source {
            failure: true,
            ..Source::default()
        });
        let p = h.provider(source.clone(), 300000, None);
        p.prefetch(&[key(80.0)]).await.unwrap();
        // Exactly how a failing backend adds lines: per re-query one failed Timings line and
        // one warning, and one more warning when they run out.
        h.clock.release_refreshes(100);
        until_idle(&h).await;
        assert_eq!(*h.clock.refresh_delays.lock().unwrap(), SCHEDULE);
        assert_eq!(source.calls.lock().unwrap().len(), 3 + SCHEDULE.len());
        assert_eq!(timings(&h), vec![format!("{QUERY} failed"); 1 + SCHEDULE.len()]);
        assert_eq!(
            lines_with(&h, Level::Warn, "Cannot fetch execution time"),
            1 + SCHEDULE.len()
        );
        assert_eq!(lines_with(&h, Level::Warn, "failed 8 more times"), 1);
        assert_eq!(h.logs.records().len(), 2 * (1 + SCHEDULE.len()) + 1);
        // After the last one nothing wakes it again, and the defaults keep answering.
        p.prefetch(&[key(80.0)]).await.unwrap();
        assert_eq!(p.lookup().execution_time(&default_test(), 80.0, limit()), 300000.0);
        assert_eq!(source.calls.lock().unwrap().len(), 3 + SCHEDULE.len());
    });
}
#[test]
fn lookups_and_prefetches_of_a_failed_key_never_query_the_backend() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source {
            failure: true,
            ..Source::default()
        });
        let p = h.provider(source.clone(), 300000, None);
        let keys = [key(80.0), MetricsKey::SuccessRate { limit: limit() }];
        let tests = generate_tests(500);
        for _ in 0..3 {
            p.prefetch(&keys).await.unwrap();
            for test in &tests {
                p.lookup().execution_time(test, 80.0, limit());
                p.lookup().success_rate(test, limit());
                p.execution_time(test, 80.0, limit()).await.unwrap();
                p.success_rate(test, limit()).await.unwrap();
            }
        }
        p.report_execution_time_coverage(&tests, 80.0, limit(), "sorting")
            .await
            .unwrap();
        tokio::task::yield_now().await;
        assert_eq!(source.calls.lock().unwrap().len(), 6);
        // One task, one delay, however many keys failed.
        assert_eq!(*h.clock.refresh_delays.lock().unwrap(), vec![5000]);
        assert_eq!(h.running.load(AtomicOrdering::SeqCst), 1);
        // A round queries each key once, in the order they first failed in.
        source.calls.lock().unwrap().clear();
        h.clock.release_refreshes(1);
        h.clock.refresh_delay_started(2).await;
        assert_eq!(*source.calls.lock().unwrap(), keys);
        assert_eq!(*h.clock.refresh_delays.lock().unwrap(), vec![5000, 10000]);
    });
}
#[test]
fn a_key_that_fails_during_a_refresh_joins_its_rounds_and_a_later_one_starts_its_own() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source {
            failure: true,
            ..Source::default()
        });
        let p = h.provider(source.clone(), 300000, None);
        p.prefetch(&[key(80.0)]).await.unwrap();
        h.clock.refresh_delay_started(1).await;
        h.clock.release_refreshes(1);
        h.clock.refresh_delay_started(2).await;
        p.prefetch(&[key(90.0)]).await.unwrap();
        assert_eq!(h.running.load(AtomicOrdering::SeqCst), 1);
        source.calls.lock().unwrap().clear();
        h.clock.release_refreshes(1);
        h.clock.refresh_delay_started(3).await;
        assert_eq!(*source.calls.lock().unwrap(), vec![key(80.0), key(90.0)]);
        // Each key has its own total: the late one outlasts the first by the rounds it missed.
        source.calls.lock().unwrap().clear();
        h.clock.release_refreshes(100);
        until_idle(&h).await;
        let calls = source.calls.lock().unwrap().clone();
        assert_eq!(calls.iter().filter(|c| **c == key(80.0)).count(), 8 - 2);
        assert_eq!(calls.iter().filter(|c| **c == key(90.0)).count(), 8 - 1);
        // All done, then another key fails: a new refresh, from the start of the schedule.
        h.clock.refresh_delays.lock().unwrap().clear();
        p.prefetch(&[key(70.0)]).await.unwrap();
        h.clock.refresh_delay_started(1).await;
        assert_eq!(h.clock.refresh_delays.lock().unwrap()[0], 5000);
    });
}

#[test]
fn close_ends_a_pending_refresh_without_waiting_and_nothing_queries_after_it() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source {
            failure: true,
            close_error: true,
            ..Source::default()
        });
        let p = h.provider(source.clone(), 300000, None);
        p.prefetch(&[key(80.0)]).await.unwrap();
        h.clock.refresh_delay_started(1).await;
        // Close errors still reach the caller, and no permit is ever released.
        assert_eq!(
            p.close().await,
            Err(OperationError::Failed(failure("java.io.IOException", "close")))
        );
        assert_eq!(h.running.load(AtomicOrdering::SeqCst), 0);
        h.clock.release_refreshes(8);
        tokio::task::yield_now().await;
        assert_eq!(source.calls.lock().unwrap().len(), 3);
        assert_eq!(h.logs.take().iter().filter(|r| r.logger == "Timings").count(), 1);
        // A query that starts failing after close starts no refresh.
        p.prefetch(&[key(90.0)]).await.unwrap();
        assert_eq!(h.running.load(AtomicOrdering::SeqCst), 0);
    });
}

/// Fails its first query, and holds every later one until released.
#[derive(Default)]
struct Hanging {
    calls: AtomicUsize,
    held: AtomicUsize,
    entered: Notify,
    release: Notify,
    closed_with_held: Mutex<Vec<usize>>,
}
impl RemoteDataSource for Hanging {
    fn request_all_success_rates(&self, _: Instant) -> ServiceFuture<'_, Vec<SuccessRate>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn request_all_execution_times(&self, _: f64, _: Instant) -> ServiceFuture<'_, Vec<ExecutionTime>> {
        Box::pin(async {
            if self.calls.fetch_add(1, AtomicOrdering::SeqCst) < 3 {
                return Err(down());
            }
            self.held.fetch_add(1, AtomicOrdering::SeqCst);
            struct Done<'a>(&'a AtomicUsize);
            impl Drop for Done<'_> {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, AtomicOrdering::SeqCst);
                }
            }
            let _done = Done(&self.held);
            self.entered.notify_one();
            self.release.notified().await;
            Ok(vec![])
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async {
            self.closed_with_held
                .lock()
                .unwrap()
                .push(self.held.load(AtomicOrdering::SeqCst));
            Ok(())
        })
    }
}
async fn entered(source: &Hanging) {
    tokio::time::timeout(std::time::Duration::from_secs(60), source.entered.notified())
        .await
        .expect("the refresh query never started");
}
#[test]
fn close_drops_a_refresh_query_in_flight_before_the_source_closes() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Hanging::default());
        let p = h.provider(source.clone(), 300000, None);
        p.prefetch(&[key(80.0)]).await.unwrap();
        h.clock.release_refreshes(1);
        entered(&source).await;
        h.logs.take();
        tokio::time::timeout(std::time::Duration::from_secs(60), p.close())
            .await
            .expect("close must not wait for a stuck backend")
            .unwrap();
        // A closed source never has a query running on it; nothing is logged or cached.
        assert_eq!(*source.closed_with_held.lock().unwrap(), vec![0]);
        assert!(h.logs.take().is_empty());
    });
}
/// Fails its first query, and then blocks its worker thread inside the future, where no
/// cancellation reaches it, until released.
struct Blocking {
    calls: AtomicUsize,
    blocked: AtomicBool,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
    closed: AtomicBool,
}
impl RemoteDataSource for Blocking {
    fn request_all_success_rates(&self, _: Instant) -> ServiceFuture<'_, Vec<SuccessRate>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn request_all_execution_times(&self, _: f64, _: Instant) -> ServiceFuture<'_, Vec<ExecutionTime>> {
        Box::pin(async {
            if self.calls.fetch_add(1, AtomicOrdering::SeqCst) < 3 {
                return Err(down());
            }
            self.blocked.store(true, AtomicOrdering::SeqCst);
            let _ = self
                .release
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(60));
            Ok(vec![time_row(&default_test(), 7000.0)])
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async {
            self.closed.store(true, AtomicOrdering::SeqCst);
            Ok(())
        })
    }
}
#[test]
fn close_stops_waiting_for_a_source_that_blocks_inside_its_future() {
    // Two workers, as the vendor runtime has: the source holds one, and close runs on the other.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let h = Harness::new();
        let (release, released) = std::sync::mpsc::channel();
        let source = Arc::new(Blocking {
            calls: AtomicUsize::new(0),
            blocked: AtomicBool::new(false),
            release: Mutex::new(released),
            closed: AtomicBool::new(false),
        });
        let p = Arc::new(h.provider(source.clone(), 300000, None));
        p.prefetch(&[key(80.0)]).await.unwrap();
        h.clock.release_refreshes(1);
        until("the query blocking", || source.blocked.load(AtomicOrdering::SeqCst)).await;
        let closing = {
            let p = p.clone();
            tokio::spawn(async move { p.close().await })
        };
        // Until the bound, close waits: the source is not closed under a query that may still end.
        until("close waiting", || {
            h.clock.close_bounds.load(AtomicOrdering::SeqCst) == 1
        })
        .await;
        assert!(!closing.is_finished());
        assert!(!source.closed.load(AtomicOrdering::SeqCst));
        assert_eq!(lines_with(&h, Level::Warn, "did not stop"), 0);

        h.clock.expire_close_bound();
        tokio::time::timeout(std::time::Duration::from_secs(60), closing)
            .await
            .expect("close must get past a source that blocks")
            .unwrap()
            .unwrap();
        assert!(source.closed.load(AtomicOrdering::SeqCst));
        assert_eq!(lines_with(&h, Level::Warn, "did not stop within 3000 ms"), 1);
        assert_eq!(h.running.load(AtomicOrdering::SeqCst), 1);

        // When the query does end, it is too late for it to log or publish.
        h.logs.take();
        release.send(()).unwrap();
        until_idle(&h).await;
        assert!(h.logs.take().is_empty());
        assert_eq!(p.lookup().execution_time(&default_test(), 80.0, limit()), 300000.0);
    });
}
#[test]
fn a_hung_refresh_blocks_neither_lookups_nor_prefetches_of_other_keys() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Hanging::default());
        let p = h.provider(source.clone(), 300000, None);
        p.prefetch(&[key(80.0)]).await.unwrap();
        h.clock.release_refreshes(1);
        entered(&source).await;
        assert_eq!(p.lookup().execution_time(&default_test(), 80.0, limit()), 300000.0);
        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            p.prefetch(&[MetricsKey::SuccessRate { limit: limit() }]),
        )
        .await
        .expect("a refresh must not hold up a queue build")
        .unwrap();
        source.release.notify_one();
        until_idle(&h).await;
    });
}
#[test]
fn the_runs_cancellation_ends_a_pending_refresh_without_a_query_or_a_log() {
    run(async {
        let mut h = Harness::new();
        let signal = Signal::new();
        h.context.cancellation = signal.clone();
        let source = Arc::new(Source {
            failure: true,
            ..Source::default()
        });
        let p = h.provider(source.clone(), 300000, None);
        p.prefetch(&[key(80.0)]).await.unwrap();
        h.clock.refresh_delay_started(1).await;
        h.logs.take();
        signal.cancel(requested("aborted"));
        until_idle(&h).await;
        h.clock.release_refreshes(8);
        tokio::task::yield_now().await;
        assert_eq!(source.calls.lock().unwrap().len(), 3);
        assert!(h.logs.take().is_empty());
    });
}
/// A run's signal that fires and then stops firing, which a real one never does, so that a test
/// can see what a refresh the run cancelled leaves behind for a later failure. It reports no
/// reason, so a prefetch still gets as far as starting a refresh.
struct Refiring(tokio::sync::watch::Sender<bool>);
impl CancellationSignal for Refiring {
    fn reason(&self) -> Option<Cancellation> {
        None
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Cancellation> + Send + '_>> {
        Box::pin(async move {
            let _ = self.0.subscribe().wait_for(|fired| *fired).await;
            requested("aborted")
        })
    }
}
/// The next failure starts a task of its own, and its rounds hold only its own key.
async fn the_next_failure_starts_a_clean_refresh(h: &Harness, p: &Analytics, source: &Source, delays: usize) {
    p.prefetch(&[key(90.0)]).await.unwrap();
    assert_eq!(h.running.load(AtomicOrdering::SeqCst), 1);
    h.clock.refresh_delay_started(delays + 1).await;
    assert_eq!(h.clock.refresh_delays.lock().unwrap()[delays], 5000);
    source.calls.lock().unwrap().clear();
    h.clock.release_refreshes(1);
    h.clock.refresh_delay_started(delays + 2).await;
    assert_eq!(*source.calls.lock().unwrap(), vec![key(90.0)]);
    p.close().await.unwrap();
}
fn refiring_harness() -> (Harness, Arc<Refiring>, Arc<Source>) {
    let mut h = Harness::new();
    let signal = Arc::new(Refiring(tokio::sync::watch::channel(false).0));
    h.context.cancellation = signal.clone();
    let source = Arc::new(Source {
        failure: true,
        ..Source::default()
    });
    (h, signal, source)
}
#[test]
fn a_refresh_the_run_cancels_leaves_no_stale_handle_for_the_next_failure() {
    run(async {
        let (h, signal, source) = refiring_harness();
        let p = h.provider(source.clone(), 300000, None);
        p.prefetch(&[key(80.0)]).await.unwrap();
        h.clock.refresh_delay_started(1).await;
        signal.0.send_replace(true);
        until_idle(&h).await;
        signal.0.send_replace(false);
        the_next_failure_starts_a_clean_refresh(&h, &p, &source, 1).await;
    });
}
#[test]
fn a_refresh_the_run_cancels_before_its_first_poll_leaves_no_stale_handle_either() {
    run(async {
        let (h, signal, source) = refiring_harness();
        let p = h.provider(source.clone(), 300000, None);
        signal.0.send_replace(true);
        p.prefetch(&[key(80.0)]).await.unwrap();
        until_idle(&h).await;
        assert!(h.clock.refresh_delays.lock().unwrap().is_empty());
        signal.0.send_replace(false);
        the_next_failure_starts_a_clean_refresh(&h, &p, &source, 0).await;
    });
}
/// Fails its first query, and panics on its first re-query. Every later query fails.
#[derive(Default)]
struct Panicking {
    calls: AtomicUsize,
}
impl RemoteDataSource for Panicking {
    fn request_all_success_rates(&self, _: Instant) -> ServiceFuture<'_, Vec<SuccessRate>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn request_all_execution_times(&self, _: f64, _: Instant) -> ServiceFuture<'_, Vec<ExecutionTime>> {
        Box::pin(async {
            if self.calls.fetch_add(1, AtomicOrdering::SeqCst) == 3 {
                panic!("a bug in the source, as this test expects");
            }
            Err(down())
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
#[test]
fn a_source_that_panics_during_a_refresh_ends_it_with_one_warning_and_no_stale_handle() {
    run(async {
        let h = Harness::new();
        let p = h.provider(Arc::new(Panicking::default()), 300000, None);
        p.prefetch(&[key(80.0)]).await.unwrap();
        h.clock.release_refreshes(1);
        // `h.running` never falls here: the harness's task count does not survive a panic.
        until("the warning", || lines_with(&h, Level::Warn, "refresh panicked") == 1).await;
        assert_eq!(p.lookup().execution_time(&default_test(), 80.0, limit()), 300000.0);
        // The next failure starts a refresh of its own, from the start of the schedule.
        p.prefetch(&[key(90.0)]).await.unwrap();
        h.clock.refresh_delay_started(2).await;
        assert_eq!(*h.clock.refresh_delays.lock().unwrap(), vec![5000, 5000]);
        p.close().await.unwrap();
        assert_eq!(lines_with(&h, Level::Warn, "refresh panicked"), 1);
    });
}
#[test]
fn a_cancelled_refresh_query_keeps_the_defaults_and_ends_the_refresh_silently() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source::default());
        *source.errors.lock().unwrap() = [
            Some(down()),
            Some(down()),
            Some(down()),
            Some(OperationError::Cancelled(requested("cancelled fetch"))),
        ]
        .into();
        *source.times.lock().unwrap() = vec![time_row(&default_test(), 1.0)];
        let p = h.provider(source.clone(), 300000, None);
        p.prefetch(&[key(80.0)]).await.unwrap();
        h.logs.take();
        h.clock.release_refreshes(8);
        until_idle(&h).await;
        assert_eq!(source.calls.lock().unwrap().len(), 4);
        assert!(h.logs.take().is_empty());
        assert_eq!(p.lookup().execution_time(&default_test(), 80.0, limit()), 300000.0);
    });
}
#[test]
fn dropping_the_provider_ends_its_refresh() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source {
            failure: true,
            ..Source::default()
        });
        let p = h.provider(source.clone(), 300000, None);
        p.prefetch(&[key(80.0)]).await.unwrap();
        h.clock.refresh_delay_started(1).await;
        drop(p);
        until_idle(&h).await;
        assert_eq!(Arc::strong_count(&source), 1);
    });
}

fn execution_time_sorting() -> SortingStrategy {
    SortingStrategy::from_config(&SortingStrategyConfiguration::ExecutionTime {
        percentile: 80.0,
        time_limit: limit(),
    })
}
fn success_rate_sorting(ascending: bool) -> SortingStrategy {
    SortingStrategy::from_config(&SortingStrategyConfiguration::SuccessRate {
        time_limit: limit(),
        ascending,
    })
}
fn hooks() -> Arc<ScriptedHooks> {
    ScriptedHooks::with_draws([], None)
}
/// All tests have the default, so the queue keeps them in insertion order; the history that
/// arrives later orders them the other way round.
async fn recovered_history(h: &Harness, tests: &[Test], rates: bool) -> Analytics {
    let source = Arc::new(Source::default());
    down_for_first_query(&source);
    if rates {
        *source.rates.lock().unwrap() = tests
            .iter()
            .enumerate()
            .map(|(i, t)| rate_row(t, 1.0 - i as f64 / 100.0))
            .collect();
    } else {
        *source.times.lock().unwrap() = tests
            .iter()
            .enumerate()
            .map(|(i, t)| time_row(t, 1000.0 + i as f64))
            .collect();
    }
    let p = h.provider(source, 300000, None);
    let key = if rates {
        MetricsKey::SuccessRate { limit: limit() }
    } else {
        key(80.0)
    };
    p.prefetch(&[key]).await.unwrap();
    p
}
async fn land_refresh(h: &Harness) {
    h.clock.release_refreshes(1);
    until_idle(h).await;
}
fn drain(queue: &mut TestQueue) -> Vec<Test> {
    std::iter::from_fn(|| queue.poll()).collect()
}
/// The heap is built, and half drained, on history the refresh then replaces.
async fn a_queue_keeps_its_order_when_history_arrives(sorting: SortingStrategy, rates: bool, sorted: Vec<usize>) {
    let h = Harness::new();
    let tests = generate_tests(9);
    let p = recovered_history(&h, &tests, rates).await;
    let mut queue = TestQueue::new(sorting.process(p.lookup(), hooks()));
    queue.add_all(tests.iter().cloned());
    let mut polled = vec![queue.poll().unwrap(), queue.poll().unwrap()];
    land_refresh(&h).await;
    // A retry returning to the queue compares against keys the comparator already judged.
    queue.add_all([polled.remove(0)]);
    polled.extend(drain(&mut queue));
    let mut got = polled.clone();
    got.sort_by_key(|t| tests.iter().position(|x| x == t));
    assert_eq!(got, tests, "every test comes out once");
    // Equal keys: a Java heap's order, the same as without any refresh.
    let mut control = TestQueue::new(Box::new(|_: &Test, _: &Test| Ordering::Equal));
    control.add_all(tests.iter().cloned());
    let first = control.poll().unwrap();
    let second = control.poll().unwrap();
    control.add_all([first]);
    let mut control_order = vec![second];
    control_order.extend(drain(&mut control));
    assert_eq!(polled, control_order);
    // The next queue is built on the new history.
    let mut next = TestQueue::new(sorting.process(p.lookup(), hooks()));
    next.add_all(tests.iter().cloned());
    let order: Vec<Test> = drain(&mut next);
    let expected: Vec<Test> = sorted.iter().map(|i| tests[*i].clone()).collect();
    assert_eq!(order, expected);
}
#[test]
fn an_execution_time_queue_keeps_its_order_and_the_next_queue_uses_new_history() {
    run(a_queue_keeps_its_order_when_history_arrives(
        execution_time_sorting(),
        false,
        (0..9).rev().collect(),
    ));
}
#[test]
fn a_success_rate_queue_keeps_its_order_and_the_next_queue_uses_new_history() {
    run(a_queue_keeps_its_order_when_history_arrives(
        success_rate_sorting(true),
        true,
        (0..9).rev().collect(),
    ));
}
#[test]
fn a_descending_success_rate_comparator_never_changes_its_answers() {
    run(async {
        let h = Harness::new();
        let tests = generate_tests(2);
        let p = recovered_history(&h, &tests, true).await;
        let mut comparator = success_rate_sorting(false).process(p.lookup(), hooks());
        assert_eq!(comparator(&tests[0], &tests[1]), Ordering::Equal);
        land_refresh(&h).await;
        assert_eq!(comparator(&tests[0], &tests[1]), Ordering::Equal);
        let mut next = success_rate_sorting(false).process(p.lookup(), hooks());
        assert_eq!(next(&tests[0], &tests[1]), Ordering::Less);
    });
}
#[test]
fn a_batch_composed_after_a_refresh_uses_the_new_history() {
    run(async {
        let h = Harness::new();
        let tests = generate_tests(6);
        let p = recovered_history(&h, &tests, false).await;
        let batching = BatchingStrategy::from_config(&BatchingStrategyConfiguration::FixedSize {
            size: 3,
            duration_millis: Some(199000),
            percentile: Some(80.0),
            time_limit: Some(limit()),
            last_mile_length: 0,
        });
        let mut queue = TestQueue::new(Box::new(|_: &Test, _: &Test| Ordering::Equal));
        queue.add_all(tests.iter().cloned());
        let batch = |queue: &mut TestQueue, p: &Analytics| {
            batching
                .process(queue, p.lookup().as_ref(), None, hooks().as_ref())
                .unwrap()
                .tests
                .len()
        };
        // The default of 300 s fills a batch at once, about 1 s does not.
        assert_eq!(batch(&mut queue, &p), 1);
        land_refresh(&h).await;
        assert_eq!(batch(&mut queue, &p), 3);
    });
}

/// A lookup whose live answers are off limits, so that a reader that does not go through its
/// view fails: this is what stops a refresh landing in the middle of a batch or of a shard.
struct OnlyThroughItsView {
    views: AtomicUsize,
    rate: f64,
    time: f64,
}
struct Frozen(f64, f64);
impl MetricsLookup for Frozen {
    fn success_rate(&self, _: &Test, _: Instant) -> f64 {
        self.0
    }
    fn execution_time(&self, _: &Test, _: f64, _: Instant) -> f64 {
        self.1
    }
}
impl MetricsLookup for OnlyThroughItsView {
    fn success_rate(&self, _: &Test, _: Instant) -> f64 {
        panic!("read the live history")
    }
    fn execution_time(&self, _: &Test, _: f64, _: Instant) -> f64 {
        panic!("read the live history")
    }
    fn view(&self) -> Option<Arc<dyn MetricsLookup>> {
        self.views.fetch_add(1, AtomicOrdering::SeqCst);
        Some(Arc::new(Frozen(self.rate, self.time)))
    }
}
#[test]
fn a_batch_a_shard_and_a_comparator_each_read_one_view_of_the_history() {
    let lookup = Arc::new(OnlyThroughItsView {
        views: AtomicUsize::new(0),
        rate: 0.5,
        time: 1000.0,
    });
    let tests = generate_tests(4);
    let shard = FlakinessStrategy::from_config(&marathon_config::schema::FlakinessStrategyConfiguration::Probability {
        min_success_rate: 0.8,
        max_count: 1,
        time_limit: limit(),
    })
    .process(
        TestShard {
            tests: tests.clone(),
            flaky_tests: vec![],
        },
        lookup.as_ref(),
    );
    assert_eq!(shard.flaky_tests.len(), 4);
    assert_eq!(lookup.views.swap(0, AtomicOrdering::SeqCst), 1);

    let mut queue = TestQueue::new(execution_time_sorting().process(lookup.clone(), hooks()));
    queue.add_all(tests.iter().cloned());
    assert_eq!(lookup.views.swap(0, AtomicOrdering::SeqCst), 1);
    let mut rates = TestQueue::new(success_rate_sorting(true).process(lookup.clone(), hooks()));
    rates.add_all(tests.iter().cloned());
    assert_eq!(lookup.views.swap(0, AtomicOrdering::SeqCst), 1);

    let batch = BatchingStrategy::from_config(&BatchingStrategyConfiguration::FixedSize {
        size: 3,
        duration_millis: Some(199000),
        percentile: Some(80.0),
        time_limit: Some(limit()),
        last_mile_length: 0,
    })
    .process(&mut queue, lookup.as_ref(), None, hooks().as_ref())
    .unwrap();
    assert_eq!(batch.tests.len(), 3);
    assert_eq!(lookup.views.load(AtomicOrdering::SeqCst), 1);
}

/// Everything a scripted run shows: the answers, the backend's calls and the log, in order.
async fn transcript(answers: Vec<Option<OperationError>>) -> Vec<String> {
    let h = Harness::new();
    let source = Arc::new(Source::default());
    let tests = generate_tests(3);
    *source.errors.lock().unwrap() = answers.into();
    *source.times.lock().unwrap() = vec![time_row(&tests[0], 7000.0)];
    *source.rates.lock().unwrap() = vec![rate_row(&tests[1], 0.75)];
    let p = h.provider(source.clone(), 300000, None);
    let keys = [MetricsKey::SuccessRate { limit: limit() }, key(80.0), key(90.0)];
    p.prefetch(&keys).await.unwrap();
    let mut out = vec![];
    for round in 1..=4 {
        h.clock.release_refreshes(1);
        // The round has ended when the next delay starts or the refresh is over.
        until("the round ending", || {
            h.clock.refresh_delays.lock().unwrap().len() > round || h.running.load(AtomicOrdering::SeqCst) == 0
        })
        .await;
        for test in &tests {
            out.push(format!(
                "{} {} {}",
                p.lookup().success_rate(test, limit()),
                p.lookup().execution_time(test, 80.0, limit()),
                p.lookup().execution_time(test, 90.0, limit())
            ));
        }
    }
    p.close().await.unwrap();
    out.extend(source.calls.lock().unwrap().iter().map(|call| format!("{call:?}")));
    out.extend(
        h.logs
            .take()
            .iter()
            .map(|r| format!("{} {} {}", r.logger, r.level.as_str(), r.message.to_owned())),
    );
    out
}
#[test]
fn the_same_backend_answers_give_the_same_run() {
    run(async {
        let answers = || {
            let (f, ok) = (|| Some(down()), || None);
            // The first queries: the rate and 80 fail all three attempts, and 90 answers. Then the
            // 1st refresh round: the rate answers, 80 fails. The 2nd: 80 answers.
            vec![f(), f(), f(), f(), f(), f(), ok(), ok(), f(), ok()]
        };
        let first = transcript(answers()).await;
        assert_eq!(first, transcript(answers()).await);
        assert_eq!(
            first
                .iter()
                .filter(|line| line.contains("after failing earlier"))
                .count(),
            2
        );
        // After the 1st round, for each test; then after the 2nd, for the first test.
        assert_eq!(first[..2], ["0 300000 7000", "0.75 300000 300000"]);
        assert_eq!(first[3], "0 7000 7000");
    });
}

#[test]
fn lookups_on_another_thread_see_one_value_or_the_other_while_refreshes_land() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let h = Harness::new();
        let test = default_test();
        let source = Arc::new(Source::default());
        down_for_first_query(&source);
        *source.times.lock().unwrap() = vec![time_row(&test, 7000.0)];
        let p = Arc::new(h.provider(source, 300000, None));
        p.prefetch(&[key(80.0)]).await.unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let (p, stop, test) = (p.clone(), stop.clone(), test.clone());
            std::thread::spawn(move || {
                let lookup = p.lookup();
                let mut seen = vec![];
                while !stop.load(AtomicOrdering::SeqCst) {
                    let value = lookup.execution_time(&test, 80.0, limit());
                    if seen.last() != Some(&value) {
                        seen.push(value);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                // One look after the stop, which comes after the refresh landed: without it a reader that slept through
                // the refresh saw only the old value (U182).
                let value = lookup.execution_time(&test, 80.0, limit());
                if seen.last() != Some(&value) {
                    seen.push(value);
                }
                seen
            })
        };
        h.clock.release_refreshes(1);
        until_idle(&h).await;
        stop.store(true, AtomicOrdering::SeqCst);
        let seen = reader.join().unwrap();
        assert!(seen == [300000.0, 7000.0] || seen == [7000.0], "{seen:?}");
        assert_eq!(p.lookup().execution_time(&test, 80.0, limit()), 7000.0);
    });
}
