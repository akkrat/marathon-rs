//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/queue/QueueActor.kt: behaviour the Kotlin tests
//! leave out — the per-pool retry quota (C30), close, a failing handler and the D13 prepare step — and the native
//! contract of D68 (console failures) and D69 (finished tests are not run again; C45–C47).
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

// ---- D69: finished tests are not run again ---------------------------------------------------------------------

fn statuses(device: &DeviceInfo, batch: &BatchRef, statuses: &[TestStatus]) -> TestBatchResults {
    let all: Vec<TestResult> = batch
        .tests
        .iter()
        .zip(statuses)
        .map(|(test, status)| result(test, device, "batch", *status, 0, 1))
        .collect();
    let of = |wanted: &[TestStatus]| {
        all.iter()
            .filter(|result| wanted.contains(&result.status))
            .cloned()
            .collect()
    };
    batch_results(
        of(&[TestStatus::Passed, TestStatus::Ignored]),
        of(&[TestStatus::Failure]),
        of(&[TestStatus::Incomplete]),
    )
}
fn retry_setup(
    batching: marathon_config::schema::BatchingStrategyConfiguration,
    mode: marathon_config::schema::ExecutionMode,
    fast: bool,
    quota: (i32, i32),
    tests: Vec<Test>,
    flaky: Vec<Test>,
) -> Setup {
    let mut configuration = configuration();
    configuration.batching_strategy = batching;
    configuration.execution_strategy = marathon_config::schema::ExecutionStrategyConfiguration { mode, fast };
    configuration.retry_strategy = RetryStrategyConfiguration::FixedQuota {
        total_allowed_retry_quota: quota.0,
        retry_per_test_quota: quota.1,
    };
    let mut setup = Setup::new(configuration, tests);
    setup.flaky = flaky;
    setup.pool = "pool".into();
    setup
}
/// D83 interleaves equal-priority copies with other tests, so a test whose copies must all go out first is the
/// longer one under execution-time sorting.
fn longest_first(mut setup: Setup, test: &Test) -> Setup {
    setup.configuration.sorting_strategy = marathon_config::schema::SortingStrategyConfiguration::ExecutionTime {
        percentile: 90.0,
        time_limit: epoch(),
    };
    setup.execution_times.insert(test.clone(), 2000.0);
    setup
}
/// (test, status, final) of each Track since the last call.
fn tracks(queue: &Queue) -> Vec<(serde_json::Value, String, String, bool)> {
    queue
        .tracked()
        .into_iter()
        .map(|event| {
            (
                event[3].clone(),
                event[2].as_str().unwrap().to_owned(),
                event[4].as_str().unwrap().to_owned(),
                event[8].as_bool().unwrap(),
            )
        })
        .collect()
}
fn track(test: &Test, serial: &str, status: &str, last: bool) -> (serde_json::Value, String, String, bool) {
    (t(test), serial.to_owned(), status.to_owned(), last)
}
async fn state(queue: &Queue, test: &Test) -> TestState {
    queue.progress.lock().await.state(test).unwrap()
}
fn snapshot(queue: &Queue) -> marathon_core::execution::queue::QueueSnapshot {
    queue.snapshot.borrow().clone().unwrap()
}
fn failed(total: i32, done: i32, running: i32) -> TestState {
    TestState::Failed { total, done, running }
}
fn passing(total: i32, done: i32, running: i32) -> TestState {
    TestState::Passed { total, done, running }
}

