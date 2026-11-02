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
    collections::BTreeMap,
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

// C3: a cancelled caller ends its traversal with its own reason; the observed jobs are unaffected.
#[test]
fn caller_cancellation_ends_observed_children_traversal() {
    run(|core, driver| {
        Box::pin(async move {
            let owner = core.job(JobKind::Ordinary, None);
            let child = core.job(JobKind::Ordinary, Some(owner.clone()));
            let caller = core.job(JobKind::Supervisor, None);
            let observed = owner.clone();
            let traversal = core.spawn(
                caller.clone(),
                Box::new(move |context| {
                    Box::pin(async move {
                        observed.join_observed_children_in(context.job.clone()).await?;
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            assert_eq!(traversal.state(), JobState::Active);
            traversal.cancel(cancelled("caller stopped"));
            replay(driver.drain_current().await);
            assert_eq!(
                completed(&traversal),
                Completion::Cancelled(cancelled("caller stopped"))
            );
            assert_eq!(child.state(), JobState::Active);
            // Like join_in, an already-cancelled caller is checked even with nothing to join.
            let done = core.job(JobKind::Ordinary, None);
            done.complete();
            let stopped = core.job(JobKind::Ordinary, None);
            stopped.cancel(cancelled("stopped"));
            assert_eq!(done.join_observed_children_in(stopped).await, Err(cancelled("stopped")));
            child.complete();
            owner.complete();
            caller.complete();
            Ok(())
        })
    });
}

fn traversal_rows(cancellable: bool) -> BTreeMap<String, String> {
    fn observe(core: &Rc<CoreExecutor>, parent: &JobRef, owner: &JobRef, cancellable: bool) -> JobRef {
        let observed = owner.clone();
        core.spawn(
            parent.clone(),
            Box::new(move |context| {
                Box::pin(async move {
                    if cancellable {
                        observed.join_observed_children_in(context.job.clone()).await?;
                    } else {
                        observed.join_observed_children().await?;
                    }
                    Ok(())
                })
            }),
        )
    }
    CoreExecutor::run_virtual(move |core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let mut rows = BTreeMap::new();
            let owner = core.job(JobKind::Ordinary, None);
            let a = core.job(JobKind::Ordinary, Some(owner.clone()));
            let traversal = observe(&core, &parent, &owner, cancellable);
            replay(driver.drain_current().await);
            let b = core.job(JobKind::Ordinary, Some(owner.clone()));
            a.complete();
            replay(driver.drain_current().await);
            rows.insert(
                "single:attached-observed".into(),
                (!matches!(traversal.state(), JobState::Completed(_))).to_string(),
            );
            b.complete();
            owner.complete();
            let owner = core.job(JobKind::Ordinary, None);
            let c = core.job(JobKind::Ordinary, Some(owner.clone()));
            let d = core.job(JobKind::Ordinary, Some(owner.clone()));
            let traversal = observe(&core, &parent, &owner, cancellable);
            replay(driver.drain_current().await);
            let e = core.job(JobKind::Ordinary, Some(owner.clone()));
            c.complete();
            replay(driver.drain_current().await);
            rows.insert(
                "list:second".into(),
                (Rc::ptr_eq(&owner.children_snapshot()[0], &d) && matches!(traversal.state(), JobState::Active))
                    .to_string(),
            );
            d.complete();
            replay(driver.drain_current().await);
            rows.insert(
                "list:attached-observed".into(),
                matches!(traversal.state(), JobState::Active).to_string(),
            );
            e.complete();
            owner.complete();
            for callbacks in [false, true] {
                let owner = core.job(JobKind::Ordinary, None);
                let first = core.job(JobKind::Ordinary, Some(owner.clone()));
                if callbacks {
                    owner.invoke_on_completion(Box::new(|_| {}));
                } else {
                    core.job(JobKind::Ordinary, Some(owner.clone())).complete();
                }
                let traversal = observe(&core, &parent, &owner, cancellable);
                replay(driver.drain_current().await);
                let late = core.job(JobKind::Ordinary, Some(owner.clone()));
                first.complete();
                replay(driver.drain_current().await);
                rows.insert(
                    if callbacks {
                        "list:callback-promotes"
                    } else {
                        "list:promotion-survives-removal"
                    }
                    .into(),
                    matches!(traversal.state(), JobState::Active).to_string(),
                );
                late.complete();
                owner.complete();
                replay(driver.drain_current().await);
            }
            parent.complete();
            Ok(rows)
        })
    })
    .unwrap()
}

#[test]
fn cancellable_traversal_matches_jvm_traversal_rows() {
    let keys = [
        "single:attached-observed",
        "list:second",
        "list:attached-observed",
        "list:callback-promotes",
        "list:promotion-survives-removal",
    ];
    let expected: BTreeMap<String, String> = [
        include_str!("../../../fixtures/oracle/runtime/coroutines-1.8.1.jsonl"),
        include_str!("../../../fixtures/oracle/runtime/coroutines-1.8.1-traversal.jsonl"),
    ]
    .iter()
    .flat_map(|corpus| corpus.lines())
    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
    .filter(|row| keys.contains(&row["case"].as_str().unwrap()))
    .map(|row| {
        (
            row["case"].as_str().unwrap().to_owned(),
            row["value"].as_str().unwrap().to_owned(),
        )
    })
    .collect();
    assert_eq!(expected.len(), keys.len());
    assert_eq!(traversal_rows(false), expected);
    assert_eq!(traversal_rows(true), expected);
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
            std::future::poll_fn(|_| {
                std::thread::sleep(std::time::Duration::from_secs(2));
                std::task::Poll::Ready(())
            })
            .await;
            Ok(())
        })
    });
    match result {
        Err(OperationError::Failed(f)) => {
            assert_eq!(f.class.to_string_lossy(), "marathon.ReplayError");
            assert!(f.message.unwrap().to_string_lossy().starts_with("replay watchdog"));
        }
        other => panic!("expected the watchdog failure, got {other:?}"),
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}
