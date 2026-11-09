//! Port of core/src/main/kotlin/com/malinskiy/marathon/actor/Actor.kt
//! and actor/extensions.kt (review round 2 witnesses, pinned JVM probes).
mod support;
use marathon_core::{
    error::*,
    executor::{RuntimeActor, SharedChannel, SharedDeferred, Signal},
    runtime::*,
    time::ReplayDriver,
};
use std::{
    cell::{Cell, RefCell},
    future::poll_fn,
    rc::Rc,
    task::Poll,
};
use support::*;

fn expected(case: &str) -> String {
    include_str!("../../../fixtures/oracle/runtime/coroutines-1.8.1-review2.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|row| row["case"] == case)
        .unwrap()["value"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn error_result(error: VendorError) -> String {
    let failure = match error {
        VendorError::Other(failure) | VendorError::Cancelled(Cancellation::Requested(failure)) => failure,
        other => panic!("unexpected error: {other:?}"),
    };
    format!(
        "{}:{}",
        failure.kind.legacy_class().rsplit('.').next().unwrap(),
        failure.message.unwrap()
    )
}
fn suspended_deferred(state: &'static str, cancel_first: bool) {
    run(move |_, _| {
        Box::pin(async move {
            let cell = SharedDeferred::<i32>::new();
            let signal = Signal::new();
            let pending = cell.wait_in(signal.clone());
            let mut pending = std::pin::pin!(pending);
            poll_fn(|cx| {
                assert!(pending.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            if cancel_first {
                signal.cancel(cancelled("waiter"));
            }
            let stored = match state {
                "failed" => Err(VendorError::Other(failed("stored-failure"))),
                "successful" => Ok(42),
                "cancelled" => Err(VendorError::Cancelled(cancelled("stored-cancel"))),
                _ => unreachable!(),
            };
            assert!(cell.complete(stored.clone()));
            if !cancel_first {
                signal.cancel(cancelled("waiter"));
            }
            let result = match pending.await {
                Ok(value) => format!("got:{value}"),
                Err(error) => error_result(error),
            };
            assert_eq!(
                result,
                expected(&format!("deferred:{state}:cancel-first={cancel_first}"))
            );
            assert_eq!(cell.completed(), Some(stored.map(std::sync::Arc::new)));
            Ok(())
        })
    });
}
#[test]
fn suspended_deferred_failure_precedes_later_waiter_cancellation() {
    suspended_deferred("failed", false);
}
#[test]
fn suspended_deferred_waiter_cancellation_precedes_later_failure() {
    suspended_deferred("failed", true);
}
#[test]
fn suspended_deferred_success_remains_prompt_cancellable() {
    suspended_deferred("successful", false);
}
#[test]
fn suspended_deferred_waiter_cancellation_precedes_later_success() {
    suspended_deferred("successful", true);
}
#[test]
fn suspended_deferred_stored_cancellation_precedes_later_waiter_cancellation() {
    suspended_deferred("cancelled", false);
}
#[test]
fn suspended_deferred_waiter_cancellation_precedes_later_stored_cancellation() {
    suspended_deferred("cancelled", true);
}

struct SuspendedHandler {
    owner: Rc<RefCell<Option<JobRef>>>,
    child: Rc<RefCell<Option<JobRef>>>,
    caught: Rc<RefCell<Option<Cancellation>>>,
    cleanup: Rc<Cell<bool>>,
}
impl ActorHandler<()> for SuspendedHandler {
    fn started(&mut self, context: ActorContext<()>) {
        assert!(Rc::ptr_eq(&context.task.job, &context.actor.job()));
        *self.owner.borrow_mut() = Some(context.task.job);
    }
    fn receive(&mut self, _: (), context: ActorContext<()>) -> LocalFuture<'_, OperationResult<()>> {
        assert!(Rc::ptr_eq(
            &context.task.job,
            &context.actor.job().children_snapshot()[0]
        ));
        assert_eq!(
            (!Rc::ptr_eq(&context.task.job, &context.actor.job())).to_string(),
            expected("actor:handler-job-is-receiver")
        );
        Box::pin(async move {
            *self.child.borrow_mut() = Some(context.task.core.spawn(
                context.actor.job(),
                Box::new(|ctx| {
                    Box::pin(async move {
                        ctx.clock.delay(i64::MAX, ctx.job).await?;
                        Ok(())
                    })
                }),
            ));
            let result = context.task.clock.delay(i64::MAX, context.task.job).await;
            *self.caught.borrow_mut() = result.clone().err();
            let cleanup = self.cleanup.clone();
            context
                .task
                .core
                .spawn_cleanup(Box::new(move |ctx| {
                    Box::pin(async move {
                        ctx.clock.delay(1, ctx.job).await?;
                        cleanup.set(true);
                        Ok(())
                    })
                }))
                .join()
                .await?;
            result.map_err(OperationError::from)
        })
    }
}
#[test]
fn receiver_only_cancellation_interrupts_handler_delay_and_awaits_cleanup() {
    run(|core, driver| {
        Box::pin(async move {
            let root = core.job(JobKind::Supervisor, None);
            let owner_seen = Rc::new(RefCell::new(None));
            let child = Rc::new(RefCell::new(None));
            let caught = Rc::new(RefCell::new(None));
            let cleanup = Rc::new(Cell::new(false));
            let actor = RuntimeActor::new(
                core.clone(),
                root.clone(),
                core.clock(),
                SuspendedHandler {
                    owner: owner_seen.clone(),
                    child: child.clone(),
                    caught: caught.clone(),
                    cleanup: cleanup.clone(),
                },
                "review2.suspended-actor",
            );
            let owner = actor.job();
            assert!(Rc::ptr_eq(owner_seen.borrow().as_ref().unwrap(), &owner));
            let receiver = owner.children_snapshot()[0].clone();
            assert!(actor.try_send(()).is_ok());
            replay(driver.drain_current().await);
            let child = child.borrow().clone().unwrap();
            receiver.cancel(cancelled("receiver-only"));
            replay(driver.drain_current().await);
            assert_eq!(
                matches!(receiver.state(), JobState::Cancelling(_)).to_string(),
                expected("actor:receiver-cancelling-during-cleanup")
            );
            assert!(!cleanup.get());
            assert!(actor.is_closed_for_send());
            replay(driver.advance_to(1).await);
            receiver.join().await?;
            assert_eq!(
                matches!(receiver.state(), JobState::Completed(_)).to_string(),
                expected("actor:receiver-completed")
            );
            assert_eq!(completed(&receiver), Completion::Cancelled(cancelled("receiver-only")));
            assert_eq!(
                (owner.state() == JobState::Active).to_string(),
                expected("actor:owner-active")
            );
            assert_eq!(
                (child.state() == JobState::Active).to_string(),
                expected("actor:scoped-child-active")
            );
            let reason = caught.borrow().clone().unwrap();
            assert_eq!(reason, cancelled(&expected("actor:caught")));
            assert_eq!(cleanup.get().to_string(), expected("actor:cleanup"));
            actor.close();
            replay(driver.drain_current().await);
            owner.join().await?;
            assert!(matches!(completed(&child), Completion::Cancelled(_)));
            root.complete();
            Ok(())
        })
    });
}

fn channel_result(result: VendorResult<Option<i32>>) -> String {
    match result {
        Ok(Some(value)) => format!("got:{value}"),
        Ok(None) => "closed".into(),
        Err(error) => error_result(error),
    }
}
fn ready_channel(state: &'static str) {
    run(move |_, _| {
        Box::pin(async move {
            let channel = SharedChannel::new();
            match state {
                "buffered" => assert!(channel.try_send(42).is_ok()),
                "closed-buffered" => {
                    assert!(channel.try_send(42).is_ok());
                    channel.close(None);
                }
                "closed" => channel.close(None),
                "failed" => channel.close(Some(failed("closed-failure"))),
                "cancelled" => {
                    assert!(channel.try_send(42).is_ok());
                    channel.cancel(cancelled("channel"));
                }
                "pending" => {}
                _ => unreachable!(),
            }
            let signal = Signal::new();
            signal.cancel(cancelled("waiter"));
            let result = channel.receive_in(signal.clone()).await;
            assert_eq!(channel_result(result), expected(&format!("channel:ready:{state}")));
            assert_eq!(EventChannel::state(channel.as_ref()).queued, 0);
            if state == "pending" {
                assert!(channel.try_send(42).is_ok());
                assert_eq!(
                    channel_result(channel.receive().await),
                    expected("channel:pending-unaffected")
                );
            }
            if state == "closed-buffered" {
                assert_eq!(
                    channel_result(channel.receive_in(signal).await),
                    expected("channel:drained")
                );
            }
            Ok(())
        })
    });
}
#[test]
fn cancelled_channel_waiter_receives_buffered_element() {
    ready_channel("buffered");
}
#[test]
fn cancelled_channel_waiter_drains_closed_buffer() {
    ready_channel("closed-buffered");
}
#[test]
fn cancelled_channel_waiter_observes_drained_closure() {
    ready_channel("closed");
}
#[test]
fn cancelled_channel_waiter_observes_close_failure() {
    ready_channel("failed");
}
#[test]
fn cancelled_channel_waiter_observes_channel_cancellation() {
    ready_channel("cancelled");
}
#[test]
fn cancelled_channel_waiter_leaves_pending_channel_usable() {
    ready_channel("pending");
}
fn suspended_channel(state: &'static str, cancel_first: bool) {
    run(move |_, _| {
        Box::pin(async move {
            let channel = SharedChannel::new();
            let signal = Signal::new();
            let pending = channel.receive_in(signal.clone());
            let mut pending = std::pin::pin!(pending);
            poll_fn(|cx| {
                assert!(pending.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            if cancel_first {
                signal.cancel(cancelled("waiter"));
            }
            match state {
                "buffered" => assert!(channel.try_send(42).is_ok()),
                "closed" => channel.close(None),
                "failed" => channel.close(Some(failed("closed-failure"))),
                _ => unreachable!(),
            }
            if !cancel_first {
                signal.cancel(cancelled("waiter"));
            }
            assert_eq!(
                channel_result(pending.await),
                expected(&format!("channel:suspended:{state}:cancel-first={cancel_first}"))
            );
            // Cancelling the waiter never cancels the provider or discards its data.
            match state {
                "buffered" => assert_eq!(channel.receive().await, Ok(Some(42))),
                "closed" => assert_eq!(channel.receive().await, Ok(None)),
                "failed" => assert_eq!(
                    channel.receive().await,
                    Err(VendorError::Other(failed("closed-failure")))
                ),
                _ => unreachable!(),
            }
            Ok(())
        })
    });
}
#[test]
fn suspended_channel_element_remains_prompt_cancellable() {
    suspended_channel("buffered", false);
}
#[test]
fn suspended_channel_waiter_cancellation_precedes_later_element() {
    suspended_channel("buffered", true);
}
#[test]
fn suspended_channel_closure_remains_prompt_cancellable() {
    suspended_channel("closed", false);
}
#[test]
fn suspended_channel_waiter_cancellation_precedes_later_closure() {
    suspended_channel("closed", true);
}
#[test]
fn suspended_channel_close_failure_remains_prompt_cancellable() {
    suspended_channel("failed", false);
}
#[test]
fn suspended_channel_waiter_cancellation_precedes_later_close_failure() {
    suspended_channel("failed", true);
}
fn cancelled_join(state: &'static str) {
    run(move |core, driver| {
        Box::pin(async move {
            let root = core.job(JobKind::Supervisor, None);
            let job = match state {
                "completed" => {
                    let job = core.job(JobKind::Ordinary, Some(root.clone()));
                    job.complete();
                    job
                }
                "failed" => {
                    let job = core.spawn(
                        root.clone(),
                        Box::new(|_| Box::pin(async { Err(failed("joined-failure").into()) })),
                    );
                    replay(driver.drain_current().await);
                    assert!(matches!(completed(&job), Completion::Failed(_)));
                    job
                }
                "pending" => core.job(JobKind::Ordinary, Some(root.clone())),
                _ => unreachable!(),
            };
            let caller = core.job(JobKind::Ordinary, None);
            caller.cancel(cancelled("waiter"));
            let result = job.join_in(caller).await.unwrap_err();
            assert_eq!(
                error_result(VendorError::Cancelled(result)),
                expected(&format!("join:cancelled-waiter:{state}"))
            );
            if state == "pending" {
                assert_eq!(job.state(), JobState::Active);
                job.complete();
            }
            root.complete();
            Ok(())
        })
    });
}
#[test]
fn cancelled_caller_joining_completed_job_still_checks_cancellation() {
    cancelled_join("completed");
}
#[test]
fn cancelled_caller_joining_failed_job_still_checks_cancellation() {
    cancelled_join("failed");
}
#[test]
fn cancelled_caller_joining_pending_job_leaves_it_active() {
    cancelled_join("pending");
}
