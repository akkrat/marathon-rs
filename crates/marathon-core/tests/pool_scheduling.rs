//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/DevicePoolActorSchedulingTest.kt
//!
//! Scheduling of batches to the devices of a pool through the real pool, queue and device
//! actors on virtual time; `runCurrent`/`advanceUntilIdle` are the replay driver's.
#[path = "support/pool.rs"]
mod pool;
use marathon_core::{error::VendorError, model};
use pool::*;

#[test]
fn all_simultaneously_ready_devices_start_distinct_batches_before_any_batch_completes() {
    scheduling_test(|h| {
        Box::pin(async move {
            let prepared = Gate::default();
            let devices: Vec<_> = (1..=64)
                .map(|i| {
                    TestDevice::new(
                        &format!("device-{i}"),
                        None,
                        Some(prepare_on(&prepared)),
                        await_cancellation(),
                    )
                })
                .collect();
            let pool = h.start_pool(tests(256, "Scheduling"), PoolOptions::default());

            for d in &devices {
                pool.add(d);
                h.run_current().await;
            }
            h.run_current().await;
            assert!(devices.iter().all(|d| d.batches().is_empty()));

            prepared.open();
            h.run_current().await;

            assert_eq!(
                devices.iter().map(|d| d.batches().len()).collect::<Vec<_>>(),
                vec![1; 64]
            );
            let mut started: Vec<String> = devices.iter().flat_map(|d| d.executed_tests()).collect();
            started.sort();
            started.dedup();
            assert_eq!(started.len(), 64);
        })
    });
}

#[test]
fn pool_terminates_when_devices_leave_before_they_are_ready() {
    scheduling_test(|h| {
        Box::pin(async move {
            let prepared = Gate::default();
            let devices: Vec<_> = (1..=2)
                .map(|i| {
                    TestDevice::new(
                        &format!("device-{i}"),
                        None,
                        Some(prepare_on(&prepared)),
                        report_passed(),
                    )
                })
                .collect();
            let pool = h.start_pool(tests(2, "Scheduling"), PoolOptions::default());
            for d in &devices {
                pool.add(d);
                h.run_current().await;
            }
            h.run_current().await;
            devices.iter().for_each(|d| pool.remove(d));
            h.advance_until_idle().await;

            assert!(devices.iter().all(|d| d.batches().is_empty()));
            assert!(pool.terminated());
        })
    });
}

fn quota(total: i32, per_test: i32) -> PoolOptions {
    PoolOptions {
        configure: Box::new(move |c| c.retry_strategy = fixed_quota(total, per_test)),
        ..PoolOptions::default()
    }
}
fn sizes(devices: &[&std::sync::Arc<TestDevice>]) -> Vec<usize> {
    devices.iter().map(|d| d.batches().len()).collect()
}
fn s(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}
fn worker(serial: &str, worker: &str, prepare: Option<OnPrepare>, execute: OnExecute) -> std::sync::Arc<TestDevice> {
    TestDevice::new(serial, Some(worker), prepare, execute)
}

#[test]
fn repeated_readiness_and_queue_notifications_neither_duplicate_assignments_nor_leave_devices_idle() {
    scheduling_test(|h| {
        Box::pin(async move {
            let devices: Vec<_> = (1..=3)
                .map(|i| TestDevice::new(&format!("device-{i}"), None, None, finish_on_signal()))
                .collect();
            let all: Vec<_> = devices.iter().collect();
            let pool = h.start_pool(tests(9, "Scheduling"), PoolOptions::default());
            for d in &devices {
                pool.add(d);
                h.run_current().await;
            }
            h.run_current().await;
            assert_eq!(sizes(&all), vec![1, 1, 1]);

            for _ in 0..3 {
                devices.iter().for_each(|d| pool.send(is_ready(d)));
                pool.send(notify());
            }
            h.run_current().await;
            assert_eq!(sizes(&all), vec![1, 1, 1]);

            devices[0].finish.send();
            h.run_current().await;
            assert_eq!(sizes(&all), vec![2, 1, 1]);

            for _ in 0..3 {
                pool.send(is_ready(&devices[0]));
            }
            h.run_current().await;
            assert_eq!(sizes(&all), vec![2, 1, 1]);

            for device in &devices {
                for _ in 0..9 {
                    device.finish.send();
                }
            }
            h.advance_until_idle().await;
            let executed: Vec<String> = devices.iter().flat_map(|d| d.executed_tests()).collect();
            assert_eq!(sorted(executed), sorted(methods(&tests(9, "Scheduling"))));
            assert_eq!(pool.aggregate_result().await, Ok(true));
            assert!(pool.terminated());
        })
    });
}

