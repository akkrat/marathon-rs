//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/MetricsProviderImpl.kt
//! (D13 prefetch, P4 cancellation and P5 nonblocking lookups).
mod metrics_support;
mod strategy_support;
use jcompat::jtime::Instant;
use marathon_core::{
    analytics::remote::*,
    error::*,
    executor::{
        Signal,
        shared::{failure, requested},
    },
    model::TestStatus,
    runtime::ServiceFuture,
    strategy::MetricsKey,
    wiring::MetricsProvider,
};
use metrics_support::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use strategy_support::*;
use tokio::sync::Notify;
#[derive(Default)]
struct Gate {
    calls: AtomicUsize,
    block: AtomicBool,
    entered: Notify,
    release: Notify,
}
impl RemoteDataSource for Gate {
    fn request_all_success_rates(&self, _: Instant) -> ServiceFuture<'_, Vec<SuccessRate>> {
        Box::pin(async {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![])
        })
    }
    fn request_all_execution_times(&self, _: f64, _: Instant) -> ServiceFuture<'_, Vec<ExecutionTime>> {
        Box::pin(async {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.block.load(Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            Ok(vec![])
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
#[test]
fn concurrent_pools_share_single_flight_and_first_touch_keys() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Gate::default());
        let p = Arc::new(h.provider(source.clone(), 300000, None));
        // Marathon-level first touches share the very same cache as queue prefetch.
        p.success_rate(&default_test(), limit()).await.unwrap();
        p.report_execution_time_coverage(&generate_tests(2), 80.0, limit(), &"sorting".into())
            .await
            .unwrap();
        source.block.store(true, Ordering::SeqCst);
        let keys = vec![MetricsKey::SuccessRate { limit: limit() }, key(80.0), key(90.0)];
        let a = {
            let p = p.clone();
            let keys = keys.clone();
            tokio::spawn(async move { p.prefetch(&keys).await })
        };
        source.entered.notified().await;
        let b = {
            let p = p.clone();
            tokio::spawn(async move { p.prefetch(&keys).await })
        };
        tokio::task::yield_now().await;
        assert_eq!(source.calls.load(Ordering::SeqCst), 3);
        source.release.notify_one();
        assert!(matches!(a.await, Ok(Ok(()))));
        assert!(matches!(b.await, Ok(Ok(()))));
        assert_eq!(source.calls.load(Ordering::SeqCst), 3);
        assert_eq!(h.logs.take().iter().filter(|r| r.logger == "Timings").count(), 3);
    });
}
#[test]
fn lookup_never_waits_for_unrelated_fetch_or_registry_and_sees_observations() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Gate::default());
        let p = Arc::new(h.provider(source.clone(), 300000, Some(1)));
        p.prefetch(&[key(80.0)]).await.unwrap();
        let lookup = p.lookup();
        let t = generate_tests(2);
        p.observe_execution_times(&[result(&t[0], TestStatus::Passed, 20000)])
            .await
            .unwrap();
        source.block.store(true, Ordering::SeqCst);
        let task = {
            let p = p.clone();
            tokio::spawn(async move { p.prefetch(&[key(90.0)]).await })
        };
        source.entered.notified().await;
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            p.observe_execution_times(&[result(&t[0], TestStatus::Failure, 40000)]),
        )
        .await
        .expect("observe must not wait for a fetch")
        .unwrap();
        {
            let _registry = h.context.registry.lock().unwrap();
            assert_eq!(lookup.execution_time(&t[1], 80.0, limit()), 40000.0);
        }
        source.release.notify_one();
        assert!(matches!(task.await, Ok(Ok(()))));
        p.observe_execution_times(&[result(&t[0], TestStatus::Failure, 40000)])
            .await
            .unwrap();
        assert_eq!(lookup.execution_time(&t[1], 80.0, limit()), 40000.0);
        assert_eq!(
            h.logs
                .take()
                .iter()
                .filter(|r| r.message.to_string_lossy().starts_with("No execution time"))
                .count(),
            1
        );
    });
}
#[test]
fn cancellation_is_not_retried_logged_or_cached_and_source_can_be_fetched_again() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source::default());
        source
            .errors
            .lock()
            .unwrap()
            .push_back(Some(OperationError::Cancelled(requested("cancelled fetch"))));
        let p = h.provider(source.clone(), 300000, None);
        assert!(matches!(
            p.prefetch(&[key(80.0)]).await,
            Err(OperationError::Cancelled(_))
        ));
        assert!(h.logs.take().is_empty());
        assert!(h.clock.delays.lock().unwrap().is_empty());
        p.prefetch(&[key(80.0)]).await.unwrap();
        assert_eq!(source.calls.lock().unwrap().len(), 2);
        assert_eq!(h.logs.take().iter().filter(|r| r.logger == "Timings").count(), 1);
    });
}
#[test]
fn abort_interrupts_a_pending_fetch_without_logging_or_caching() {
    run(async {
        let mut h = Harness::new();
        let signal = Signal::new();
        h.context.cancellation = signal.clone();
        let source = Arc::new(Gate::default());
        source.block.store(true, Ordering::SeqCst);
        let p = Arc::new(h.provider(source.clone(), 300000, None));
        let task = {
            let p = p.clone();
            tokio::spawn(async move { p.prefetch(&[key(80.0)]).await })
        };
        source.entered.notified().await;
        signal.cancel(requested("aborted"));
        assert!(matches!(task.await, Ok(Err(OperationError::Cancelled(_)))));
        assert!(h.logs.take().is_empty());
        assert!(h.clock.delays.lock().unwrap().is_empty());
    });
}
#[test]
fn failed_queries_cache_defaults_close_errors_propagate_and_duplicate_names_count() {
    run(async {
        let h = Harness::new();
        let source = Arc::new(Source {
            failure: true,
            close_error: true,
            ..Source::default()
        });
        let p = h.provider(source.clone(), 300000, None);
        let keys = [key(80.0), MetricsKey::SuccessRate { limit: limit() }];
        for _ in 0..2 {
            p.prefetch(&keys).await.unwrap();
            assert_eq!(p.lookup().execution_time(&default_test(), 80.0, limit()), 300000.0);
            assert_eq!(p.lookup().success_rate(&default_test(), limit()), 0.0);
        }
        assert_eq!(source.calls.lock().unwrap().len(), 6);
        assert_eq!(*h.clock.delays.lock().unwrap(), vec![50, 50, 50, 50]);
        assert_eq!(
            p.close().await,
            Err(OperationError::Failed(failure("java.io.IOException", "close")))
        );
    });
}
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "MetricsKey must be prefetched")]
fn unprefetched_lookup_is_a_debug_contract_violation() {
    let h = Harness::new();
    let p = h.provider(Arc::new(Source::default()), 300000, None);
    p.lookup().execution_time(&default_test(), 80.0, limit());
}
#[test]
fn shared_log_producer_preserves_core_and_vendor_call_order() {
    let q = marathon_core::logging::LogQueue::default();
    q.log("core", 1, jcompat::logging::Level::Info, "before".into(), None);
    q.sender().push(marathon_core::logging::LogRecord {
        logger: "provider".into(),
        at_millis: 2,
        wall_millis: 3,
        level: jcompat::logging::Level::Warn,
        message: "middle".into(),
        cause: None,
    });
    q.log("core", 4, jcompat::logging::Level::Info, "after".into(), None);
    assert_eq!(
        q.records()
            .iter()
            .map(|r| r.message.to_string_lossy())
            .collect::<Vec<_>>(),
        vec!["before", "middle", "after"]
    );
    assert_eq!(q.take().len(), 3);
    assert!(q.take().is_empty());
}

