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