#[test]
fn retry_runs_on_an_idle_device_rather_than_on_the_device_that_has_just_failed_it() {
    scheduling_test(|h| {
        Box::pin(async move {
            let ready_b = Gate::default();
            let fail_a = Gate::default();
            let device_a = TestDevice::new("serial-A", None, None, fail_on(&fail_a));
            let device_b = TestDevice::new("serial-B", None, Some(prepare_on(&ready_b)), report_passed());
            let pool = h.start_pool(tests(1, "Scheduling"), quota(1, 1));

            pool.add(&device_a);
            h.run_current().await;
            pool.add(&device_b);
            h.run_current().await;
            ready_b.open();
            h.run_current().await;
            assert_eq!(device_a.batches().len(), 1);
            assert!(device_b.batches().is_empty());

            fail_a.open();
            h.advance_until_idle().await;

            assert_eq!(device_a.executed_tests(), s(&["test1"]));
            assert_eq!(device_b.executed_tests(), s(&["test1"]));
            assert_eq!(
                h.tracker.events(),
                s(&["test1 FAILURE non-final on serial-A", "test1 PASSED final on serial-B"])
            );
            assert!(pool.terminated());
        })
    });
}

#[test]
fn retry_runs_on_an_idle_device_of_another_worker_rather_than_on_one_of_the_worker_that_has_just_failed_it() {
    scheduling_test(|h| {
        Box::pin(async move {
            let fail_a1 = Gate::default();
            let ready_a2 = Gate::default();
            let ready_b = Gate::default();
            let device_a1 = worker("serial-A1", "worker-A", None, fail_on(&fail_a1));
            let device_a2 = worker("serial-A2", "worker-A", Some(prepare_on(&ready_a2)), report_passed());
            let device_b = worker("serial-B", "worker-B", Some(prepare_on(&ready_b)), report_passed());
            let pool = h.start_pool(tests(1, "Scheduling"), quota(1, 1));

            for d in &[&device_a1, &device_a2, &device_b] {
                pool.add(d);
                h.run_current().await;
            }
            h.run_current().await;
            // A2 has been idle longer than B
            ready_a2.open();
            h.run_current().await;
            ready_b.open();
            h.run_current().await;
            assert_eq!(device_a1.batches().len(), 1);

            fail_a1.open();
            h.advance_until_idle().await;

            assert_eq!(device_b.executed_tests(), s(&["test1"]));
            assert!(device_a2.batches().is_empty());
            assert_eq!(
                h.tracker.events(),
                s(&["test1 FAILURE non-final on serial-A1", "test1 PASSED final on serial-B"])
            );
            assert!(pool.terminated());
        })
    });
}

#[test]
fn retries_of_batches_that_failed_together_on_one_worker_run_on_idle_devices_of_other_workers() {
    scheduling_test(|h| {
        Box::pin(async move {
            let fail = Gate::default();
            let ready_b = Gate::default();
            let ready_c = Gate::default();
            let device_a1 = worker("serial-A1", "worker-A", None, fail_on(&fail));
            let device_a2 = worker("serial-A2", "worker-A", None, fail_on(&fail));
            let device_b = worker("serial-B", "worker-B", Some(prepare_on(&ready_b)), report_passed());
            let device_c = worker("serial-C", "worker-C", Some(prepare_on(&ready_c)), report_passed());
            let pool = h.start_pool(tests(2, "Scheduling"), quota(2, 1));

            for d in &[&device_a1, &device_a2, &device_b, &device_c] {
                pool.add(d);
                h.run_current().await;
            }
            h.run_current().await;
            ready_b.open();
            ready_c.open();
            h.run_current().await;
            assert_eq!(sizes(&[&device_a1, &device_a2]), vec![1, 1]);

            fail.open();
            h.advance_until_idle().await;

            let retried = [device_b.executed_tests(), device_c.executed_tests()].concat();
            assert_eq!(sorted(retried), s(&["test1", "test2"]));
            assert_eq!(sizes(&[&device_a1, &device_a2]), vec![1, 1]);
            assert_eq!(pool.aggregate_result().await, Ok(true));
            assert!(pool.terminated());
        })
    });
}