/// D69 (C45): under strict mode (fast ALL_SUCCESS) the failure that is granted a retry also completes the test. The
/// retry is not queued, its attempt leaves the total and its quota is given back. A copy still running elsewhere ends
/// later as a non-final result. Before D69 the test ran a third time and the quota stayed spent.
#[test]
fn a_retry_granted_to_the_attempt_that_completes_the_test_is_not_run_d69() {
    use marathon_config::schema::ExecutionMode::AllSuccess;
    let (c0, c1) = (mtest("pkg", "C0", "m0"), mtest("pkg", "C1", "m1"));
    let setup = longest_first(
        retry_setup(
            fixed_size(1),
            AllSuccess,
            true,
            (10, 3),
            vec![c0.clone(), c0.clone(), c1.clone()],
            vec![],
        ),
        &c0,
    );
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            let (a1, b1) = (device("serial-A1"), device("serial-B1"));
            let first = batch_of(&queue.request(&a1, Some("worker-A")).await[0]);
            let second = batch_of(&queue.request(&b1, Some("worker-B")).await[0]);
            assert_eq!(
                (first.tests.clone(), second.tests.clone()),
                (vec![c0.clone()], vec![c0.clone()])
            );
            queue.trace.drain();

            let replies = queue.complete(&a1, statuses(&a1, &first, &[TestStatus::Failure])).await;
            assert_eq!(replies, [FromQueue::Notify { retry_workers: vec![] }]);
            assert_eq!(tracks(&queue), [track(&c0, "serial-A1", "FAILURE", true)]);
            assert_eq!(state(&queue, &c0).await, failed(2, 1, -1));
            let now = snapshot(&queue);
            assert_eq!((now.queue, now.retry_workers), (vec![c1.clone()], vec![]));
            assert_eq!(now.total_retries_left, Some(10));

            // The copy in flight on serial-B1 ends late, after the test has completed.
            let replies = queue.complete(&b1, statuses(&b1, &second, &[TestStatus::Passed])).await;
            assert_eq!(replies, [FromQueue::Notify { retry_workers: vec![] }]);
            assert_eq!(tracks(&queue), [track(&c0, "serial-B1", "PASSED", false)]);
            assert_eq!(state(&queue, &c0).await, failed(2, 2, -2));

            let last = batch_of(&queue.request(&a1, Some("worker-A")).await[0]);
            assert_eq!(last.tests, std::slice::from_ref(&c1));
            let replies = queue.complete(&a1, statuses(&a1, &last, &[TestStatus::Passed])).await;
            assert_eq!(replies, [FromQueue::Terminated]);
            assert_eq!(tracks(&queue), [track(&c1, "serial-A1", "PASSED", true)]);
            assert_eq!(state(&queue, &c0).await, failed(2, 2, -2));
            assert_eq!(state(&queue, &c1).await, passing(1, 1, -1));
            assert_eq!(snapshot(&queue).total_retries_left, Some(10));
            Ok(())
        })
    });
}

/// D69 (C45): both failed copies of a test in one batch are granted a retry, and the first completes the test. The
/// second copy's retry is not run either: each leaves the total and gives its quota back.
#[test]
fn no_retry_of_a_batch_runs_once_an_earlier_copy_completed_the_test_d69() {
    use marathon_config::schema::{BatchingStrategyConfiguration::ClassName, ExecutionMode::AllSuccess};
    let (m1, m2) = (mtest("pkg", "A", "m1"), mtest("pkg", "A", "m2"));
    let setup = retry_setup(
        ClassName,
        AllSuccess,
        true,
        (10, 3),
        vec![m1.clone(), m2.clone()],
        vec![m1.clone()],
    );
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            let a1 = device("serial-A1");
            let batch = batch_of(&queue.request(&a1, Some("worker-A")).await[0]);
            assert_eq!(batch.tests, [m1.clone(), m2.clone(), m1.clone()]);
            queue.trace.drain();
            let results = statuses(
                &a1,
                &batch,
                &[TestStatus::Failure, TestStatus::Passed, TestStatus::Failure],
            );
            assert_eq!(queue.complete(&a1, results).await, [FromQueue::Terminated]);
            assert_eq!(
                tracks(&queue),
                [
                    track(&m2, "serial-A1", "PASSED", true),
                    track(&m1, "serial-A1", "FAILURE", true),
                    track(&m1, "serial-A1", "FAILURE", false),
                ]
            );
            assert_eq!(state(&queue, &m1).await, failed(2, 2, -2));
            let now = snapshot(&queue);
            assert_eq!((now.queue, now.retry_workers), (vec![], vec![]));
            assert_eq!(now.total_retries_left, Some(10));
            Ok(())
        })
    });
}

