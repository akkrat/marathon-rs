//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/DevicePoolActorBatchAccountingTest.kt
#[path = "support/pool.rs"]
mod support;
use marathon_core::{
    error::*,
    execution::{DevicePoolMessage, FromDevice},
    model::*,
};
use std::sync::{Arc, Mutex};
use support::*;

fn accounting(names: &[&str]) -> Vec<Test> {
    names.iter().map(|n| mtest("Accounting", n)).collect()
}
fn options(size: i32) -> PoolOptions {
    PoolOptions {
        pool: "test".into(),
        configure: Box::new(move |c| c.batching_strategy = fixed_size(size)),
        ..Default::default()
    }
}
fn strings(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|s| (*s).into()).collect()
}
fn reported_then_lost(gate: &Gate) -> OnExecute {
    let gate = gate.clone();
    on_execute(move |e| {
        let gate = gate.clone();
        async move {
            e.result.complete(Ok(e.device.report(&e.batch, TestStatus::Passed)));
            gate.wait_in(&e.context).await?;
            Err(device_lost(ise("transport lost")))
        }
    })
}

#[test]
fn mixed_batch_reported_before_device_loss_retries_only_eligible_tests_and_counts_quotas_once() {
    scheduling_test(|h| {
        Box::pin(async move {
            let lose = Gate::default();
            let trigger = lose.clone();
            let a = TestDevice::new(
                "serial-A",
                None,
                None,
                on_execute(move |e| {
                    let trigger = trigger.clone();
                    async move {
                        let statuses = e
                            .batch
                            .tests
                            .iter()
                            .map(|t| {
                                (
                                    t.clone(),
                                    match t.method.to_owned().as_str() {
                                        "passed" => TestStatus::Passed,
                                        "failed" => TestStatus::Failure,
                                        _ => TestStatus::Incomplete,
                                    },
                                )
                            })
                            .collect::<Vec<_>>();
                        e.result.complete(Ok(e.device.report_each(&e.batch, &statuses)));
                        trigger.wait_in(&e.context).await?;
                        Err(device_lost(ise("transport lost while collecting artifacts")))
                    }
                }),
            );
            let b = TestDevice::new(
                "serial-B",
                None,
                None,
                on_execute(|e| async move {
                    let statuses = e
                        .batch
                        .tests
                        .iter()
                        .map(|t| {
                            (
                                t.clone(),
                                if t.method == "failed".to_owned() {
                                    TestStatus::Failure
                                } else {
                                    TestStatus::Incomplete
                                },
                            )
                        })
                        .collect::<Vec<_>>();
                    e.result.complete(Ok(e.device.report_each(&e.batch, &statuses)));
                    Ok(())
                }),
            );
            let pool = h.start_pool(
                accounting(&["passed", "failed", "uncompleted"]),
                PoolOptions {
                    pool: "test".into(),
                    configure: Box::new(|c| {
                        c.batching_strategy = fixed_size(3);
                        c.retry_strategy = fixed_quota(10, 1);
                        c.uncompleted_test_retry_quota = 1;
                    }),
                    ..Default::default()
                },
            );
            pool.add(&a);
            h.run_current().await;
            pool.add(&b);
            lose.open();
            h.await_pool_termination().await;
            assert_eq!(a.executed_sorted(), strings(&["failed", "passed", "uncompleted"]));
            assert_eq!(b.executed_sorted(), strings(&["failed", "uncompleted"]));
            assert_eq!(
                h.tracker.events(),
                strings(&[
                    "failed FAILURE final on serial-B",
                    "failed FAILURE non-final on serial-A",
                    "passed PASSED final on serial-A",
                    "uncompleted FAILURE final on serial-B",
                    "uncompleted INCOMPLETE non-final on serial-A"
                ])
            );
            assert!(!pool.aggregate_result().await.unwrap());
        })
    });
}
#[test]
fn results_of_the_last_device_lost_after_reporting_reach_the_final_report() {
    scheduling_test(|h| {
        Box::pin(async move {
            let a = TestDevice::new(
                "serial-A",
                None,
                None,
                on_execute(|e| async move {
                    e.result.complete(Ok(e.device.report_each(
                        &e.batch,
                        &[
                            (mtest("Accounting", "passed"), TestStatus::Passed),
                            (mtest("Accounting", "failed"), TestStatus::Failure),
                        ],
                    )));
                    Err(device_lost(ise("transport lost while collecting artifacts")))
                }),
            );
            let pool = h.start_pool(accounting(&["passed", "failed"]), options(2));
            pool.add(&a);
            h.await_pool_termination().await;
            assert_eq!(a.executed_sorted(), strings(&["failed", "passed"]));
            assert_eq!(
                h.tracker.events(),
                strings(&["failed FAILURE final on serial-A", "passed PASSED final on serial-A"])
            );
        })
    });
}
#[test]
fn results_of_the_last_device_removed_while_finishing_its_batch_reach_the_final_report() {
    scheduling_test(|h| {
        Box::pin(async move {
            let cancelled = Gate::default();
            let signal = cancelled.clone();
            let a = TestDevice::new(
                "serial-A",
                None,
                None,
                on_execute(move |e| {
                    let signal = signal.clone();
                    async move {
                        e.result.complete(Ok(e.device.report(&e.batch, TestStatus::Passed)));
                        let reason = e.context.cancellation.cancelled().await;
                        signal.open();
                        Err(VendorError::Cancelled(reason))
                    }
                }),
            );
            let pool = h.start_pool(accounting(&["passed"]), options(1));
            pool.add(&a);
            h.run_current().await;
            pool.remove(&a);
            h.await_pool_termination().await;
            assert_eq!(h.tracker.events(), strings(&["passed PASSED final on serial-A"]));
            assert!(cancelled.is_open());
            assert!(pool.aggregate_result().await.unwrap());
        })
    });
}
#[test]
fn batch_without_usable_results_is_returned_whole_and_runs_on_another_device() {
    scheduling_test(|h| {
        Box::pin(async move {
            let lose = Gate::default();
            let signal = lose.clone();
            let a = TestDevice::new(
                "serial-A",
                None,
                None,
                on_execute(move |e| {
                    let signal = signal.clone();
                    async move {
                        e.result.cancel(job_was_cancelled());
                        signal.wait_in(&e.context).await?;
                        Err(device_lost(ise("transport lost")))
                    }
                }),
            );
            let b = TestDevice::plain("serial-B");
            let pool = h.start_pool(accounting(&["first", "second"]), options(2));
            pool.add(&a);
            h.run_current().await;
            pool.add(&b);
            lose.open();
            h.await_pool_termination().await;
            assert_eq!(b.executed_sorted(), strings(&["first", "second"]));
            assert_eq!(
                h.tracker.events(),
                strings(&[
                    "first INCOMPLETE non-final on serial-A",
                    "first PASSED final on serial-B",
                    "second INCOMPLETE non-final on serial-A",
                    "second PASSED final on serial-B"
                ])
            );
            assert!(pool.aggregate_result().await.unwrap());
        })
    });
}
async fn two_devices(h: std::rc::Rc<Harness>, fail_child: bool) {
    let lose = Gate::default();
    let a = TestDevice::new("serial-A", None, None, reported_then_lost(&lose));
    let b = TestDevice::new(
        "serial-B",
        None,
        None,
        if fail_child {
            on_execute(|e| async move {
                e.result.complete(Ok(e.device.report(&e.batch, TestStatus::Passed)));
                Err(VendorError::Cancelled(e.context.cancellation.cancelled().await))
            })
        } else {
            reported_then_lost(&lose)
        },
    );
    let pool = h.start_pool(accounting(&["first", "second"]), options(1));
    pool.add(&a);
    h.run_current().await;
    pool.add(&b);
    h.run_current().await;
    if fail_child {
        // 3.6 decision 6: a core child fails the device actor job, preserving the Kotlin parent graph.
        h.fail_actor_on(&pool.actor_of(&b).unwrap(), Some(lose.clone()), "listener crashed");
    }
    lose.open();
    h.await_pool_termination().await;
    assert_eq!(a.executed_tests().len(), 1);
    assert_eq!(
        sorted([a.executed_tests(), b.executed_tests()].concat()),
        strings(&["first", "second"])
    );
    assert_eq!(
        h.tracker.events(),
        sorted(vec![
            format!("{} PASSED final on serial-A", a.executed_tests()[0]),
            format!("{} PASSED final on serial-B", b.executed_tests()[0])
        ])
    );
    assert!(pool.aggregate_result().await.unwrap());
}
#[test]
fn devices_lost_together_keep_the_results_of_both() {
    scheduling_test(|h| Box::pin(two_devices(h, false)));
}
#[test]
fn device_failing_while_another_device_terminates_keeps_its_reported_results() {
    scheduling_test(|h| Box::pin(two_devices(h, true)));
}
#[test]
fn pool_shuts_down_only_after_the_queue_processed_results_forwarded_before_the_last_terminated() {
    for _ in 0..50 {
        scheduling_test(|h| {
            Box::pin(async move {
                let reported = Arc::new(Mutex::new(None));
                let capture = reported.clone();
                let a = TestDevice::new(
                    "serial-A",
                    None,
                    None,
                    on_execute(move |e| {
                        let capture = capture.clone();
                        async move {
                            *lock(&capture) = Some(e.device.report(&e.batch, TestStatus::Passed));
                            Err(VendorError::Cancelled(e.context.cancellation.cancelled().await))
                        }
                    }),
                );
                let pool = h.start_pool(accounting(&["passed"]), options(1));
                pool.add(&a);
                h.run_current().await;
                let results = lock(&reported).take().unwrap();
                // Enqueue both before yielding: the Kotlin CountDownLatch held the sole executor thread here.
                pool.send(DevicePoolMessage::FromDevice(FromDevice::CompletedTestBatch(
                    a.reference(),
                    results,
                )));
                pool.send(DevicePoolMessage::FromDevice(FromDevice::Terminated(a.reference())));
                h.await_pool_termination().await;
                assert_eq!(h.tracker.events(), strings(&["passed PASSED final on serial-A"]));
            })
        });
    }
}
#[test]
fn results_of_a_device_whose_actor_fails_are_kept_and_the_pool_still_shuts_down() {
    scheduling_test(|h| {
        Box::pin(async move {
            let a = TestDevice::new(
                "serial-A",
                None,
                None,
                on_execute(|e| async move {
                    e.result.complete(Ok(e.device.report(&e.batch, TestStatus::Passed)));
                    Err(VendorError::Cancelled(e.context.cancellation.cancelled().await))
                }),
            );
            let pool = h.start_pool(accounting(&["passed"]), options(1));
            pool.add(&a);
            h.run_current().await;
            // 3.6 decision 6: this child has the same device actor parent as Kotlin's failing execution child.
            h.fail_actor_on(&pool.actor_of(&a).unwrap(), None, "listener crashed");
            h.await_pool_termination().await;
            assert_eq!(h.tracker.events(), strings(&["passed PASSED final on serial-A"]));
            assert!(pool.aggregate_result().await.unwrap());
        })
    });
}
