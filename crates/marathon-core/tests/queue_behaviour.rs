//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/queue/QueueActor.kt: behaviour the Kotlin tests
//! leave out — the per-pool retry quota (C30), close, a failing handler and the D13 prepare step — and the native
//! contract of D68 (console failures).
#[path = "support/queue.rs"]
mod support;

use marathon_config::schema::RetryStrategyConfiguration;
use marathon_core::{
    analytics::Analytics,
    error::{OperationError, VendorError},
    execution::{
        FromQueue,
        progress::TestState,
        queue::{QueueActor, QueueMessage, RequestBatch},
    },
    executor::Signal,
    model::*,
    runtime::{Actor, Completion, JobState},
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
/// return left it (P3). A remote backend's failures no longer get this far (D51, `BackendGuard`). This is the actor's
/// half of D68 (U168); its pool and run then end with the cause, as pinned by
/// `pool_scheduler::a_queue_that_fails_on_the_last_result_is_not_a_success_d68` and
/// `queue_failure_run::a_failed_queue_ends_the_run_with_its_cause_while_another_pool_is_alive_d68`.
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

// ---- D68: console failures are not fatal -----------------------------------------------------------------------

const CONSOLE_WARNING: &str = "Cannot write progress to the console; the run carries on. Later failures are not logged";

fn passed(device: &DeviceInfo, batch: &BatchRef) -> TestBatchResults {
    let finished = batch
        .tests
        .iter()
        .map(|test| result(test, device, "batch", TestStatus::Passed, 0, 1))
        .collect();
    batch_results(finished, vec![], vec![])
}
fn console_warnings(queue: &Queue) -> usize {
    lock(&queue.sink.progress_logs)
        .iter()
        .filter(|(level, message)| *level == marathon_logging::Level::WARN && message == CONSOLE_WARNING)
        .count()
}
fn stdout(events: &[serde_json::Value]) -> Vec<String> {
    events
        .iter()
        .filter(|event| event[0] == "stdout")
        .map(|event| event[1].as_str().unwrap().to_owned())
        .collect()
}

/// D68 (U168): `test_ended` flushes twice, after the progress line and after the transition. A failing write at either
/// loses its records only: the transition and the Track still happen, the queue goes on and drains, and one WARN is
/// logged for the run. Before D68 the error ended the queue at the first test.
#[test]
fn a_console_failure_at_either_test_ended_flush_costs_only_its_output_d68() {
    let mut configuration = configuration();
    configuration.batching_strategy = fixed_size(1);
    let (a, b) = (mtest("pkg", "A", "a"), mtest("pkg", "B", "b"));
    let mut setup = Setup::new(configuration, vec![a.clone(), b.clone()]);
    setup.pool = "pool".into();
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            let device = device("serial");
            // The first flush: the write that carries A's line.
            lock(&queue.sink.console_fails).push("pkg.A#a passed".into());
            let batch = batch_of(&queue.request(&device, None).await[0]);
            assert_eq!(batch.tests, std::slice::from_ref(&a));
            let replies = queue.complete(&device, passed(&device, &batch)).await;
            assert_eq!(replies, [FromQueue::Notify { retry_workers: vec![] }]);

            // The second flush: output that arrives while B's line is being written goes out after the transition.
            let (entered_tx, entered) = tokio::sync::oneshot::channel();
            let (release, release_rx) = tokio::sync::oneshot::channel();
            *lock(&queue.sink.console_pause) = Some(ConsolePause {
                text: "pkg.B#b passed".into(),
                entered: entered_tx,
                release: release_rx,
            });
            lock(&queue.sink.console_fails).push("concurrent output".into());
            let batch = batch_of(&queue.request(&device, None).await[0]);
            assert_eq!(batch.tests, std::slice::from_ref(&b));
            assert!(
                queue
                    .handle
                    .try_send(QueueMessage::Completed(device.clone(), passed(&device, &batch)))
            );
            entered.await.unwrap();
            core.log_queue().raw("concurrent output\n".into());
            release.send(()).unwrap();
            queue.barrier().await;
            assert_eq!(queue.replies(), [FromQueue::Terminated]);

            let events = queue.trace.drain();
            assert_eq!(stdout(&events), ["50% | [pool]-[serial] pkg.B#b passed"]);
            let tracks: Vec<_> = events.iter().filter(|event| event[0] == "track").collect();
            assert_eq!(tracks.len(), 2, "{events:?}");
            assert_eq!((&tracks[0][3], &tracks[0][8]), (&t(&a), &serde_json::json!(true)));
            assert_eq!((&tracks[1][3], &tracks[1][8]), (&t(&b), &serde_json::json!(true)));
            assert!(!events.iter().any(|event| event[0] == "handler"), "{events:?}");
            assert_eq!(console_warnings(&queue), 1);
            let progress = queue.progress.lock().await;
            for test in [&a, &b] {
                assert_eq!(
                    progress.state(test),
                    Some(TestState::Passed {
                        total: 1,
                        done: 1,
                        running: -1
                    })
                );
            }
            drop(progress);
            queue.handle.job().join().await.ok();
            assert!(matches!(
                queue.handle.job().state(),
                JobState::Completed(Completion::Cancelled(_))
            ));
            Ok(())
        })
    });
}

/// D68: `test_started` has no production caller (nor has `testStarted` in the Kotlin marathon), but it shares the
/// flushes: a failure before or after its line is not fatal either, and the Started transition happens.
#[test]
fn a_console_failure_at_either_test_started_flush_is_not_fatal_d68() {
    let (a, b) = (mtest("pkg", "A", "a"), mtest("pkg", "B", "b"));
    let mut setup = Setup::new(configuration(), vec![a.clone(), b.clone()]);
    setup.pool = "pool".into();
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            let device = device("serial");
            lock(&queue.sink.console_fails).extend(["earlier output".into(), "pkg.B#b started".into()]);
            let signal = Signal::new();
            let mut progress = queue.progress.lock().await;
            core.log_queue().raw("earlier output\n".into());
            progress.test_started(&device, &a, signal.clone()).await.unwrap();
            progress.test_started(&device, &b, signal.clone()).await.unwrap();
            for test in [&a, &b] {
                assert_eq!(progress.state(test), Some(TestState::Added { total: 1, running: 1 }));
            }
            drop(progress);
            queue.drain_logs();
            assert_eq!(stdout(&queue.trace.drain()), ["00% | [pool]-[serial] pkg.A#a started"]);
            assert_eq!(console_warnings(&queue), 1);
            Ok(())
        })
    });
}

/// D68: a console write that is cancelled still stops `test_ended`, as cancellation does everywhere.
#[test]
fn a_cancelled_console_write_still_cancels_test_ended_d68() {
    let a = mtest("pkg", "A", "a");
    let setup = Setup::new(configuration(), vec![a.clone()]);
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            let device = device("serial");
            lock(&queue.sink.console_cancels).push("pkg.A#a passed".into());
            let mut progress = queue.progress.lock().await;
            let ended = progress
                .test_ended(
                    &device,
                    &result(&a, &device, "batch", TestStatus::Passed, 0, 1),
                    false,
                    Signal::new(),
                )
                .await;
            assert!(matches!(ended, Err(VendorError::Cancelled(_))), "{ended:?}");
            assert_eq!(progress.state(&a), Some(TestState::Added { total: 1, running: 0 }));
            drop(progress);
            assert!(queue.tracked().is_empty());
            assert_eq!(console_warnings(&queue), 0);
            Ok(())
        })
    });
}
