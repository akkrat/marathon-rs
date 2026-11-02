//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/queue/QueueActorTest.kt
//!
//! The Mockito `Track` and `Analytics` are the recording sink and metrics of support/queue.rs;
//! `verify(track)` counts its `track` events.
#[path = "support/queue.rs"]
mod support;
use marathon_core::{
    execution::{
        FromQueue,
        queue::{QueueMessage, RequestBatch},
    },
    model::*,
};
use serde_json::Value;
use support::*;

fn test_1() -> Test {
    mtest("", "", "test1")
}
fn test_2() -> Test {
    mtest("", "", "test2")
}
fn test_device_info() -> DeviceInfo {
    device("serialNumber")
}
fn other_device_info() -> DeviceInfo {
    device("otherSerialNumber")
}
fn test_result(test: &Test, status: TestStatus) -> TestResult {
    result(test, &test_device_info(), "stub-batch", status, 0, 0)
}

/// uncompleted tests retry quota is `quota`, max batch size is `size` and `tests` in the shard
fn setup(quota: i32, size: i32, tests: Vec<Test>) -> Setup {
    let mut configuration = configuration();
    configuration.uncompleted_test_retry_quota = quota;
    configuration.batching_strategy = fixed_size(size);
    Setup::new(configuration, tests)
}
/// uncompleted tests retry quota is 0, max batch size is 1 and one test in the shard and processing finished
fn setup_1() -> (Setup, TestBatchResults) {
    let results = batch_results(vec![], vec![], vec![test_result(&test_1(), TestStatus::Failure)]);
    (setup(0, 1, vec![test_1()]), results)
}
/// uncompleted tests retry quota is 1, max batch size is 1 and one test in the shard
fn setup_2() -> (Setup, TestBatchResults) {
    let results = batch_results(vec![], vec![], vec![test_result(&test_1(), TestStatus::Incomplete)]);
    (setup(1, 1, vec![test_1()]), results)
}
fn request(device: &DeviceInfo) -> QueueMessage {
    QueueMessage::RequestBatch(RequestBatch {
        device: device.clone(),
        worker_id: None,
    })
}
fn completed(results: &TestBatchResults) -> QueueMessage {
    QueueMessage::Completed(test_device_info(), results.clone())
}
fn tracked_status(event: &Value) -> (Value, &str) {
    (event[3].clone(), event[4].as_str().unwrap())
}

#[test]
fn setup_1_should_terminate_once_the_queue_is_drained() {
    let (setup, results) = setup_1();
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            queue.request(&test_device_info(), None).await;
            assert_eq!(
                queue.complete(&test_device_info(), results).await,
                vec![FromQueue::Terminated]
            );
            Ok(())
        })
    });
}

#[test]
fn setup_1_should_report_failure() {
    let (setup, results) = setup_1();
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            queue.request(&test_device_info(), None).await;
            queue.send(completed(&results)).await;
            let tracked = queue.tracked();
            assert_eq!(tracked.len(), 1);
            assert_eq!(tracked_status(&tracked[0]), (t(&test_1()), "FAILURE"));
            Ok(())
        })
    });
}

#[test]
fn setup_2_should_have_non_empty_queue() {
    let (setup, results) = setup_2();
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            assert!(matches!(
                queue.request(&test_device_info(), None).await[..],
                [FromQueue::ExecuteBatch(..)]
            ));
            assert!(matches!(
                queue.complete(&test_device_info(), results).await[..],
                [FromQueue::Notify { .. }]
            ));
            assert_eq!(queue.is_empty().await, Some(false));
            Ok(())
        })
    });
}

#[test]
fn setup_2_should_report_test_failed() {
    let (setup, results) = setup_2();
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            assert!(matches!(
                queue.request(&test_device_info(), None).await[..],
                [FromQueue::ExecuteBatch(..)]
            ));
            assert!(matches!(
                queue.complete(&test_device_info(), results).await[..],
                [FromQueue::Notify { .. }]
            ));
            let tracked = queue.tracked();
            assert_eq!(tracked.len(), 1);
            assert_eq!(tracked_status(&tracked[0]), (t(&test_1()), "INCOMPLETE"));
            Ok(())
        })
    });
}

