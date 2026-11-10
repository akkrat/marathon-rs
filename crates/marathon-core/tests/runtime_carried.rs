//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/Scheduler.kt
//! and actor/Actor.kt: regressions for the items WP 3.1 carried to WP 3.6 (C1–C5).
mod support;
use marathon_core::{
    error::*,
    executor::{CoreExecutor, SharedDeferred, Signal},
    runtime::*,
    time::*,
};
use std::{
    pin::Pin,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use support::*;

const DEPTH: usize = 100_000;

fn chain(core: &Rc<CoreExecutor>) -> Vec<JobRef> {
    let mut jobs: Vec<JobRef> = vec![core.job(JobKind::Ordinary, None)];
    for _ in 1..DEPTH {
        let parent = jobs[jobs.len() - 1].clone();
        jobs.push(core.job(JobKind::Ordinary, Some(parent)));
    }
    jobs
}

// R1-sol-2: completing a job dropped its callback list recursively, one frame per node.
#[test]
fn many_completion_callbacks_on_one_job_are_released_iteratively() {
    run(|core, _driver| {
        Box::pin(async move {
            let job = core.job(JobKind::Ordinary, None);
            let calls = Rc::new(std::cell::Cell::new(0));
            for _ in 0..DEPTH {
                let calls = calls.clone();
                job.invoke_on_completion(Box::new(move |_| calls.set(calls.get() + 1)));
            }
            assert!(job.complete());
            assert_eq!(calls.get(), DEPTH);
            Ok(())
        })
    });
}

// C4: the core thread has the default 2 MiB stack; recursion per level overflowed it.
#[test]
fn deep_chain_cancellation_cascades_iteratively() {
    run(|core, _driver| {
        Box::pin(async move {
            let jobs = chain(&core);
            jobs[0].cancel(cancelled("root"));
            assert!(matches!(completed(&jobs[0]), Completion::Cancelled(_)));
            assert!(matches!(completed(&jobs[DEPTH - 1]), Completion::Cancelled(_)));
            Ok(())
        })
    });
}

#[test]
fn deep_chain_completion_cascades_upward_iteratively() {
    run(|core, _driver| {
        Box::pin(async move {
            let jobs = chain(&core);
            for job in &jobs[..DEPTH - 1] {
                assert!(job.complete());
            }
            assert_eq!(jobs[0].state(), JobState::Completing);
            jobs[DEPTH - 1].complete();
            assert_eq!(completed(&jobs[0]), Completion::Success);
            Ok(())
        })
    });
}

#[test]
fn deep_chain_failure_propagates_iteratively() {
    run(|core, driver| {
        Box::pin(async move {
            let jobs = chain(&core);
            core.spawn(
                jobs[DEPTH - 1].clone(),
                Box::new(|_| Box::pin(async { Err(failed("leaf").into()) })),
            );
            replay(driver.drain_current().await);
            assert_eq!(completed(&jobs[0]), Completion::Failed(failed("leaf")));
            assert!(matches!(completed(&jobs[DEPTH / 2]), Completion::Failed(_)));
            core.take_completion_logs();
            Ok(())
        })
    });
}

// C3 / D65: a cancelled caller ends its join with its own reason; the target is unaffected.
#[test]
fn caller_cancellation_ends_join_without_cancelling_the_joined_job_d65() {
    run(|core, driver| {
        Box::pin(async move {
            let owner = core.job(JobKind::Ordinary, None);
            let child = core.job(JobKind::Ordinary, Some(owner.clone()));
            let caller = core.job(JobKind::Supervisor, None);
            let observed = child.clone();
            let waiter = core.spawn(
                caller.clone(),
                Box::new(move |context| {
                    Box::pin(async move {
                        observed.join_in(context.job.clone()).await?;
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            assert_eq!(waiter.state(), JobState::Active);
            waiter.cancel(cancelled("caller stopped"));
            replay(driver.drain_current().await);
            assert_eq!(completed(&waiter), Completion::Cancelled(cancelled("caller stopped")));
            assert_eq!(child.state(), JobState::Active);
            // Even an already-completed target checks the caller cancellation first.
            let done = core.job(JobKind::Ordinary, None);
            done.complete();
            let stopped = core.job(JobKind::Ordinary, None);
            stopped.cancel(cancelled("stopped"));
            assert_eq!(done.join_in(stopped).await, Err(cancelled("stopped")));
            child.complete();
            owner.complete();
            caller.complete();
            Ok(())
        })
    });
}

/// A waiter signal whose first poll lands a completion just before reporting
/// cancellation: the cross-thread write in the window after the fast-path check.
struct LandsCompletion {
    signal: Arc<Signal>,
    cell: Arc<SharedDeferred<i32>>,
    value: VendorResult<i32>,
}
impl CancellationSignal for LandsCompletion {
    fn reason(&self) -> Option<Cancellation> {
        self.signal.reason()
    }
    fn cancelled(&self) -> Pin<Box<dyn std::future::Future<Output = Cancellation> + Send + '_>> {
        self.cell.complete(self.value.clone());
        self.signal.cancelled()
    }
}

// C5: before the await first suspends, a completion that was never notified still wins.
#[test]
fn completion_in_the_first_poll_window_precedes_pending_cancellation() {
    run(|_core, _driver| {
        Box::pin(async move {
            for value in [Ok(7), Err(VendorError::Other(failed("bundle corrupted")))] {
                let signal = Signal::new();
                signal.cancel(cancelled("waiter stopped"));
                let cell = SharedDeferred::new();
                let waiter = Arc::new(LandsCompletion {
                    signal,
                    cell: cell.clone(),
                    value: value.clone(),
                });
                let result = cell.wait_in(waiter).await.map(|v| *v);
                assert_eq!(result, value);
            }
            Ok(())
        })
    });
}

#[test]
fn vendor_thread_completion_races_pending_cancellation() {
    const ITERATIONS: usize = 2_000;
    let counts = CoreExecutor::run_virtual(|core, _driver| {
        Box::pin(async move {
            let (mut stored, mut cancellation) = (0usize, 0usize);
            for _ in 0..ITERATIONS {
                let cell = SharedDeferred::<i32>::new();
                let landed = Arc::new(AtomicBool::new(false));
                let (writer, flag) = (cell.clone(), landed.clone());
                let task = core.vendor().spawn(Box::pin(async move {
                    writer.complete(Err(VendorError::Other(failed("late"))));
                    flag.store(true, Ordering::SeqCst);
                    Ok(())
                }));
                let signal = Signal::new();
                signal.cancel(cancelled("waiter stopped"));
                let before = landed.load(Ordering::SeqCst);
                match cell.wait_in(signal).await {
                    Err(VendorError::Other(f)) => {
                        assert_eq!(f, failed("late"));
                        stored += 1;
                    }
                    Err(VendorError::Cancelled(_)) => {
                        // A completion visible before the await must never lose to the waiter.
                        assert!(!before, "a landed completion lost to the pending cancellation");
                        cancellation += 1;
                    }
                    other => panic!("unexpected {other:?}"),
                }
                task.join().await.ok();
            }
            Ok((stored, cancellation))
        })
    })
    .unwrap();
    println!(
        "C5 race: {ITERATIONS} iterations, {} stored, {} cancelled",
        counts.0, counts.1
    );
    assert_eq!(counts.0 + counts.1, ITERATIONS);
}

// C1: blocking work is runnable for quiescence until it returns.
#[test]
fn drain_waits_for_blocking_work() {
    run(|core, driver| {
        Box::pin(async move {
            let finished = Arc::new(AtomicBool::new(false));
            let flag = finished.clone();
            // The sleep stands for blocking library I/O in the blocking pool; the test never sleeps.
            let task = core.vendor().spawn_blocking(Box::new(move || {
                std::thread::sleep(std::time::Duration::from_millis(50));
                flag.store(true, Ordering::SeqCst);
                Ok(())
            }));
            replay(driver.drain_current().await);
            assert!(
                finished.load(Ordering::SeqCst),
                "drain returned while blocking work ran"
            );
            assert_eq!(task.join().await, Ok(()));
            replay(driver.finish());
            Ok(())
        })
    });
}

// C2 (S5): a future that blocks inside poll stalls the executor and its driver; the
// external watchdog fails the scenario instead of hanging the test binary.
#[test]
fn watchdog_fails_a_scenario_blocked_inside_poll() {
    let started = std::time::Instant::now();
    let result = CoreExecutor::run_virtual_with_watchdog(std::time::Duration::from_millis(200), |_core, _driver| {
        Box::pin(async move {
            // Long enough that a loaded host cannot mistake waiting for the poll for the watchdog.
            std::future::poll_fn(|_| {
                std::thread::sleep(std::time::Duration::from_secs(10));
                std::task::Poll::Ready(())
            })
            .await;
            Ok(())
        })
    });
    match result {
        Err(OperationError::Failed(f)) => {
            assert_eq!(f.kind, marathon_error::FailureKind::Replay);
            assert!(f.message.unwrap().to_owned().starts_with("replay watchdog"));
        }
        other => panic!("expected the watchdog failure, got {other:?}"),
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn wide_children_keep_order_and_release_after_completion_d65() {
    run(|core, _| {
        Box::pin(async move {
            const WIDTH: usize = 10_000;
            let parent = core.job(JobKind::Ordinary, None);
            let children: Vec<_> = (0..WIDTH)
                .map(|_| core.job(JobKind::Ordinary, Some(parent.clone())))
                .collect();
            let weak: Vec<_> = children.iter().map(Rc::downgrade).collect();
            parent.invoke_on_completion(Box::new(|_| {}));
            assert_eq!(parent.children_snapshot().len(), WIDTH, "callbacks are not children");
            for child in children.iter().skip(1).step_by(2) {
                child.complete();
            }
            let snapshot = parent.children_snapshot();
            assert_eq!(snapshot.len(), WIDTH / 2);
            for (actual, expected) in snapshot.iter().zip(children.iter().step_by(2)) {
                assert!(
                    Rc::ptr_eq(actual, expected),
                    "stable removal preserves attachment order"
                );
            }
            parent.complete();
            assert_eq!(parent.state(), JobState::Completing);
            for child in children.iter().step_by(2) {
                child.complete();
            }
            assert_eq!(completed(&parent), Completion::Success);
            assert!(parent.children_snapshot().is_empty());
            drop(snapshot);
            drop(children);
            assert!(weak.iter().all(|child| child.upgrade().is_none()));
            Ok(())
        })
    });
}

#[test]
fn dropping_root_only_incomplete_deep_chain_is_iterative_u193() {
    run(|core, _| {
        Box::pin(async move {
            let root = core.job(JobKind::Ordinary, None);
            let mut tail = root.clone();
            for _ in 1..DEPTH {
                tail = core.job(JobKind::Ordinary, Some(tail));
            }
            let leaf = Rc::downgrade(&tail);
            drop(tail);
            assert_eq!(Rc::strong_count(&root), 1);
            assert_eq!(root.state(), JobState::Active);
            drop(root);
            assert!(leaf.upgrade().is_none());
            Ok(())
        })
    });
}

#[test]
fn dropping_parent_keeps_shared_child_tree_intact_u193() {
    run(|core, _| {
        Box::pin(async move {
            let parent = core.job(JobKind::Ordinary, None);
            let child = core.job(JobKind::Ordinary, Some(parent.clone()));
            let grandchild = core.job(JobKind::Ordinary, Some(child.clone()));
            let weak = Rc::downgrade(&grandchild);
            drop(parent);
            assert_eq!(child.state(), JobState::Active);
            assert!(Rc::ptr_eq(&child.children_snapshot()[0], &grandchild));
            drop(grandchild);
            drop(child);
            assert!(weak.upgrade().is_none());
            Ok(())
        })
    });
}