/// D69 (C46): two failed copies of one test in a class-name batch, the first retried and the second over the per-test
/// quota. The second is ended too, so the retry's pass completes the test with exact totals. Before D69 it was never
/// ended and the test stayed Passing, one attempt short.
#[test]
fn every_failed_copy_beside_a_retried_copy_is_ended_once_d69() {
    use marathon_config::schema::{BatchingStrategyConfiguration::ClassName, ExecutionMode::AnySuccess};
    let (m1, m2) = (mtest("pkg", "A", "m1"), mtest("pkg", "A", "m2"));
    let setup = retry_setup(
        ClassName,
        AnySuccess,
        false,
        (10, 2),
        vec![m1.clone(), m2.clone()],
        vec![m1.clone()],
    );
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            let a1 = device("serial-A1");
            let batch = batch_of(&queue.request(&a1, Some("worker-A")).await[0]);
            assert_eq!(batch.tests, [m1.clone(), m2.clone(), m1.clone()]);
            queue.trace.drain();
            let results = statuses(
                &a1,
                &batch,
                &[TestStatus::Failure, TestStatus::Passed, TestStatus::Failure],
            );
            assert_eq!(
                queue.complete(&a1, results).await,
                [FromQueue::Notify {
                    retry_workers: vec!["worker-A".into()]
                }]
            );
            assert_eq!(
                tracks(&queue),
                [
                    track(&m2, "serial-A1", "PASSED", true),
                    track(&m1, "serial-A1", "FAILURE", false),
                    track(&m1, "serial-A1", "FAILURE", false),
                ]
            );
            assert_eq!(
                state(&queue, &m1).await,
                TestState::Failing {
                    total: 3,
                    done: 2,
                    running: -2
                }
            );
            let now = snapshot(&queue);
            assert_eq!(now.queue, std::slice::from_ref(&m1));
            assert_eq!(now.retry_workers, [(m1.clone(), vec![String::from("worker-A")])]);
            assert_eq!(now.total_retries_left, Some(9));

            let retry = batch_of(&queue.request(&a1, Some("worker-A")).await[0]);
            assert_eq!(retry.tests, std::slice::from_ref(&m1));
            let replies = queue.complete(&a1, statuses(&a1, &retry, &[TestStatus::Passed])).await;
            assert_eq!(replies, [FromQueue::Terminated]);
            assert_eq!(tracks(&queue), [track(&m1, "serial-A1", "PASSED", true)]);
            assert_eq!(state(&queue, &m1).await, passing(3, 3, -3));
            assert_eq!(snapshot(&queue).total_retries_left, Some(9));
            Ok(())
        })
    });
}

