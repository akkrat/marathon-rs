//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/queue/QueueActorRetryPlacementTest.kt
#[path = "support/queue.rs"]
mod support;
use marathon_config::schema::{RetryStrategyConfiguration, SortingStrategyConfiguration};
use marathon_core::{
    execution::{
        FromQueue,
        queue::{QueueMessage, RequestBatch},
    },
    model::*,
};
use std::collections::HashMap;
use support::*;

fn slow() -> Test {
    mtest("pkg", "Placement", "slow")
}
fn fast() -> Test {
    mtest("pkg", "Placement", "fast")
}
fn d(name: &str) -> DeviceInfo {
    device(&format!("serial-{name}"))
}
fn queue_setup(tests: Vec<Test>, flaky: Vec<Test>) -> Setup {
    let mut configuration = configuration();
    configuration.batching_strategy = fixed_size(1);
    configuration.sorting_strategy = SortingStrategyConfiguration::ExecutionTime {
        percentile: 90.0,
        time_limit: epoch(),
    };
    configuration.retry_strategy = RetryStrategyConfiguration::FixedQuota {
        total_allowed_retry_quota: 10,
        retry_per_test_quota: 3,
    };
    configuration.uncompleted_test_retry_quota = 3;
    let mut setup = Setup::new(configuration, tests);
    setup.flaky = flaky;
    setup.execution_times = HashMap::from([(slow(), 2_000.0), (fast(), 1_000.0)]);
    setup.pool = "placement".into();
    setup
}
fn results(device: &DeviceInfo, uncompleted: Option<Test>, failed: Option<Test>) -> TestBatchResults {
    let of = |test: Option<Test>, status| {
        test.map(|test| result(&test, device, "batch", status, 0, 1))
            .into_iter()
            .collect()
    };
    batch_results(
        vec![],
        of(failed, TestStatus::Failure),
        of(uncompleted, TestStatus::Incomplete),
    )
}
/// The tests of the batch the queue hands out, past any Notify.
async fn request(queue: &Queue, device: &DeviceInfo, worker: &str) -> Vec<Test> {
    let replies = queue.request(device, Some(worker)).await;
    let replies: Vec<&FromQueue> = replies
        .iter()
        .filter(|reply| !matches!(reply, FromQueue::Notify { .. }))
        .collect();
    batch_of(replies[0]).tests.clone()
}
fn notify(workers: &[&str]) -> FromQueue {
    FromQueue::Notify {
        retry_workers: workers.iter().map(|w| (*w).into()).collect(),
    }
}

#[test]
fn a_retry_from_another_worker_is_left_to_a_device_on_its_own_worker_only_if_nothing_else_is_queued() {
    // Two copies of the slow test run on worker A and stay incomplete, the fast test fails on worker B
    let setup = queue_setup(vec![slow(), fast()], vec![slow()]);
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            let (a1, a2, b1) = (d("A1"), d("A2"), d("B1"));
            assert_eq!(request(&queue, &a1, "worker-A").await, vec![slow()]);
            assert_eq!(request(&queue, &a2, "worker-A").await, vec![slow()]);
            assert_eq!(request(&queue, &b1, "worker-B").await, vec![fast()]);
            queue
                .send(QueueMessage::Completed(a1.clone(), results(&a1, Some(slow()), None)))
                .await;
            queue
                .send(QueueMessage::Completed(a2.clone(), results(&a2, Some(slow()), None)))
                .await;
            queue
                .send(QueueMessage::Completed(b1.clone(), results(&b1, None, Some(fast()))))
                .await;
            assert_eq!(queue.replies().last(), Some(&notify(&["worker-A", "worker-B"])));

            // A device of a third worker takes one copy of the slow test, which goes first. The other copy still last ran on
            // worker A, so a device of worker A gets the fast test and a device of worker B the remaining slow one
            assert_eq!(request(&queue, &d("C1"), "worker-C").await, vec![slow()]);
            assert_eq!(request(&queue, &d("A3"), "worker-A").await, vec![fast()]);
            assert_eq!(request(&queue, &d("B2"), "worker-B").await, vec![slow()]);
            Ok(())
        })
    });
}

#[test]
fn devices_that_ask_together_leave_a_retry_to_a_device_of_another_worker() {
    let setup = queue_setup(vec![slow(), fast()], vec![]);
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            let (a1, b1) = (d("A1"), d("B1"));
            assert_eq!(request(&queue, &a1, "worker-A").await, vec![slow()]);
            assert_eq!(request(&queue, &b1, "worker-B").await, vec![fast()]);
            queue
                .send(QueueMessage::Completed(a1.clone(), results(&a1, None, Some(slow()))))
                .await;
            queue
                .send(QueueMessage::Completed(b1.clone(), results(&b1, None, Some(fast()))))
                .await;
            queue.replies();

            let (b2, b3, a2) = (d("B2"), d("B3"), d("A2"));
            let requests = [(&b2, "worker-B"), (&b3, "worker-B"), (&a2, "worker-A")]
                .into_iter()
                .map(|(device, worker)| RequestBatch {
                    device: device.clone(),
                    worker_id: Some(worker.into()),
                })
                .collect();
            queue.send(QueueMessage::RequestBatches(requests)).await;
            let replies = queue.replies();
            assert_eq!(replies.len(), 3);

            let batches: HashMap<DeviceInfo, Vec<Test>> = replies
                .iter()
                .filter_map(|reply| match reply {
                    FromQueue::ExecuteBatch(device, batch) => Some((device.clone(), batch.tests.clone())),
                    _ => None,
                })
                .collect();
            assert_eq!(batches, HashMap::from([(b2, vec![slow()]), (a2, vec![fast()])]));
            let parked: Vec<&DeviceInfo> = replies
                .iter()
                .filter_map(|reply| match reply {
                    FromQueue::NoBatch(device) => Some(device),
                    _ => None,
                })
                .collect();
            assert_eq!(parked, vec![&b3]);
            Ok(())
        })
    });
}

#[test]
fn a_rejected_retry_keeps_its_worker() {
    let setup = queue_setup(vec![slow(), fast()], vec![]);
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            let a1 = d("A1");
            assert_eq!(request(&queue, &a1, "worker-A").await, vec![slow()]);
            queue
                .send(QueueMessage::Completed(a1.clone(), results(&a1, None, Some(slow()))))
                .await;
            assert_eq!(queue.replies().last(), Some(&notify(&["worker-A"])));

            // A device of worker C takes the retry and leaves before starting it
            let c1 = d("C1");
            let batch = batch_of(&queue.request(&c1, Some("worker-C")).await[0]);
            assert_eq!(batch.tests, vec![slow()]);
            queue.send(QueueMessage::RejectBatch(c1, batch)).await;

            assert_eq!(queue.replies().last(), Some(&notify(&["worker-A"])));
            assert_eq!(request(&queue, &d("A2"), "worker-A").await, vec![fast()]);
            Ok(())
        })
    });
}

#[test]
fn a_device_gets_a_retry_from_its_own_worker_when_nothing_else_is_queued() {
    let setup = queue_setup(vec![fast()], vec![]);
    run(move |core| {
        Box::pin(async move {
            let queue = Queue::spawn(&core, setup);
            let a1 = d("A1");
            assert_eq!(request(&queue, &a1, "worker-A").await, vec![fast()]);
            queue
                .send(QueueMessage::Completed(a1.clone(), results(&a1, None, Some(fast()))))
                .await;
            assert_eq!(queue.replies().last(), Some(&notify(&["worker-A"])));

            assert_eq!(request(&queue, &d("A2"), "worker-A").await, vec![fast()]);
            Ok(())
        })
    });
}