#[test]
fn retries_of_batches_that_failed_together_on_different_workers_swap_workers() {
    scheduling_test(|h| {
        Box::pin(async move {
            let fail = Gate::default();
            let ready_a2 = Gate::default();
            let ready_b2 = Gate::default();
            let finish = Gate::default();
            let passed = finish.clone();
            let passing = on_execute(move |e| {
                let passed = passed.clone();
                async move {
                    passed.wait_in(&e.context).await?;
                    e.result
                        .complete(Ok(e.device.report(&e.batch, model::TestStatus::Passed)));
                    Ok(())
                }
            });
            let device_a1 = worker("serial-A1", "worker-A", None, fail_on(&fail));
            let device_b1 = worker("serial-B1", "worker-B", None, fail_on(&fail));
            let device_a2 = worker("serial-A2", "worker-A", Some(prepare_on(&ready_a2)), passing.clone());
            let device_b2 = worker("serial-B2", "worker-B", Some(prepare_on(&ready_b2)), passing);
            let pool = h.start_pool(tests(2, "Scheduling"), quota(2, 1));

            for d in &[&device_a1, &device_b1, &device_a2, &device_b2] {
                pool.add(d);
                h.run_current().await;
            }
            h.run_current().await;
            ready_a2.open();
            h.run_current().await;
            ready_b2.open();
            h.run_current().await;

            fail.open();
            // A fast successful retry can ask again before the other failure notifies parked devices
            // (as in U99). Keep both retries in flight until the shared failure wave has settled.
            h.run_current().await;
            assert_eq!(device_a2.batches().len(), 1);
            assert_eq!(device_b2.batches().len(), 1);
            finish.open();
            h.advance_until_idle().await;

            assert_eq!(device_b2.executed_tests(), device_a1.executed_tests());
            assert_eq!(device_a2.executed_tests(), device_b1.executed_tests());
            assert_eq!(sizes(&[&device_a1, &device_b1]), vec![1, 1]);
            assert_eq!(pool.aggregate_result().await, Ok(true));
            assert!(pool.terminated());
        })
    });
}

#[test]
fn a_retry_of_one_of_two_batches_that_failed_together_runs_on_an_idle_device_of_another_worker() {
    scheduling_test(|h| {
        Box::pin(async move {
            let fail = Gate::default();
            let ready_a2 = Gate::default();
            let ready_b2 = Gate::default();
            let devices = [
                worker("serial-A1", "worker-A", None, fail_on(&fail)),
                worker("serial-B1", "worker-B", None, fail_on(&fail)),
                worker("serial-A2", "worker-A", Some(prepare_on(&ready_a2)), report_passed()),
                worker("serial-B2", "worker-B", Some(prepare_on(&ready_b2)), report_passed()),
            ];
            // Only the first failure gets a retry
            let pool = h.start_pool(tests(2, "Scheduling"), quota(1, 1));

            for d in &devices {
                pool.add(d);
                h.run_current().await;
            }
            h.run_current().await;
            ready_a2.open();
            h.run_current().await;
            ready_b2.open();
            h.run_current().await;

            fail.open();
            h.advance_until_idle().await;

            let runs: Vec<(String, Option<String>)> = devices
                .iter()
                .flat_map(|d| d.executed_tests().into_iter().map(|t| (t, d.worker_id.clone())))
                .collect();
            let mut by_test: indexmap::IndexMap<String, Vec<Option<String>>> = indexmap::IndexMap::new();
            for (test, worker) in runs {
                by_test.entry(test).or_default().push(worker);
            }
            let retried: Vec<_> = by_test.values().filter(|w| w.len() == 2).collect();
            assert_eq!(retried.len(), 1);
            assert_ne!(retried[0][1], retried[0][0]);
            assert_eq!(devices.iter().map(|d| d.batches().len()).sum::<usize>(), 3);
            assert!(pool.terminated());
        })
    });
}

