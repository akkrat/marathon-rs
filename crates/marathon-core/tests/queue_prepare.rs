//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/queue/QueueActor.kt
//! (D13 preparation cancellation and the shared metrics-provider handoff).
mod metrics_support;
mod strategy_support;
#[path = "support/queue.rs"]
mod support;

use futures_util::future::{Either, select};
use jcompat::jtime::Instant;
use marathon_config::schema::SortingStrategyConfiguration;
use marathon_core::{
    analytics::remote::{ExecutionTime, RemoteDataSource, SuccessRate},
    error::{Cancellation, OperationError},
    execution::queue::QueueActor,
    executor::{
        Signal,
        shared::{failure, requested},
    },
    runtime::ServiceFuture,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

#[derive(Default)]
struct Gate {
    calls: AtomicUsize,
    active: AtomicUsize,
    entered: Notify,
    release: Notify,
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl RemoteDataSource for Gate {
    fn request_all_success_rates(&self, _: Instant) -> ServiceFuture<'_, Vec<SuccessRate>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn request_all_execution_times(&self, _: f64, _: Instant) -> ServiceFuture<'_, Vec<ExecutionTime>> {
        Box::pin(async {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.active.fetch_add(1, Ordering::SeqCst);
            let _active = Active(&self.active);
            self.entered.notify_one();
            self.release.notified().await;
            Ok(vec![])
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn configuration() -> marathon_config::Configuration {
    let mut configuration = support::configuration();
    configuration.sorting_strategy = SortingStrategyConfiguration::ExecutionTime {
        percentile: 90.0,
        time_limit: support::epoch(),
    };
    configuration
}

#[test]
fn prepare_preserves_caller_cancellation_when_the_fetch_is_released() {
    let configuration = configuration();
    support::run(move |core| {
        Box::pin(async move {
            let h = metrics_support::Harness::new();
            let source = Arc::new(Gate::default());
            let analytics = Arc::new(h.provider(source.clone(), 1000, None));
            let signal = Signal::new();
            let vendor = core.vendor();
            let preparation = QueueActor::prepare(vendor.as_ref(), analytics, &configuration, signal.clone());
            let mut preparation = std::pin::pin!(preparation);
            assert!(matches!(
                select(preparation.as_mut(), Box::pin(source.entered.notified())).await,
                Either::Right(_)
            ));
            let reason = Cancellation::Timeout {
                scope: 73,
                detail: failure("timeout", "prepare caller cancelled"),
            };
            signal.cancel(reason.clone());
            source.release.notify_one();
            let result = preparation.await;
            assert!(matches!(result, Err(OperationError::Cancelled(actual)) if actual == reason));
            assert_eq!(source.active.load(Ordering::SeqCst), 0);
            Ok(())
        })
    });
}

#[test]
fn cancelled_prepare_holder_stops_fetch_without_caching_and_another_pool_refetches() {
    let configuration = configuration();
    support::run(move |core| {
        Box::pin(async move {
            let h = metrics_support::Harness::new();
            let source = Arc::new(Gate::default());
            let analytics = Arc::new(h.provider(source.clone(), 1000, None));
            let signal = Signal::new();
            let vendor = core.vendor();
            let preparation = QueueActor::prepare(vendor.as_ref(), analytics.clone(), &configuration, signal.clone());
            let mut preparation = std::pin::pin!(preparation);
            assert!(matches!(
                select(preparation.as_mut(), Box::pin(source.entered.notified())).await,
                Either::Right(_)
            ));
            let reason = requested("holder cancelled");
            signal.cancel(reason.clone());
            let result = tokio::time::timeout(Duration::from_secs(60), preparation).await;
            assert!(matches!(result, Ok(Err(OperationError::Cancelled(actual))) if actual == reason));
            assert_eq!(
                source.active.load(Ordering::SeqCst),
                0,
                "vendor work must finish before prepare returns"
            );
            assert!(h.logs.take().is_empty());
            assert!(h.clock.delays.lock().unwrap().is_empty());
            source.release.notify_one();
            QueueActor::prepare(vendor.as_ref(), analytics.clone(), &configuration, Signal::new()).await?;
            QueueActor::prepare(vendor.as_ref(), analytics, &configuration, Signal::new()).await?;
            assert_eq!(source.calls.load(Ordering::SeqCst), 2);
            assert_eq!(h.logs.take().iter().filter(|r| r.logger == "Timings").count(), 1);
            Ok(())
        })
    });
}

#[test]
fn cancelling_a_prepare_waiter_keeps_the_other_pools_single_flight_fetch() {
    let configuration = configuration();
    support::run(move |core| {
        Box::pin(async move {
            let h = metrics_support::Harness::new();
            let source = Arc::new(Gate::default());
            let analytics = Arc::new(h.provider(source.clone(), 1000, None));
            let vendor = core.vendor();
            let holder = QueueActor::prepare(vendor.as_ref(), analytics.clone(), &configuration, Signal::new());
            let mut holder = std::pin::pin!(holder);
            assert!(matches!(
                select(holder.as_mut(), Box::pin(source.entered.notified())).await,
                Either::Right(_)
            ));
            let signal = Signal::new();
            let waiter = QueueActor::prepare(vendor.as_ref(), analytics.clone(), &configuration, signal.clone());
            let mut waiter = std::pin::pin!(waiter);
            std::future::poll_fn(|cx| {
                assert!(std::future::Future::poll(waiter.as_mut(), cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            let reason = Cancellation::Parent(failure("cancelled", "waiter parent cancelled"));
            signal.cancel(reason.clone());
            let result = tokio::time::timeout(Duration::from_secs(60), waiter).await;
            assert!(matches!(result, Ok(Err(OperationError::Cancelled(actual))) if actual == reason));
            assert_eq!(source.active.load(Ordering::SeqCst), 1);
            assert_eq!(source.calls.load(Ordering::SeqCst), 1);
            source.release.notify_one();
            holder.await?;
            QueueActor::prepare(vendor.as_ref(), analytics, &configuration, Signal::new()).await?;
            assert_eq!(source.calls.load(Ordering::SeqCst), 1);
            assert_eq!(h.logs.take().iter().filter(|r| r.logger == "Timings").count(), 1);
            Ok(())
        })
    });
}

#[test]
fn already_cancelled_prepare_rejects_even_an_empty_prefetch() {
    let configuration = support::configuration();
    support::run(move |core| {
        Box::pin(async move {
            let h = metrics_support::Harness::new();
            let source = Arc::new(Gate::default());
            let analytics = Arc::new(h.provider(source.clone(), 1000, None));
            let signal = Signal::new();
            let reason = requested("cancelled before prepare");
            signal.cancel(reason.clone());
            let result = QueueActor::prepare(core.vendor().as_ref(), analytics, &configuration, signal).await;
            assert!(matches!(result, Err(OperationError::Cancelled(actual)) if actual == reason));
            assert_eq!(source.calls.load(Ordering::SeqCst), 0);
            assert!(h.logs.take().is_empty());
            Ok(())
        })
    });
}

/// D34: the JavaPriorityQueue of a live pool keeps the history it was built on, and the next pool's takes the refresh.
#[test]
fn a_queue_keeps_the_history_it_was_built_on_and_the_next_pools_queue_gets_the_refresh() {
    use marathon_core::runtime::CoreRuntime;
    use marathon_core::{
        execution::FromQueue,
        execution::queue::PreparedQueue,
        execution::queue::{QueueHandle, QueueMessage, RequestBatch},
        model::{DevicePoolId, Test, TestShard},
        strategy::TestQueue,
    };
    use std::rc::Rc;
    use support::*;
    let mut configuration = support::configuration();
    configuration.sorting_strategy = SortingStrategyConfiguration::SuccessRate {
        time_limit: support::epoch(),
        ascending: true,
    };
    configuration.batching_strategy = support::fixed_size(1);
    support::run(move |core| {
        Box::pin(async move {
            let h = metrics_support::Harness::new();
            let tests: Vec<Test> = (0..7).map(|i| mtest("pkg", "Class", &format!("method{i}"))).collect();
            let source = Arc::new(metrics_support::Source::default());
            let down = || Some(OperationError::Failed(failure("java.lang.RuntimeException", "down")));
            *source.errors.lock().unwrap() = [down(), down(), down()].into();
            // Ascending in insertion order. The heap drains equal keys as 0, 6, 5, 4, ..., so a comparator
            // that took this history while the heap holds them would come out differently.
            *source.rates.lock().unwrap() = tests
                .iter()
                .enumerate()
                .map(|(i, t)| metrics_support::rate_row(t, 0.5 + i as f64 / 100.0))
                .collect();
            let analytics = Arc::new(h.provider(source.clone(), 1000, None));
            let spawn = |prepared: PreparedQueue| -> (QueueHandle, Rc<RecordingPool>) {
                let pool = RecordingPool::new(&core);
                let progress = Queue::spawn(&core, Setup::new(configuration.clone(), tests.clone())).progress;
                let handle = QueueActor::spawn(
                    core.clone(),
                    prepared,
                    &configuration,
                    TestShard {
                        tests: tests.clone(),
                        flaky_tests: vec![],
                    },
                    pool.clone(),
                    DevicePoolId { name: "pool".into() },
                    Arc::new(ScriptedTimer {
                        trace: Trace::default(),
                        times: Default::default(),
                    }),
                    None,
                    progress,
                    core.job(marathon_core::runtime::JobKind::Ordinary, None),
                    Arc::new(Hooks {
                        trace: Trace::default(),
                        draws: Default::default(),
                        ids: Default::default(),
                    }),
                );
                (handle, pool)
            };
            let order = |queue: (QueueHandle, Rc<RecordingPool>)| async move {
                let (handle, pool) = queue;
                let mut order: Vec<Test> = vec![];
                for i in 0..7 {
                    assert!(handle.try_send(QueueMessage::RequestBatch(RequestBatch {
                        device: device(&format!("serial{i}")),
                        worker_id: None,
                    })));
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    assert!(handle.try_send(QueueMessage::IsEmpty(tx)));
                    rx.await.unwrap();
                    for reply in pool.take() {
                        if let FromQueue::ExecuteBatch(_, batch) = reply {
                            order.extend(batch.tests.iter().cloned());
                        }
                    }
                }
                order
            };

            let vendor = core.vendor();
            let prepared =
                QueueActor::prepare(vendor.as_ref(), analytics.clone(), &configuration, Signal::new()).await?;
            assert_eq!(source.calls.lock().unwrap().len(), 3);
            let live = spawn(prepared);

            h.clock.release_refreshes(1);
            tokio::time::timeout(Duration::from_secs(60), async {
                while h.running.load(Ordering::SeqCst) != 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("the refresh never ended");
            assert_eq!(source.calls.lock().unwrap().len(), 4);

            // All keys equal: the order the same heap gives without any history.
            let mut control = TestQueue::new(Box::new(|_: &Test, _: &Test| std::cmp::Ordering::Equal));
            control.add_all(tests.iter().cloned());
            assert_eq!(
                order(live).await,
                std::iter::from_fn(|| control.poll()).collect::<Vec<_>>()
            );

            let prepared = QueueActor::prepare(vendor.as_ref(), analytics, &configuration, Signal::new()).await?;
            assert_eq!(order(spawn(prepared)).await, tests);
            // The second pool's prepare took what the refresh had cached, without a query.
            assert_eq!(source.calls.lock().unwrap().len(), 4);
            Ok(())
        })
    });
}