/// D69 (C47): a retry of C0 is rejected by serial-B1 after another copy has completed the test. The copy leaves the
/// queue and the totals, and its retry worker is not restored; the copy still in flight on serial-Z1 ends later as a
/// non-final result and the totals come out exact. Before D69 the rejected copy was queued again and ran after the
/// test had completed.
#[test]
fn a_rejected_copy_of_a_completed_test_is_dropped_d69() {
    use marathon_config::schema::ExecutionMode::AnySuccess;
    let (c0, c1) = (mtest("pkg", "C0", "m0"), mtest("pkg", "C1", "m1"));
    // D83: the queue starts C0, C1, C0, C0.
    let setup = retry_setup(
        fixed_size(1),
        AnySuccess,
        true,
        (10, 3),
        vec![c0.clone(), c0.clone(), c0.clone(), c1.clone()],
        vec![],
    );
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            let [a1, x1, z1, y1, b1] = ["serial-A1", "serial-X1", "serial-Z1", "serial-Y1", "serial-B1"].map(device);
            let on_a1 = batch_of(&queue.request(&a1, Some("worker-A")).await[0]);
            let on_y1 = batch_of(&queue.request(&y1, Some("worker-Y")).await[0]);
            let on_x1 = batch_of(&queue.request(&x1, Some("worker-X")).await[0]);
            let on_z1 = batch_of(&queue.request(&z1, Some("worker-Z")).await[0]);
            let failure = queue.complete(&a1, statuses(&a1, &on_a1, &[TestStatus::Failure])).await;
            assert_eq!(
                failure,
                [FromQueue::Notify {
                    retry_workers: vec!["worker-A".into()]
                }]
            );
            let on_b1 = batch_of(&queue.request(&b1, Some("worker-B")).await[0]);
            assert_eq!(
                [&on_a1, &on_x1, &on_z1, &on_y1, &on_b1].map(|batch| batch.tests.clone()),
                [
                    vec![c0.clone()],
                    vec![c0.clone()],
                    vec![c0.clone()],
                    vec![c1.clone()],
                    vec![c0.clone()]
                ]
            );
            assert_eq!(
                state(&queue, &c0).await,
                TestState::Failing {
                    total: 4,
                    done: 1,
                    running: -1
                }
            );
            queue.trace.drain();

            let completed = queue.complete(&x1, statuses(&x1, &on_x1, &[TestStatus::Passed])).await;
            assert_eq!(completed, []);
            assert_eq!(tracks(&queue), [track(&c0, "serial-X1", "PASSED", true)]);
            assert_eq!(state(&queue, &c0).await, passing(4, 2, -2));

            assert!(queue.send(QueueMessage::RejectBatch(b1.clone(), on_b1.clone())).await);
            assert_eq!(queue.replies(), []);
            let now = snapshot(&queue);
            assert_eq!((now.queue, now.retry_workers), (vec![], vec![]));
            assert_eq!(now.active_batches, ["serial-Y1", "serial-Z1"]);
            assert_eq!(now.total_retries_left, Some(9));
            assert_eq!(state(&queue, &c0).await, passing(3, 2, -2));

            // The copy in flight on serial-Z1 ends late.
            let late = queue.complete(&z1, statuses(&z1, &on_z1, &[TestStatus::Passed])).await;
            assert_eq!(late, []);
            assert_eq!(tracks(&queue), [track(&c0, "serial-Z1", "PASSED", false)]);
            assert_eq!(state(&queue, &c0).await, passing(3, 3, -3));

            let last = queue.complete(&y1, statuses(&y1, &on_y1, &[TestStatus::Passed])).await;
            assert_eq!(last, [FromQueue::Terminated]);
            assert_eq!(tracks(&queue), [track(&c1, "serial-Y1", "PASSED", true)]);
            assert_eq!(state(&queue, &c1).await, passing(1, 1, -1));
            Ok(())
        })
    });
}