#[test]
fn idle_devices_that_ask_together_leave_each_retry_to_another_worker() {
    scheduling_test(|h| {
        Box::pin(async move {
            let fail = Gate::default();
            let ready = Gate::default();
            let finish = Gate::default();
            let passed = finish.clone();
            let passing = on_execute(move |e| {
                let passed = passed.clone();
                async move {
                    passed.wait_in(&e.context).await?;
                    e.result
                        .complete(Ok(e.device.report(&e.batch, model::TestStatus::Passed)));
                    Ok(())
                }
            });
            let device_a1 = worker("serial-A1", "worker-A", None, fail_on(&fail));
            let device_b1 = worker("serial-B1", "worker-B", None, fail_on(&fail));
            let device_a2 = worker("serial-A2", "worker-A", Some(prepare_on(&ready)), passing.clone());
            let device_b2 = worker("serial-B2", "worker-B", Some(prepare_on(&ready)), passing.clone());
            let device_b3 = worker("serial-B3", "worker-B", Some(prepare_on(&ready)), passing.clone());
            let pool = h.start_pool(tests(2, "Scheduling"), quota(2, 1));

            for d in &[&device_a1, &device_b1, &device_a2, &device_b2, &device_b3] {
                pool.add(d);
                h.run_current().await;
            }
            h.run_current().await;
            ready.open();
            h.run_current().await;

            fail.open();
            // A fast successful retry can ask again before the other failure notifies parked devices.
            // Keep both retries in flight until the shared failure wave has settled, as on the JVM dispatcher.
            h.run_current().await;
            assert_eq!(device_a2.batches().len(), 1);
            assert_eq!(device_b2.batches().len() + device_b3.batches().len(), 1);
            finish.open();
            h.advance_until_idle().await;

            assert_eq!(device_a2.executed_tests(), device_b1.executed_tests());
            assert_eq!(
                [device_b2.executed_tests(), device_b3.executed_tests()].concat(),
                device_a1.executed_tests()
            );
            assert_eq!(sizes(&[&device_a1, &device_b1]), vec![1, 1]);
            assert_eq!(pool.aggregate_result().await, Ok(true));
            assert!(pool.terminated());
        })
    });
}

#[test]
fn retries_of_a_batch_that_failed_on_one_worker_run_on_all_idle_devices_of_other_workers() {
    scheduling_test(|h| {
        Box::pin(async move {
            let fail_a = Gate::default();
            let device_a = worker("serial-A", "worker-A", None, fail_on(&fail_a));
            let finish_b = Gate::default();
            let finish = finish_b.clone();
            let device_b = worker(
                "serial-B",
                "worker-B",
                None,
                on_execute(move |e| {
                    let finish = finish.clone();
                    async move {
                        finish.wait_in(&e.context).await?;
                        e.result
                            .complete(Ok(e.device.report(&e.batch, model::TestStatus::Passed)));
                        Ok(())
                    }
                }),
            );
            let device_c = worker("serial-C", "worker-C", None, report_passed());
            // A gets two tests, B the third one. The two retries are in the last mile, so they make two batches
            let pool = h.start_pool(
                tests(3, "Scheduling"),
                PoolOptions {
                    configure: Box::new(|c| {
                        c.batching_strategy = marathon_config::schema::BatchingStrategyConfiguration::FixedSize {
                            size: 2,
                            duration_millis: None,
                            percentile: None,
                            time_limit: None,
                            last_mile_length: 3,
                        };
                        c.retry_strategy = fixed_quota(2, 1);
                    }),
                    ..PoolOptions::default()
                },
            );

            for d in &[&device_a, &device_b, &device_c] {
                pool.add(d);
                h.run_current().await;
            }
            h.run_current().await;
            assert_eq!(device_a.batches().len(), 1);
            assert_eq!(device_a.batches()[0].len(), 2);
            assert_eq!(device_b.batches().len(), 1);
            assert!(device_c.batches().is_empty());

            // Preserve the JVM checkpoint: C is parked before B's first completion.
            finish_b.open();
            h.run_current().await;
            fail_a.open();
            h.advance_until_idle().await;

            assert_eq!(device_a.batches().len(), 1);
            assert_eq!(device_b.batches().len(), 2);
            assert_eq!(device_c.batches().len(), 1);
            let retried = [device_b.batches()[1].clone(), device_c.batches()[0].clone()].concat();
            assert_eq!(sorted(retried), sorted(device_a.batches()[0].clone()));
            assert_eq!(pool.aggregate_result().await, Ok(true));
            assert!(pool.terminated());
        })
    });
}

