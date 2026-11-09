//! Port of core/src/main/kotlin/com/malinskiy/marathon/actor/Actor.kt
//! and coroutines/ThreadPoolDispatcher.kt (review witnesses, pinned JVM probes).
mod support;
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
fn parent_completion_is_ordered_between_child_callbacks() {
    run(|core, _| {
        Box::pin(async move {
            let parent = core.job(JobKind::Ordinary, None);
            let child = core.job(JobKind::Ordinary, Some(parent.clone()));
            let seen = Rc::new(RefCell::new(Vec::new()));
            let out = seen.clone();
            child.invoke_on_completion(Box::new(move |_| out.borrow_mut().push("early-child")));
            parent.complete();
            let out = seen.clone();
            child.invoke_on_completion(Box::new(move |_| out.borrow_mut().push("late-child")));
            let out = seen.clone();
            parent.invoke_on_completion(Box::new(move |_| out.borrow_mut().push("parent")));
            child.complete();
            assert_eq!(seen.borrow().join(","), expected("job:parent-child-callbacks"));
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