// Adopted from the round 1 concurrency review.
#[derive(Default)]
struct Slow {
    calls: std::sync::Mutex<std::collections::HashMap<String, usize>>,
    total: std::sync::atomic::AtomicUsize,
}
impl Slow {
    async fn hit(&self, k: String) {
        *self.calls.lock().unwrap().entry(k).or_default() += 1;
        self.total.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}
impl RemoteDataSource for Slow {
    fn request_all_success_rates(&self, l: Instant) -> ServiceFuture<'_, Vec<SuccessRate>> {
        Box::pin(async move {
            self.hit(format!("rate {l}")).await;
            Ok(vec![])
        })
    }
    fn request_all_execution_times(&self, p: f64, l: Instant) -> ServiceFuture<'_, Vec<ExecutionTime>> {
        Box::pin(async move {
            self.hit(format!("time {p} {l}")).await;
            Ok(vec![])
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn n_pools_prefetch_overlapping_keys_once_each_multithread() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let h = Harness::new();
        let source = Arc::new(Slow::default());
        let p = Arc::new(h.provider(source.clone(), 300000, Some(1)));
        let all = vec![
            key(80.0),
            key(90.0),
            key(f64::NAN),
            key(-0.0),
            key(0.0),
            MetricsKey::SuccessRate { limit: limit() },
        ];
        let mut tasks = vec![];
        for i in 0..32usize {
            let p = p.clone();
            let mut keys = all.clone();
            keys.rotate_left(i % all.len());
            keys.truncate(2 + i % 5);
            let keys2 = keys.clone();
            tasks.push(tokio::spawn(async move {
                p.prefetch(&keys2).await.unwrap();
                // lookups concurrently with other pools' fetches
                let lookup = p.lookup();
                for k in &keys2 {
                    if let MetricsKey::ExecutionTime { percentile, limit } = k {
                        lookup.execution_time(&default_test(), *percentile, *limit);
                    }
                }
                p.observe_execution_times(&[result(&default_test(), marathon_core::model::TestStatus::Passed, 10)])
                    .await
                    .unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        let calls = source.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 6, "{calls:?}");
        assert!(calls.values().all(|n| *n == 1), "{calls:?}");
        let timings = h.logs.take().into_iter().filter(|r| r.logger == "Timings").count();
        assert_eq!(timings, 6);
    });
}

#[test]
fn aborted_holder_mid_fetch_leaves_no_entry_and_next_caller_refetches() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let h = Harness::new();
        let source = Arc::new(Gate::default());
        source.block.store(true, Ordering::SeqCst);
        let p = Arc::new(h.provider(source.clone(), 300000, None));
        let a = {
            let p = p.clone();
            tokio::spawn(async move { p.prefetch(&[key(80.0)]).await })
        };
        source.entered.notified().await;
        a.abort();
        assert!(a.await.unwrap_err().is_cancelled());
        assert!(h.logs.take().is_empty());
        source.block.store(false, Ordering::SeqCst);
        p.prefetch(&[key(80.0)]).await.unwrap();
        assert_eq!(source.calls.load(Ordering::SeqCst), 2);
        assert_eq!(h.logs.take().iter().filter(|r| r.logger == "Timings").count(), 1);
    });
}