#[test]
fn batch_returned_by_a_lost_device_runs_on_an_idle_device_of_another_worker() {
    scheduling_test(|h| {
        Box::pin(async move {
            let lose_a1 = Gate::default();
            let ready_a2 = Gate::default();
            let ready_b = Gate::default();
            let lose = lose_a1.clone();
            let device_a1 = worker(
                "serial-A1",
                "worker-A",
                None,
                on_execute(move |e| {
                    let lose = lose.clone();
                    async move {
                        lose.wait_in(&e.context).await?;
                        Err(device_lost(ise("simulator lost")))
                    }
                }),
            );
            let device_a2 = worker("serial-A2", "worker-A", Some(prepare_on(&ready_a2)), report_passed());
            let device_b = worker("serial-B", "worker-B", Some(prepare_on(&ready_b)), report_passed());
            let pool = h.start_pool(tests(1, "Scheduling"), PoolOptions::default());

            for d in &[&device_a1, &device_a2, &device_b] {
                pool.add(d);
                h.run_current().await;
            }
            h.run_current().await;
            ready_a2.open();
            h.run_current().await;
            ready_b.open();
            h.run_current().await;

            lose_a1.open();
            h.advance_until_idle().await;

            assert_eq!(device_b.executed_tests(), s(&["test1"]));
            assert!(device_a2.batches().is_empty());
            assert_eq!(
                h.tracker.events(),
                s(&[
                    "test1 INCOMPLETE non-final on serial-A1",
                    "test1 PASSED final on serial-B"
                ])
            );
            assert!(pool.terminated());
        })
    });
}

#[test]
fn retry_runs_on_an_idle_device_of_the_same_worker_when_no_other_worker_has_one() {
    scheduling_test(|h| {
        Box::pin(async move {
            let fail_a1 = Gate::default();
            let ready_a2 = Gate::default();
            let device_a1 = worker("serial-A1", "worker-A", None, fail_on(&fail_a1));
            let device_a2 = worker("serial-A2", "worker-A", Some(prepare_on(&ready_a2)), report_passed());
            let device_b = worker("serial-B", "worker-B", None, finish_on_signal());
            let pool = h.start_pool(tests(2, "Scheduling"), quota(1, 1));

            for d in &[&device_a1, &device_b, &device_a2] {
                pool.add(d);
                h.run_current().await;
            }
            h.run_current().await;
            ready_a2.open();
            h.run_current().await;
            assert!(device_a2.batches().is_empty());

            fail_a1.open();
            h.run_current().await;

            // B is busy, so the retry doesn't wait for it
            assert_eq!(device_a2.executed_tests(), device_a1.executed_tests());
            assert_eq!(device_a1.batches().len(), 1);
            device_b.finish.send();
            h.advance_until_idle().await;
            assert_eq!(pool.aggregate_result().await, Ok(true));
            assert!(pool.terminated());
        })
    });
}

#[test]
fn retry_runs_on_the_device_that_has_just_failed_it_when_no_other_device_is_idle() {
    scheduling_test(|h| {
        Box::pin(async move {
            let fail_a = Gate::default();
            let gate = fail_a.clone();
            let device_a = TestDevice::new(
                "serial-A",
                None,
                None,
                on_execute(move |e| {
                    let gate = gate.clone();
                    async move {
                        gate.wait_in(&e.context).await?;
                        let status = if e.device.batches().len() == 1 {
                            model::TestStatus::Failure
                        } else {
                            model::TestStatus::Passed
                        };
                        e.result.complete(Ok(e.device.report(&e.batch, status)));
                        Ok(())
                    }
                }),
            );
            let device_b = TestDevice::new("serial-B", None, None, finish_on_signal());
            let pool = h.start_pool(tests(2, "Scheduling"), quota(1, 1));

            pool.add(&device_a);
            h.run_current().await;
            pool.add(&device_b);
            h.run_current().await;
            fail_a.open();
            h.run_current().await;

            assert_eq!(device_a.batches().len(), 2);
            assert_eq!(device_a.batches()[1], device_a.batches()[0]);
            assert_eq!(device_b.batches().len(), 1);
            device_b.finish.send();
            h.advance_until_idle().await;
            assert_eq!(pool.aggregate_result().await, Ok(true));
            assert!(pool.terminated());
        })
    });
}

#[test]
fn pool_terminates_when_the_last_batch_ends_on_a_lost_device_while_another_device_waits() {
    scheduling_test(|h| {
        Box::pin(async move {
            let ready_b = Gate::default();
            let lose_a = Gate::default();
            let lose = lose_a.clone();
            let device_a = TestDevice::new(
                "serial-A",
                None,
                None,
                on_execute(move |e| {
                    let lose = lose.clone();
                    async move {
                        e.result
                            .complete(Ok(e.device.report(&e.batch, model::TestStatus::Passed)));
                        lose.wait_in(&e.context).await?;
                        Err(device_lost(ise("transport lost")))
                    }
                }),
            );
            let device_b = TestDevice::new("serial-B", None, Some(prepare_on(&ready_b)), report_passed());
            let pool = h.start_pool(tests(1, "Scheduling"), PoolOptions::default());

            pool.add(&device_a);
            h.run_current().await;
            pool.add(&device_b);
            h.run_current().await;
            ready_b.open();
            h.run_current().await;
            lose_a.open();
            h.advance_until_idle().await;

            assert!(device_b.batches().is_empty());
            assert_eq!(h.tracker.events(), s(&["test1 PASSED final on serial-A"]));
            assert_eq!(pool.aggregate_result().await, Ok(true));
            assert!(pool.terminated());
        })
    });
}