/// D69, review round 1: under fast ANY_SUCCESS the copy of C0 on serial-A1 passes and completes the test while the copy
/// on serial-B1 is still out. That copy then comes back INCOMPLETE, as a returned batch or as `uncompleted` in a
/// completed one. It is tracked once, non-final, leaves the totals and is not queued again, and its worker is not marked
/// for a retry. Before this the finished test was queued and ran again (the Kotlin marathon does the same).
#[test]
fn a_late_incomplete_copy_of_a_finished_test_is_not_run_again_d69() {
    use marathon_config::schema::ExecutionMode::AnySuccess;
    for returned in [true, false] {
        let (c0, c1) = (mtest("pkg", "C0", "m0"), mtest("pkg", "C1", "m1"));
        let mut setup = longest_first(
            retry_setup(
                fixed_size(1),
                AnySuccess,
                true,
                (10, 3),
                vec![c0.clone(), c0.clone(), c1.clone()],
                vec![],
            ),
            &c0,
        );
        // The returned batch's one clock read.
        setup.timer = vec![5_000];
        run(move |core| {
            Box::pin(async move {
                let queue = Queue::spawn(&core, setup);
                let (a1, b1) = (device("serial-A1"), device("serial-B1"));
                let on_a1 = batch_of(&queue.request(&a1, Some("worker-A")).await[0]);
                let on_b1 = batch_of(&queue.request(&b1, Some("worker-B")).await[0]);
                assert_eq!(
                    (on_a1.tests.clone(), on_b1.tests.clone()),
                    (vec![c0.clone()], vec![c0.clone()])
                );
                queue.trace.drain();

                let passed = queue.complete(&a1, statuses(&a1, &on_a1, &[TestStatus::Passed])).await;
                assert_eq!(passed, [FromQueue::Notify { retry_workers: vec![] }]);
                assert_eq!(tracks(&queue), [track(&c0, "serial-A1", "PASSED", true)]);
                assert_eq!(state(&queue, &c0).await, passing(2, 1, -1));

                let late = if returned {
                    let reason = String::from("device lost");
                    assert!(
                        queue
                            .send(QueueMessage::ReturnBatch(b1.clone(), on_b1.clone(), reason))
                            .await
                    );
                    queue.replies()
                } else {
                    queue
                        .complete(&b1, statuses(&b1, &on_b1, &[TestStatus::Incomplete]))
                        .await
                };
                assert_eq!(
                    late,
                    [FromQueue::Notify { retry_workers: vec![] }],
                    "returned {returned}"
                );
                assert_eq!(tracks(&queue), [track(&c0, "serial-B1", "INCOMPLETE", false)]);
                assert_eq!(state(&queue, &c0).await, passing(1, 1, -2));
                let now = snapshot(&queue);
                assert_eq!((now.queue, now.retry_workers), (vec![c1.clone()], vec![]));
                assert_eq!(now.active_batches, Vec::<String>::new());

                let last = batch_of(&queue.request(&b1, Some("worker-B")).await[0]);
                assert_eq!(last.tests, std::slice::from_ref(&c1));
                let replies = queue.complete(&b1, statuses(&b1, &last, &[TestStatus::Passed])).await;
                assert_eq!(replies, [FromQueue::Terminated]);
                assert_eq!(tracks(&queue), [track(&c1, "serial-B1", "PASSED", true)]);
                assert_eq!(state(&queue, &c0).await, passing(1, 1, -2));
                assert_eq!(snapshot(&queue).total_retries_left, Some(10));
                Ok(())
            })
        });
    }
}

/// D69, review round 1: the counterpart. An INCOMPLETE copy of a test that has not finished is queued again with its
/// worker marked, as before, and the rerun completes the test with exact totals.
#[test]
fn an_incomplete_copy_of_an_unfinished_test_is_still_run_again_d69() {
    use marathon_config::schema::ExecutionMode::AnySuccess;
    let c0 = mtest("pkg", "C0", "m0");
    let setup = retry_setup(fixed_size(1), AnySuccess, true, (10, 3), vec![c0.clone()], vec![]);
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            let (a1, b1) = (device("serial-A1"), device("serial-B1"));
            let on_a1 = batch_of(&queue.request(&a1, Some("worker-A")).await[0]);
            assert_eq!(on_a1.tests, std::slice::from_ref(&c0));
            queue.trace.drain();

            let back = queue
                .complete(&a1, statuses(&a1, &on_a1, &[TestStatus::Incomplete]))
                .await;
            assert_eq!(
                back,
                [FromQueue::Notify {
                    retry_workers: vec!["worker-A".into()]
                }]
            );
            assert_eq!(tracks(&queue), [track(&c0, "serial-A1", "INCOMPLETE", false)]);
            assert_eq!(state(&queue, &c0).await, TestState::Added { total: 1, running: -1 });
            let now = snapshot(&queue);
            assert_eq!(now.queue, std::slice::from_ref(&c0));
            assert_eq!(now.retry_workers, [(c0.clone(), vec![String::from("worker-A")])]);

            let again = batch_of(&queue.request(&b1, Some("worker-B")).await[0]);
            assert_eq!(again.tests, std::slice::from_ref(&c0));
            let replies = queue.complete(&b1, statuses(&b1, &again, &[TestStatus::Passed])).await;
            assert_eq!(replies, [FromQueue::Terminated]);
            assert_eq!(tracks(&queue), [track(&c0, "serial-B1", "PASSED", true)]);
            assert_eq!(state(&queue, &c0).await, passing(1, 1, -2));
            Ok(())
        })
    });
}