#[test]
fn fifty_thousand_individual_observations_do_not_copy_prior_samples() {
    run(async {
        let h = Harness::new();
        let p = h.provider(Arc::new(Source::default()), 300000, Some(1));
        p.prefetch(&[key(80.0)]).await.unwrap();
        let tests = generate_tests(50000);
        let results: Vec<_> = tests.iter().map(|t| result(t, TestStatus::Passed, 10)).collect();
        let started = std::time::Instant::now();
        for r in &results {
            p.observe_execution_times(std::slice::from_ref(r)).await.unwrap();
        }
        // Also exercise repeated appends: no copies of a test's previous samples.
        for _ in 0..50000 {
            p.observe_execution_times(&results[..1]).await.unwrap();
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "100k observations took {:?}",
            started.elapsed()
        );
        assert_eq!(p.lookup().execution_time(&tests[49999], 80.0, limit()), 10.0);
        assert_eq!(p.lookup().execution_time(&default_test(), 80.0, limit()), 10.0);
    });
}

#[test]
fn prefetch_does_not_reacquire_the_logging_registry() {
    let h = Harness::new();
    let p = Arc::new(h.provider(Arc::new(Source::default()), 300000, None));
    let registry = h.context.registry.lock().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let task = std::thread::spawn(move || {
        run(async {
            send.send(p.prefetch(&[key(80.0)]).await).unwrap();
        });
    });
    let completed = receive.recv_timeout(std::time::Duration::from_secs(2));
    // Release before joining even on failure, so a regression cannot hang the gate.
    drop(registry);
    task.join().unwrap();
    assert!(matches!(completed, Ok(Ok(()))));
}