fn estimating(before_execute: marathon_core::execution::queue::BeforeExecute) -> PoolOptions {
    PoolOptions {
        configure: Box::new(|c| {
            c.batching_strategy = estimating_batches();
            c.uncompleted_test_retry_quota = 0;
        }),
        before_execute: Some(before_execute),
        ..PoolOptions::default()
    }
}

// The three EstimateHook tests reach the moment the queue picks a batch through a metrics
// provider. Under D13 the estimates are prefetched before the queue exists, so the hook is
// the queue's test-only `before_execute`, which runs at the same point (WP 3.8 decision 2).

#[test]
fn batch_picked_for_a_device_that_has_left_runs_on_another_device_without_using_an_attempt() {
    scheduling_test(|h| {
        Box::pin(async move {
            let device_a = TestDevice::plain("serial-A");
            let device_b = TestDevice::plain("serial-B");
            let left = std::rc::Rc::new(std::cell::Cell::new(false));
            let slot: std::rc::Rc<std::cell::RefCell<Option<std::rc::Rc<marathon_core::executor::RuntimeActor<_>>>>> =
                Default::default();
            // The device leaves while the queue is picking its batch, so the batch arrives after the device has gone
            let (flag, target, a, b) = (left.clone(), slot.clone(), device_a.clone(), device_b.clone());
            let pool = h.start_pool(
                tests(1, "Scheduling"),
                estimating(std::rc::Rc::new(move |_, _| {
                    if !flag.get() {
                        flag.set(true);
                        if let Some(pool) = target.borrow().as_ref() {
                            let _ = marathon_core::runtime::Actor::try_send(pool.as_ref(), remove(&a));
                            let _ = marathon_core::runtime::Actor::try_send(pool.as_ref(), add(&b));
                        }
                    }
                })),
            );
            *slot.borrow_mut() = Some(pool.pool.actor.clone());

            pool.add(&device_a);
            h.advance_until_idle().await;

            assert!(left.get());
            assert!(device_a.batches().is_empty());
            assert_eq!(device_b.executed_tests(), s(&["test1"]));
            assert_eq!(h.tracker.events(), s(&["test1 PASSED final on serial-B"]));
            assert_eq!(pool.aggregate_result().await, Ok(true));
            assert!(pool.terminated());
            slot.borrow_mut().take();
        })
    });
}

#[test]
fn batch_picked_for_a_replaced_device_goes_to_its_replacement_once_it_is_ready() {
    scheduling_test(|h| {
        Box::pin(async move {
            let replaced = TestDevice::plain("serial-A");
            let replacement = TestDevice::plain("serial-A");
            let left = std::rc::Rc::new(std::cell::Cell::new(false));
            let slot: std::rc::Rc<std::cell::RefCell<Option<std::rc::Rc<marathon_core::executor::RuntimeActor<_>>>>> =
                Default::default();
            let (flag, target, a, b) = (left.clone(), slot.clone(), replaced.clone(), replacement.clone());
            let pool = h.start_pool(
                tests(1, "Scheduling"),
                estimating(std::rc::Rc::new(move |_, _| {
                    if !flag.get() {
                        flag.set(true);
                        if let Some(pool) = target.borrow().as_ref() {
                            let _ = marathon_core::runtime::Actor::try_send(pool.as_ref(), remove(&a));
                            let _ = marathon_core::runtime::Actor::try_send(pool.as_ref(), add(&b));
                        }
                    }
                })),
            );
            *slot.borrow_mut() = Some(pool.pool.actor.clone());

            pool.add(&replaced);
            h.advance_until_idle().await;

            assert!(left.get());
            assert!(replaced.batches().is_empty());
            assert_eq!(replacement.executed_tests(), s(&["test1"]));
            assert_eq!(h.tracker.events(), s(&["test1 PASSED final on serial-A"]));
            assert!(pool.terminated());
            slot.borrow_mut().take();
        })
    });
}