#[test]
fn setup_2_should_provide_uncompleted_test_in_the_batch() {
    let (setup, results) = setup_2();
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            assert!(matches!(
                queue.request(&test_device_info(), None).await[..],
                [FromQueue::ExecuteBatch(..)]
            ));
            assert!(matches!(
                queue.complete(&test_device_info(), results).await[..],
                [FromQueue::Notify { .. }]
            ));
            let actual = queue.request(&test_device_info(), None).await;
            assert_eq!(actual.len(), 1);
            assert_eq!(batch_of(&actual[0]).tests, vec![test_1()]);
            Ok(())
        })
    });
}

#[test]
fn setup_2_should_terminate_once_the_queue_is_drained() {
    let (setup, results) = setup_2();
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            assert!(matches!(
                queue.request(&test_device_info(), None).await[..],
                [FromQueue::ExecuteBatch(..)]
            ));
            queue.send(completed(&results)).await;
            queue.send(request(&test_device_info())).await;
            assert!(matches!(
                queue.replies()[..],
                [FromQueue::Notify { .. }, FromQueue::ExecuteBatch(..)]
            ));
            assert_eq!(
                queue.complete(&test_device_info(), results).await,
                vec![FromQueue::Terminated]
            );
            Ok(())
        })
    });
}

#[test]
fn request_without_a_batch_to_give_is_answered() {
    let (setup, _) = setup_1();
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            assert!(matches!(
                queue.request(&test_device_info(), None).await[..],
                [FromQueue::ExecuteBatch(..)]
            ));
            assert_eq!(
                queue.request(&other_device_info(), None).await,
                vec![FromQueue::NoBatch(other_device_info())]
            );
            Ok(())
        })
    });
}

#[test]
fn request_of_a_device_with_an_active_batch_is_answered_without_a_batch() {
    let setup = setup(
        configuration().uncompleted_test_retry_quota,
        1,
        vec![test_1(), test_2()],
    );
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            assert!(matches!(
                queue.request(&test_device_info(), None).await[..],
                [FromQueue::ExecuteBatch(..)]
            ));
            assert_eq!(
                queue.request(&test_device_info(), None).await,
                vec![FromQueue::NoBatch(test_device_info())]
            );
            match &queue.request(&other_device_info(), None).await[..] {
                [FromQueue::ExecuteBatch(device, _)] => assert_eq!(*device, other_device_info()),
                other => panic!("{other:?}"),
            }
            Ok(())
        })
    });
}

#[test]
fn rejected_batch_is_queued_again_without_counting_an_attempt() {
    let (setup, _) = setup_1();
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            let assigned = batch_of(&queue.request(&test_device_info(), None).await[0]);
            queue
                .send(QueueMessage::RejectBatch(test_device_info(), assigned))
                .await;
            assert!(matches!(queue.replies()[..], [FromQueue::Notify { .. }]));
            match &queue.request(&other_device_info(), None).await[..] {
                [FromQueue::ExecuteBatch(device, batch)] => {
                    assert_eq!(*device, other_device_info());
                    assert_eq!(batch.tests, vec![test_1()]);
                }
                other => panic!("{other:?}"),
            }
            assert!(queue.tracked().is_empty());
            Ok(())
        })
    });
}

#[test]
fn setup_2_should_report_test_as_failed() {
    let (setup, results) = setup_2();
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            queue.request(&test_device_info(), None).await;
            queue.send(completed(&results)).await;
            let tracked = queue.tracked();
            assert_eq!(tracked.len(), 1);
            assert_eq!(tracked_status(&tracked[0]), (t(&test_1()), "INCOMPLETE"));
            Ok(())
        })
    });
}

#[test]
fn completed_batch_results_are_passed_to_analytics() {
    let setup = setup(
        configuration().uncompleted_test_retry_quota,
        2,
        vec![test_1(), test_2()],
    );
    let passed = test_result(&test_1(), TestStatus::Passed);
    let failed = test_result(&test_2(), TestStatus::Failure);
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            assert!(matches!(
                queue.request(&test_device_info(), None).await[..],
                [FromQueue::ExecuteBatch(..)]
            ));
            let results = batch_results(vec![passed.clone()], vec![failed.clone()], vec![]);
            assert_eq!(
                queue.complete(&test_device_info(), results).await,
                vec![FromQueue::Terminated]
            );
            assert_eq!(*lock(&queue.metrics.observed), vec![vec![passed, failed]]);
            Ok(())
        })
    });
}
