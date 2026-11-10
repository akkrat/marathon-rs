//! Port of core/src/main/kotlin/com/malinskiy/marathon/actor/Actor.kt
//! and coroutines/ThreadPoolDispatcher.kt (review witnesses, pinned JVM probes).
mod support;
use marathon_core::time::SchedulerClock;
use marathon_core::{
    error::*,
    executor::{RuntimeActor, SharedDeferred, Signal},
    runtime::*,
    time::ReplayDriver,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use support::*;

fn expected(case: &str) -> String {
    let value = include_str!("../../../fixtures/oracle/runtime/coroutines-1.8.1-review1.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|row| row["case"] == case)
        .unwrap()["value"]
        .as_str()
        .unwrap()
        .to_owned();
    marathon_test_support::recorded_errors::runtime_outcome(&value)
}
fn cancelled_state(job: &JobRef) -> String {
    format!(
        "{},{}",
        matches!(job.state(), JobState::Completed(_)),
        matches!(job.state(), JobState::Completed(Completion::Cancelled(_)))
    )
}
#[test]
fn cancellation_return_cancels_children() {
    run(|core, driver| {
        Box::pin(async move {
            let root = core.job(JobKind::Supervisor, None);
            let detached = core.job(JobKind::Ordinary, None);
            let child = Rc::new(RefCell::new(None));
            let out = child.clone();
            let task = core.spawn(
                root.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        let started = SharedDeferred::new();
                        let ready = started.clone();
                        *out.borrow_mut() = Some(ctx.core.spawn(
                            ctx.job,
                            Box::new(move |ctx| {
                                Box::pin(async move {
                                    started.complete(Ok(()));
                                    ctx.clock.delay(i64::MAX, ctx.job).await?;
                                    Ok(())
                                })
                            }),
                        ));
                        ready.wait().await.unwrap();
                        Err(cancelled("body cancelled").into())
                    })
                }),
            );
            replay(driver.drain_current().await);
            let child = child.borrow().clone().unwrap();
            assert_eq!(cancelled_state(&task), expected("cancelled-body:parent"));
            assert_eq!(cancelled_state(&child), expected("cancelled-body:child"));
            task.join().await?;
            child.join().await?;
            assert_eq!(
                (root.state() == JobState::Active).to_string(),
                expected("cancelled-body:supervisor-active")
            );
            assert_eq!(detached.state(), JobState::Active);
            root.complete();
            detached.complete();
            Ok(())
        })
    });
}
struct CancelHandler;
impl ActorHandler<()> for CancelHandler {
    fn receive(&mut self, _: (), context: ActorContext<()>) -> LocalFuture<'_, OperationResult<()>> {
        assert!(Rc::ptr_eq(
            &context.task.job,
            &context.actor.job().children_snapshot()[0]
        ));
        Box::pin(async { Err(cancelled("handler cancelled").into()) })
    }
}
#[test]
fn cancelled_receiver_keeps_actor_owner_active() {
    run(|core, driver| {
        Box::pin(async move {
            let root = core.job(JobKind::Supervisor, None);
            let actor = RuntimeActor::new(core.clone(), root.clone(), core.clock(), CancelHandler, "review.actor");
            let owner = actor.job();
            let receiver = owner.children_snapshot()[0].clone();
            assert!(!Rc::ptr_eq(&owner, &receiver));
            assert!(actor.try_send(()).is_ok());
            replay(driver.drain_current().await);
            assert_eq!(
                (owner.state() == JobState::Active).to_string(),
                expected("actor:owner-active")
            );
            assert_eq!(
                owner.children_snapshot().len().to_string(),
                expected("actor:owner-children")
            );
            assert_eq!(
                actor.is_closed_for_send().to_string(),
                expected("actor:receiver-closed")
            );
            assert!(matches!(completed(&receiver), Completion::Cancelled(_)));
            assert!(actor.try_send(()).is_err());
            replay(driver.assert_quiescent());
            actor.close();
            replay(driver.drain_current().await);
            owner.join().await?;
            root.complete();
            Ok(())
        })
    });
}
#[test]
fn parent_is_sealed_before_child_callbacks_and_dispatched_after_them_d65() {
    run(|core, _| {
        Box::pin(async move {
            let parent = core.job(JobKind::Ordinary, None);
            let child = core.job(JobKind::Ordinary, Some(parent.clone()));
            let seen = Rc::new(RefCell::new(Vec::new()));
            let out = seen.clone();
            let sealed = parent.clone();
            child.invoke_on_completion(Box::new(move |_| {
                assert_eq!(completed(&sealed), Completion::Success);
                out.borrow_mut().push("early-child");
            }));
            parent.complete();
            let out = seen.clone();
            let sealed = parent.clone();
            child.invoke_on_completion(Box::new(move |_| {
                assert_eq!(completed(&sealed), Completion::Success);
                out.borrow_mut().push("late-child");
            }));
            let out = seen.clone();
            parent.invoke_on_completion(Box::new(move |_| out.borrow_mut().push("parent")));
            child.complete();
            assert_eq!(expected("job:parent-child-callbacks"), "early-child,parent,late-child");
            assert_eq!(seen.borrow().join(","), "early-child,late-child,parent");
            Ok(())
        })
    });
}
fn cancelled_waiter(state: &'static str) {
    run(move |_, _| {
        Box::pin(async move {
            let cell = SharedDeferred::<i32>::new();
            match state {
                "failed" => {
                    cell.complete(Err(VendorError::Other(failed("stored-failure"))));
                }
                "successful" => {
                    cell.complete(Ok(7));
                }
                "cancelled" => {
                    cell.cancel(cancelled("stored-cancel"));
                }
                _ => {}
            }
            let signal = Signal::new();
            signal.cancel(cancelled("waiter"));
            let result = cell.wait_in(signal).await;
            if let Ok(value) = result {
                assert_eq!(*value, 7);
                assert_eq!("success", expected(&format!("deferred:cancelled-waiter-{state}")));
                return Ok(());
            }
            let error = result.unwrap_err();
            let failure = match error {
                VendorError::Other(f) | VendorError::Cancelled(Cancellation::Requested(f)) => f,
                other => panic!("unexpected await error: {other:?}"),
            };
            let class = failure.kind.description();
            let simple = class;
            let message = failure.message.unwrap();
            assert_eq!(
                format!("{simple}:{message}"),
                expected(&format!("deferred:cancelled-waiter-{state}"))
            );
            if state == "pending" {
                assert!(cell.completed().is_none());
                assert!(cell.complete(Ok(9)));
            }
            Ok(())
        })
    });
}
#[test]
fn cancelled_waiter_observes_already_failed_deferred() {
    cancelled_waiter("failed");
}
#[test]
fn cancelled_waiter_observes_already_successful_deferred() {
    cancelled_waiter("successful");
}
#[test]
fn cancelled_waiter_leaves_pending_deferred_unsealed() {
    cancelled_waiter("pending");
}
#[test]
fn cancelled_waiter_observes_deferred_stored_cancellation() {
    cancelled_waiter("cancelled");
}
#[test]
fn exception_handler_only_records_unhandled_failure() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            core.spawn(parent.clone(), Box::new(|_| Box::pin(async { Ok(()) })));
            core.spawn(
                parent.clone(),
                Box::new(|_| Box::pin(async { Err(cancelled("cancel").into()) })),
            );
            core.spawn(
                parent.clone(),
                Box::new(|_| Box::pin(async { Err(failed("failure").into()) })),
            );
            replay(driver.drain_current().await);
            let logs = core.take_completion_logs();
            assert_eq!(logs.len().to_string(), expected("exception-handler:calls"));
            assert_eq!(logs[0].message, "CoroutineContext closing due to unrecoverable error");
            assert_eq!(logs[0].exception, Some(failed("failure")));
            let queued = core.log_queue().take();
            assert_eq!(queued.len(), 1);
            assert_eq!(queued[0].level, marathon_logging::Level::ERROR);
            assert_eq!(queued[0].logger, "marathon_core::executor");
            assert_eq!(queued[0].target(), "marathon_core::executor");
            assert_eq!(queued[0].message, logs[0].message);
            assert_eq!(queued[0].cause, logs[0].exception);
            assert_eq!(queued[0].at_millis, core.clock().now_millis());
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn child_failure_reports_once_at_owning_task_boundary() {
    run(|core, driver| {
        Box::pin(async move {
            let root = core.job(JobKind::Supervisor, None);
            let entered = Rc::new(Cell::new(false));
            let flag = entered.clone();
            core.spawn(
                root.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        let owner = ctx.core.job(JobKind::Ordinary, Some(ctx.job));
                        ctx.core.spawn(
                            owner.clone(),
                            Box::new(move |_| {
                                Box::pin(async move {
                                    flag.set(true);
                                    Err(failed("nested").into())
                                })
                            }),
                        );
                        owner.complete();
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            assert!(entered.get());
            let logs = core.take_completion_logs();
            assert_eq!(logs.len().to_string(), expected("exception-handler:nested-calls"));
            assert_eq!(logs[0].exception, Some(failed("nested")));
            let timeouts = core.timeouts();
            core.spawn(
                root.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        assert!(
                            timeouts
                                .with_timeout(ctx, 5, Box::new(|_| Box::pin(async { Err(failed("caught").into()) })))
                                .await
                                .is_err()
                        );
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            assert_eq!(
                core.take_completion_logs().len().to_string(),
                expected("exception-handler:caught-calls")
            );
            root.complete();
            Ok(())
        })
    });
}

#[test]
fn sealed_ancestors_reject_callback_failing_work_d65() {
    for kind in [JobKind::Ordinary, JobKind::Supervisor] {
        for early in [false, true] {
            for depth in [1, 4] {
                run(move |core, driver| {
                    Box::pin(async move {
                        let mut ancestors = vec![core.job(kind, None)];
                        for _ in 1..depth {
                            ancestors.push(core.job(JobKind::Ordinary, Some(ancestors.last().unwrap().clone())));
                        }
                        let leaf = core.job(JobKind::Ordinary, Some(ancestors.last().unwrap().clone()));
                        let entered = Rc::new(Cell::new(0));
                        let attempted = Rc::new(RefCell::new(Vec::new()));
                        let callback_calls = Rc::new(Cell::new(0));
                        let register = || {
                            let core = core.clone();
                            let ancestors = ancestors.clone();
                            let entered = entered.clone();
                            let attempted = attempted.clone();
                            leaf.invoke_on_completion(Box::new(move |_| {
                                for parent in ancestors {
                                    assert_eq!(completed(&parent), Completion::Success);
                                    let entered = entered.clone();
                                    attempted.borrow_mut().push(core.spawn(
                                        parent,
                                        Box::new(move |_| {
                                            entered.set(entered.get() + 1);
                                            Box::pin(async { Err(failed("rejected work").into()) })
                                        }),
                                    ));
                                }
                            }));
                        };
                        if early {
                            register();
                        }
                        for parent in &ancestors {
                            let calls = callback_calls.clone();
                            parent.invoke_on_completion(Box::new(move |outcome| {
                                assert_eq!(outcome, Completion::Success);
                                calls.set(calls.get() + 1);
                            }));
                            parent.complete();
                        }
                        if !early {
                            register();
                        }
                        leaf.complete();
                        for parent in &ancestors {
                            assert_eq!(completed(parent), Completion::Success);
                            parent.join().await?;
                        }
                        assert_eq!(callback_calls.get(), depth);
                        replay(driver.drain_current().await);
                        assert_eq!(entered.get(), 0, "TaskBody was never called");
                        assert_eq!(
                            attempted.borrow().len(),
                            depth,
                            "child callback completed every seal assertion"
                        );
                        for child in attempted.borrow().iter() {
                            assert!(matches!(completed(child), Completion::Cancelled(_)));
                        }
                        for parent in ancestors {
                            assert_eq!(completed(&parent), Completion::Success);
                        }
                        assert_eq!(callback_calls.get(), depth);
                        Ok(())
                    })
                });
            }
        }
    }
}

#[test]
fn actor_receiver_seals_owner_and_supervisor_before_callbacks_d65() {
    for early in [false, true] {
        run(move |core, driver| {
            Box::pin(async move {
                let supervisor = core.job(JobKind::Supervisor, None);
                let actor = RuntimeActor::new(
                    core.clone(),
                    supervisor.clone(),
                    core.clock(),
                    CancelHandler,
                    "review.seal",
                );
                let owner = actor.job();
                let receiver = owner.children_snapshot()[0].clone();
                let entered = Rc::new(Cell::new(0));
                let callback_calls = Rc::new(Cell::new(0));
                let register = || {
                    let calls = callback_calls.clone();
                    let parents = [owner.clone(), supervisor.clone()];
                    let core = core.clone();
                    let entered = entered.clone();
                    receiver.invoke_on_completion(Box::new(move |_| {
                        for parent in parents {
                            assert_eq!(completed(&parent), Completion::Success);
                            let entered = entered.clone();
                            core.spawn(
                                parent,
                                Box::new(move |_| {
                                    entered.set(entered.get() + 1);
                                    Box::pin(async { Err(failed("rejected actor work").into()) })
                                }),
                            );
                        }
                        calls.set(calls.get() + 1);
                    }));
                };
                if early {
                    register();
                }
                owner.complete();
                supervisor.complete();
                if !early {
                    register();
                }
                assert!(actor.try_send(()).is_ok());
                replay(driver.drain_current().await);
                assert_eq!(completed(&owner), Completion::Success);
                assert_eq!(completed(&supervisor), Completion::Success);
                assert_eq!(entered.get(), 0);
                assert_eq!(
                    callback_calls.get(),
                    1,
                    "callback seal assertions must not be swallowed"
                );
                owner.join().await?;
                supervisor.join().await?;
                Ok(())
            })
        });
    }
}

#[test]
fn admitted_failure_waits_for_cleanup_and_supervisors_isolate_it_d65() {
    for kind in [JobKind::Ordinary, JobKind::Supervisor] {
        run(move |core, driver| {
            Box::pin(async move {
                let parent = core.job(kind, None);
                let cleanup = Rc::new(tokio::sync::Notify::new());
                let gate = cleanup.clone();
                let pending = core.spawn(
                    parent.clone(),
                    Box::new(move |_| {
                        Box::pin(async move {
                            gate.notified().await;
                            Ok(())
                        })
                    }),
                );
                replay(driver.drain_current().await);
                parent.complete();
                assert_eq!(parent.state(), JobState::Completing);
                let child = core.spawn(
                    parent.clone(),
                    Box::new(|_| Box::pin(async { Err(failed("admitted").into()) })),
                );
                replay(driver.drain_current().await);
                assert_eq!(completed(&child), Completion::Failed(failed("admitted")));
                assert!(!matches!(parent.state(), JobState::Completed(_)));
                if kind == JobKind::Ordinary {
                    assert!(matches!(pending.state(), JobState::Cancelling(_)));
                    parent.cancel(cancelled("later cancellation"));
                } else {
                    assert_eq!(parent.state(), JobState::Completing);
                    assert_eq!(pending.state(), JobState::Active);
                }
                cleanup.notify_one();
                replay(driver.drain_current().await);
                assert_eq!(
                    completed(&parent),
                    if kind == JobKind::Ordinary {
                        Completion::Failed(failed("admitted"))
                    } else {
                        Completion::Success
                    }
                );
                parent.join().await?;
                core.take_completion_logs();
                Ok(())
            })
        });
    }
}

#[test]
fn cancelling_attachment_never_enters_or_removes_the_pending_child_d65() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Ordinary, None);
            let cleanup = Rc::new(tokio::sync::Notify::new());
            let gate = cleanup.clone();
            let pending = core.spawn(
                parent.clone(),
                Box::new(move |_| {
                    Box::pin(async move {
                        gate.notified().await;
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            let calls = Rc::new(Cell::new(0));
            let counted = calls.clone();
            parent.invoke_on_completion(Box::new(move |_| counted.set(counted.get() + 1)));
            parent.cancel(cancelled("first reason"));
            let entered = Rc::new(Cell::new(false));
            let flag = entered.clone();
            let rejected = core.spawn(
                parent.clone(),
                Box::new(move |_| {
                    flag.set(true);
                    Box::pin(async { Err(failed("must not enter").into()) })
                }),
            );
            replay(driver.drain_current().await);
            assert!(!entered.get());
            assert_eq!(completed(&rejected), Completion::Cancelled(cancelled("first reason")));
            let children = parent.children_snapshot();
            assert_eq!(children.len(), 1);
            assert!(Rc::ptr_eq(&children[0], &pending));
            assert_eq!(parent.state(), JobState::Cancelling(cancelled("first reason")));
            parent.cancel(cancelled("second reason"));
            assert_eq!(calls.get(), 0);
            cleanup.notify_one();
            replay(driver.drain_current().await);
            assert_eq!(completed(&parent), Completion::Cancelled(cancelled("first reason")));
            assert_eq!(calls.get(), 1);
            parent.join().await?;
            Ok(())
        })
    });
}

#[test]
fn reentrant_cancellation_waits_for_other_child_cleanup_d65() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Ordinary, None);
            let first = core.job(JobKind::Ordinary, Some(parent.clone()));
            let cleanup = Rc::new(tokio::sync::Notify::new());
            let gate = cleanup.clone();
            let pending = core.spawn(
                parent.clone(),
                Box::new(move |_| {
                    Box::pin(async move {
                        gate.notified().await;
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            parent.complete();
            let target = parent.clone();
            first.invoke_on_completion(Box::new(move |_| {
                target.cancel(cancelled("first reason"));
                target.cancel(cancelled("second reason"));
                assert_eq!(target.state(), JobState::Cancelling(cancelled("first reason")));
            }));
            let calls = Rc::new(Cell::new(0));
            let counted = calls.clone();
            parent.invoke_on_completion(Box::new(move |_| counted.set(counted.get() + 1)));
            first.complete();
            assert_eq!(pending.state(), JobState::Cancelling(cancelled("first reason")));
            assert_eq!(calls.get(), 0);
            cleanup.notify_one();
            replay(driver.drain_current().await);
            assert_eq!(completed(&parent), Completion::Cancelled(cancelled("first reason")));
            assert_eq!(calls.get(), 1);
            parent.join().await?;
            Ok(())
        })
    });
}