#[test]
fn batch_of_a_device_that_stops_before_starting_it_runs_on_another_device_without_using_an_attempt() {
    scheduling_test(|h| {
        Box::pin(async move {
            let crash = Gate::default();
            let prepared = Gate::default();
            let device_a = TestDevice::new("serial-A", None, Some(prepare_on(&prepared)), report_passed());
            let device_b = TestDevice::plain("serial-B");
            let trigger = crash.clone();
            let pool = h.start_pool(
                tests(1, "Scheduling"),
                estimating(std::rc::Rc::new(move |_, _| trigger.open())),
            );

            pool.add(&device_a);
            h.run_current().await;
            // Kotlin launches the failing child from A's prepare; here the test launches it into A's actor
            // job right after the actor exists (3.6 decision 6). It fails once the queue is picking a batch
            // for A, before the batch reaches the device.
            let actor = pool.actor_of(&device_a).expect("A's actor");
            h.fail_actor_on(&actor, Some(crash.clone()), "device actor crashed");
            pool.add(&device_b);
            prepared.open();
            h.advance_until_idle().await;

            assert!(crash.is_open());
            assert!(device_a.batches().is_empty());
            assert_eq!(device_b.executed_tests(), s(&["test1"]));
            assert_eq!(h.tracker.events(), s(&["test1 PASSED final on serial-B"]));
            assert!(pool.terminated());
        })
    });
}

#[test]
fn late_readiness_of_a_replaced_device_does_not_claim_work() {
    scheduling_test(|h| {
        Box::pin(async move {
            let ready_replaced = Gate::default();
            let ready_replacement = Gate::default();
            let replaced = TestDevice::new("serial-A", None, Some(prepare_on(&ready_replaced)), report_passed());
            let replacement = TestDevice::new("serial-A", None, Some(prepare_on(&ready_replacement)), report_passed());
            let device_b = TestDevice::new("serial-B", None, None, finish_on_signal());
            let pool = h.start_pool(tests(2, "Scheduling"), PoolOptions::default());
            pool.add(&replaced);
            pool.add(&device_b);
            h.run_current().await;
            assert_eq!(device_b.batches().len(), 1);

            // The replaced device becomes ready while it is being replaced; its notification reaches the pool afterwards
            ready_replaced.open();
            pool.remove(&replaced);
            pool.add(&replacement);
            h.run_current().await;
            assert!(replaced.batches().is_empty());
            assert!(replacement.batches().is_empty());

            // Kotlin's single-threaded scheduler resumes the replacement's prepare before device B's execute, so the
            // replacement asks first. Our vendor tasks complete on separate threads, so drain before releasing B (U56)
            ready_replacement.open();
            h.run_current().await;
            assert_eq!(replacement.executed_tests().len(), 1);
            device_b.finish.send();
            h.advance_until_idle().await;
            assert!(replaced.batches().is_empty());
            assert_eq!(replacement.executed_tests().len(), 1);
            assert_ne!(replacement.executed_tests(), device_b.executed_tests());
            assert_eq!(pool.aggregate_result().await, Ok(true));
            assert!(pool.terminated());
        })
    });
}

#[test]
fn pools_schedule_their_own_devices_and_tests_independently() {
    scheduling_test(|h| {
        Box::pin(async move {
            let named = |name: &str| PoolOptions {
                pool: name.into(),
                ..PoolOptions::default()
            };
            let first = h.start_pool(tests(8, "first"), named("first"));
            let second = h.start_pool(tests(8, "second"), named("second"));
            let first_devices: Vec<_> = (1..=4)
                .map(|i| TestDevice::new(&format!("first-{i}"), None, None, finish_on_signal()))
                .collect();
            let second_devices: Vec<_> = (1..=4)
                .map(|i| TestDevice::new(&format!("second-{i}"), None, None, finish_on_signal()))
                .collect();
            first_devices.iter().for_each(|d| first.add(d));
            second_devices.iter().for_each(|d| second.add(d));
            h.run_current().await;

            let all: Vec<_> = first_devices.iter().chain(&second_devices).collect();
            assert_eq!(sizes(&all), vec![1; 8]);
            let distinct = |devices: &[std::sync::Arc<TestDevice>]| {
                let mut tests: Vec<String> = devices.iter().flat_map(|d| d.executed_tests()).collect();
                tests.sort();
                tests.dedup();
                tests.len()
            };
            assert_eq!(distinct(&first_devices), 4);
            assert_eq!(distinct(&second_devices), 4);

            for device in &all {
                for _ in 0..8 {
                    device.finish.send();
                }
            }
            h.advance_until_idle().await;
            let executed = |devices: &[std::sync::Arc<TestDevice>]| {
                sorted(devices.iter().flat_map(|d| d.executed_tests()).collect())
            };
            assert_eq!(executed(&first_devices), sorted(methods(&tests(8, "first"))));
            assert_eq!(executed(&second_devices), sorted(methods(&tests(8, "second"))));
            assert!(first.terminated());
            assert!(second.terminated());
        })
    });
}

