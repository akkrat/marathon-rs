//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/queue/QueueActor.kt: behaviour the Kotlin tests
//! leave out — the per-pool retry quota (C30), close, a failing handler and the D13 prepare step.
#[path = "support/queue.rs"]
mod support;

use marathon_config::schema::RetryStrategyConfiguration;
use marathon_core::{
    analytics::Analytics,
    error::OperationError,
    execution::{
        FromQueue,
        queue::{QueueActor, QueueMessage, RequestBatch},
    },
    executor::Signal,
    model::*,
    runtime::Actor,
};
use std::sync::{Arc, atomic::Ordering};
use support::*;

fn quota_setup(pool: &str, total: i32, tests: Vec<Test>) -> Setup {
    let mut configuration = configuration();
    configuration.batching_strategy = fixed_size(2);
    configuration.retry_strategy = RetryStrategyConfiguration::FixedQuota {
        total_allowed_retry_quota: total,
        retry_per_test_quota: 3,
    };
    let mut setup = Setup::new(configuration, tests);
    setup.pool = pool.into();
    setup
}
fn failures(device: &DeviceInfo, batch: &BatchRef) -> TestBatchResults {
    let failed = batch
        .tests
        .iter()
        .map(|test| result(test, device, "batch", TestStatus::Failure, 0, 1))
        .collect();
    batch_results(vec![], failed, vec![])
}

/// C30: each queue builds its own retry strategy, so a total quota of 1 allows one retry per pool.
#[test]
fn retry_quota_applies_per_pool() {
    let first = quota_setup("first", 1, vec![mtest("pkg", "A", "a"), mtest("pkg", "B", "b")]);
    let second = quota_setup("second", 1, vec![mtest("pkg", "C", "c"), mtest("pkg", "D", "d")]);
    run(move |core| {
        Box::pin(async move {
            for setup in [first, second] {
                let queue = Queue::spawn(&core, setup);
                let device = device("serial");
                let batch = batch_of(&queue.request(&device, Some("worker")).await[0]);
                assert_eq!(batch.tests.len(), 2);
                let replies = queue.complete(&device, failures(&device, &batch)).await;
                assert!(matches!(replies[..], [FromQueue::Notify { .. }]), "{replies:?}");
                let retried = batch_of(&queue.request(&device, Some("worker")).await[0]);
                assert_eq!(retried.tests, vec![batch.tests[0].clone()]);
            }
            Ok(())
        })
    });
}

/// close() cancels the job and discards the mailbox; later sends fail (0.6 decision 5).
#[test]
fn close_discards_queued_messages_and_fails_later_sends() {
    let setup = quota_setup("pool", 1, vec![mtest("pkg", "A", "a")]);
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            let (tx, rx) = tokio::sync::oneshot::channel();
            assert!(queue.handle.try_send(QueueMessage::IsEmpty(tx)));
            queue.handle.close();
            assert!(rx.await.is_err(), "a discarded IsEmpty is never answered");
            assert!(!queue.handle.try_send(QueueMessage::Terminate));
            queue.handle.job().join().await.ok();
            assert!(queue.replies().is_empty());
            Ok(())
        })
    });
}

/// A failure at the `Tracker` seam escapes the handler: the queue ends and logs it, the state stays as the early
/// return left it (P3). A remote backend's failures no longer get this far (D51, `BackendGuard`), but a handler
/// that fails for any other reason still ends the queue, and nothing then ends the run (U168).
#[test]
fn handler_failure_ends_the_queue() {
    let setup = quota_setup("pool", 1, vec![mtest("pkg", "A", "a"), mtest("pkg", "B", "b")]);
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            let device = device("serial");
            let batch = batch_of(&queue.request(&device, None).await[0]);
            queue.sink.track_fails.store(true, Ordering::SeqCst);
            let results = batch_results(
                batch
                    .tests
                    .iter()
                    .map(|test| result(test, &device, "batch", TestStatus::Passed, 0, 1))
                    .collect(),
                vec![],
                vec![],
            );
            assert!(queue.send(QueueMessage::Completed(device.clone(), results)).await);
            assert!(queue.replies().is_empty());
            assert!(queue.handle.actor().is_closed_for_send());
            let events = queue.trace.drain();
            assert_eq!(events.iter().filter(|e| e[0] == "track").count(), 1);
            assert!(events.iter().any(|e| e[0] == "handler"), "{events:?}");
            let state = queue.snapshot.borrow().clone().unwrap();
            assert_eq!(state.active_batches, vec![String::from("serial")]);
            assert!(!queue.send(QueueMessage::Terminate).await);
            Ok(())
        })
    });
}

/// prepare prefetches off core and hands the analytics on to spawn.
#[test]
fn prepare_builds_a_queue_from_analytics() {
    let configuration = configuration();
    run(move |core| {
        Box::pin(async move {
            let metrics = Arc::new(Metrics {
                trace: Trace::default(),
                execution_times: Default::default(),
                observed: Default::default(),
            });
            let analytics = Arc::new(Analytics::from_lookup(metrics));
            let prepared =
                QueueActor::prepare(core.vendor().as_ref(), analytics, &configuration, Signal::new()).await?;
            let pool = RecordingPool::new(&core);
            let progress = Queue::spawn(&core, Setup::new(configuration.clone(), vec![])).progress;
            let handle = QueueActor::spawn(
                core.clone(),
                prepared,
                &configuration,
                TestShard {
                    tests: vec![],
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
            assert!(handle.try_send(QueueMessage::RequestBatch(RequestBatch {
                device: device("serial"),
                worker_id: None,
            })));
            handle.job().join().await.ok();
            assert_eq!(pool.take(), vec![FromQueue::Terminated]);
            Ok::<(), OperationError>(())
        })
    });
}