#[test]
fn device_added_during_the_run_starts_right_away_and_a_removed_device_returns_its_batch() {
    scheduling_test(|h| {
        Box::pin(async move {
            let device_a = TestDevice::new("serial-A", None, None, finish_on_signal());
            let device_b = TestDevice::new("serial-B", None, None, await_cancellation());
            let device_c = TestDevice::new("serial-C", None, None, finish_on_signal());
            let pool = h.start_pool(tests(4, "Scheduling"), PoolOptions::default());
            pool.add(&device_a);
            h.run_current().await;
            pool.add(&device_b);
            h.run_current().await;
            assert_eq!(sizes(&[&device_a, &device_b]), vec![1, 1]);

            pool.add(&device_c);
            h.run_current().await;
            assert_eq!(device_c.batches().len(), 1);

            let returned = device_b.executed_tests();
            assert_eq!(returned.len(), 1);
            let returned = returned[0].clone();
            pool.remove(&device_b);
            for device in [&device_a, &device_c] {
                for _ in 0..4 {
                    device.finish.send();
                }
            }
            h.advance_until_idle().await;

            let executed = [device_a.executed_tests(), device_c.executed_tests()].concat();
            assert_eq!(sorted(executed), sorted(methods(&tests(4, "Scheduling"))));
            let a = device_a.executed_tests();
            let mut expected: Vec<String> = methods(&tests(4, "Scheduling"))
                .into_iter()
                .map(|m| {
                    let on = if a.contains(&m) { "serial-A" } else { "serial-C" };
                    format!("{m} PASSED final on {on}")
                })
                .collect();
            expected.push(format!("{returned} INCOMPLETE non-final on serial-B"));
            assert_eq!(h.tracker.events(), sorted(expected));
            assert!(pool.terminated());
        })
    });
}

#[test]
fn cancelled_execution_returns_its_batch_to_a_waiting_device() {
    scheduling_test(|h| {
        Box::pin(async move {
            let ready_b = Gate::default();
            let cancel_a = Gate::default();
            let cancel = cancel_a.clone();
            let device_a = TestDevice::new(
                "serial-A",
                None,
                None,
                on_execute(move |e| {
                    let cancel = cancel.clone();
                    async move {
                        cancel.wait_in(&e.context).await?;
                        Err(VendorError::Cancelled(marathon_core::executor::shared::requested(
                            "execution cancelled by the vendor",
                        )))
                    }
                }),
            );
            let device_b = TestDevice::new("serial-B", None, Some(prepare_on(&ready_b)), report_passed());
            let pool = h.start_pool(tests(1, "Scheduling"), PoolOptions::default());
            pool.add(&device_a);
            h.run_current().await;
            pool.add(&device_b);
            h.run_current().await;
            ready_b.open();
            h.run_current().await;
            assert!(device_b.batches().is_empty());

            cancel_a.open();
            h.advance_until_idle().await;

            assert_eq!(device_b.executed_tests(), s(&["test1"]));
            assert_eq!(
                h.tracker.events(),
                s(&[
                    "test1 INCOMPLETE non-final on serial-A",
                    "test1 PASSED final on serial-B"
                ])
            );
            assert!(pool.terminated());
        })
    });
}

#[test]
fn pool_terminates_once_its_devices_leave_while_one_is_busy_and_one_waits() {
    scheduling_test(|h| {
        Box::pin(async move {
            let ready_b = Gate::default();
            let device_a = TestDevice::new("serial-A", None, None, await_cancellation());
            let device_b = TestDevice::new("serial-B", None, Some(prepare_on(&ready_b)), report_passed());
            let pool = h.start_pool(tests(1, "Scheduling"), PoolOptions::default());
            pool.add(&device_a);
            h.run_current().await;
            pool.add(&device_b);
            h.run_current().await;
            ready_b.open();
            h.run_current().await;

            pool.remove(&device_b);
            pool.remove(&device_a);
            h.advance_until_idle().await;

            assert!(device_b.batches().is_empty());
            assert_eq!(h.tracker.events(), s(&["test1 INCOMPLETE non-final on serial-A"]));
            assert_eq!(pool.aggregate_result().await, Ok(false));
            assert!(pool.terminated());
        })
    });
}
