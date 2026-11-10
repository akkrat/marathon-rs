//! Port of core/src/main/kotlin/com/malinskiy/marathon/actor/Actor.kt,
//! execution/Retry.kt and execution/Scheduler.kt (new behavioural coverage).
mod support;
use marathon_core::{
    error::*,
    executor::{CoreExecutor, RuntimeActor, SharedDeferred, retry::with_retry},
    runtime::*,
    time::*,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use support::*;

#[test]
fn standalone_job_needs_complete_and_waits_for_children() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Ordinary, None);
            let child = core.job(JobKind::Ordinary, Some(parent.clone()));
            assert!(parent.complete());
            assert!(!parent.complete());
            assert_eq!(parent.state(), JobState::Completing);
            child.complete();
            parent.join().await?;
            assert_eq!(completed(&parent), Completion::Success);
            replay(driver.drain_current().await);
            Ok(())
        })
    });
}
#[test]
fn ordinary_child_failure_cancels_parent_and_sibling() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Ordinary, None);
            let sibling = core.spawn(
                parent.clone(),
                Box::new(|ctx| {
                    Box::pin(async move {
                        ctx.clock.delay(100, ctx.job).await?;
                        Ok(())
                    })
                }),
            );
            let bad = failed("child");
            let error = bad.clone();
            let child = core.spawn(
                parent.clone(),
                Box::new(move |_| Box::pin(async move { Err(error.into()) })),
            );
            replay(driver.drain_current().await);
            parent.join().await?;
            assert_eq!(completed(&parent), Completion::Failed(bad));
            assert!(matches!(completed(&sibling), Completion::Cancelled(_)));
            assert!(matches!(completed(&child), Completion::Failed(_)));
            Ok(())
        })
    });
}
#[test]
fn supervisor_isolates_failure_but_cascades_cancellation() {
    run(|core, driver| {
        Box::pin(async move {
            let supervisor = core.job(JobKind::Supervisor, None);
            let error = failed("isolated");
            core.spawn(
                supervisor.clone(),
                Box::new(move |_| Box::pin(async move { Err(error.into()) })),
            );
            let child = core.job(JobKind::Ordinary, Some(supervisor.clone()));
            replay(driver.drain_current().await);
            assert_eq!(supervisor.state(), JobState::Active);
            assert_eq!(child.state(), JobState::Active);
            let reason = cancelled("stop");
            supervisor.cancel(reason.clone());
            assert_eq!(completed(&child), Completion::Cancelled(reason.clone()));
            assert_eq!(completed(&supervisor), Completion::Cancelled(reason));
            Ok(())
        })
    });
}
#[test]
fn child_cancellation_is_not_a_failure_and_detached_jobs_are_independent() {
    run(|core, _| {
        Box::pin(async move {
            let parent = core.job(JobKind::Ordinary, None);
            let child = core.job(JobKind::Ordinary, Some(parent.clone()));
            let detached = core.job(JobKind::Ordinary, None);
            child.cancel(cancelled("child"));
            assert_eq!(parent.state(), JobState::Active);
            parent.cancel(cancelled("parent"));
            assert_eq!(detached.state(), JobState::Active);
            detached.complete();
            Ok(())
        })
    });
}
#[test]
fn join_ignores_joined_failure_and_join_in_observes_caller() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let error = failed("failure");
            let child = core.spawn(
                parent.clone(),
                Box::new(move |_| Box::pin(async move { Err(error.into()) })),
            );
            child.join().await?;
            let caller = core.job(JobKind::Ordinary, None);
            let waiting = core.job(JobKind::Ordinary, None);
            let reason = cancelled("caller");
            caller.cancel(reason.clone());
            assert_eq!(waiting.join_in(caller).await, Err(reason));
            waiting.cancel(cancelled("done"));
            waiting.join().await?;
            replay(driver.drain_current().await);
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn cancellation_waits_for_body_cleanup_and_children() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let cleaned = Rc::new(Cell::new(false));
            let flag = cleaned.clone();
            let child = core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        let reason = ctx.clock.delay(100, ctx.job.clone()).await;
                        let cleanup = ctx.core.spawn_cleanup(Box::new(move |cleanup| {
                            Box::pin(async move {
                                cleanup.clock.delay(5, cleanup.job).await?;
                                flag.set(true);
                                Ok(())
                            })
                        }));
                        cleanup.join().await?;
                        reason?;
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            let reason = cancelled("stop");
            child.cancel(reason.clone());
            replay(driver.drain_current().await);
            assert_eq!(child.state(), JobState::Cancelling(reason.clone()));
            assert!(!cleaned.get());
            replay(driver.advance_to(5).await);
            assert!(cleaned.get());
            assert_eq!(completed(&child), Completion::Cancelled(reason));
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn completion_callbacks_are_once_late_and_disposable() {
    run(|core, _| {
        Box::pin(async move {
            let job = core.job(JobKind::Ordinary, None);
            let seen = Rc::new(RefCell::new(Vec::new()));
            let first = seen.clone();
            job.invoke_on_completion(Box::new(move |_| first.borrow_mut().push(1)));
            let removed = seen.clone();
            let registration = job.invoke_on_completion(Box::new(move |_| removed.borrow_mut().push(2)));
            registration.dispose();
            job.complete();
            job.cancel(cancelled("late"));
            job.complete();
            let late = seen.clone();
            job.invoke_on_completion(Box::new(move |_| late.borrow_mut().push(3)));
            assert_eq!(*seen.borrow(), [1, 3]);
            Ok(())
        })
    });
}
#[test]
fn spawned_body_gets_own_context_and_panics_become_failures() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let observed = Rc::new(RefCell::new(None));
            let out = observed.clone();
            let child = core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        *out.borrow_mut() = Some(ctx.job);
                        assert_eq!(std::thread::current().name(), Some("marathon-core"));
                        panic!("body panic");
                    })
                }),
            );
            replay(driver.drain_current().await);
            assert!(Rc::ptr_eq(observed.borrow().as_ref().unwrap(), &child));
            assert!(matches!(completed(&child), Completion::Failed(_)));
            assert_eq!(parent.state(), JobState::Active);
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn deferred_first_seal_repeat_wait_and_cancelled_waiter_isolation() {
    run(|core, driver| {
        Box::pin(async move {
            let cell = SharedDeferred::new();
            assert!(cell.complete(Ok(7)));
            assert!(!cell.complete(Err(VendorError::Other(failed("later")))));
            assert_eq!(*cell.wait().await.unwrap(), 7);
            assert_eq!(*cell.wait().await.unwrap(), 7);
            let cancelled_cell = SharedDeferred::<i32>::new();
            assert!(cancelled_cell.cancel(cancelled("seal")));
            assert!(!cancelled_cell.complete(Ok(8)));
            let cell = SharedDeferred::new();
            let waiting = cell.clone();
            let parent = core.job(JobKind::Supervisor, None);
            let waiter = core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        let _ = waiting.wait_in(ctx.job.cancellation_signal()).await;
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            waiter.cancel(cancelled("waiter"));
            replay(driver.drain_current().await);
            assert!(cell.complete(Ok(9)));
            assert_eq!(*cell.wait().await.unwrap(), 9);
            parent.complete();
            Ok(())
        })
    });
}
struct Handler {
    seen: Rc<RefCell<Vec<i32>>>,
    suspend: bool,
    fail: bool,
    started: Rc<Cell<usize>>,
    dropped: Rc<Cell<bool>>,
}
impl Drop for Handler {
    fn drop(&mut self) {
        self.dropped.set(true);
    }
}
impl ActorHandler<i32> for Handler {
    fn started(&mut self, context: ActorContext<i32>) {
        self.started.set(self.started.get() + 1);
        assert!(matches!(
            context.task.job.state(),
            JobState::Active | JobState::Cancelling(_)
        ));
    }
    fn receive(&mut self, message: i32, context: ActorContext<i32>) -> LocalFuture<'_, OperationResult<()>> {
        Box::pin(async move {
            self.seen.borrow_mut().push(message);
            if self.fail {
                return Err(failed("handler").into());
            }
            if self.suspend {
                context.task.clock.delay(100, context.task.job).await?;
            }
            Ok(())
        })
    }
}
fn handler(suspend: bool, fail: bool) -> Handler {
    Handler {
        seen: Rc::new(RefCell::new(Vec::new())),
        suspend,
        fail,
        started: Rc::new(Cell::new(0)),
        dropped: Rc::new(Cell::new(false)),
    }
}
#[test]
fn actor_fifo_started_and_send_after_close() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let h = handler(false, false);
            let seen = h.seen.clone();
            let started = h.started.clone();
            let dropped = h.dropped.clone();
            let actor = RuntimeActor::new(core.clone(), parent.clone(), core.clock(), h, "core.Actor.mailbox");
            assert_eq!(started.get(), 1);
            for n in 0..5 {
                assert!(actor.try_send(n).is_ok());
            }
            replay(driver.drain_current().await);
            assert_eq!(*seen.borrow(), [0, 1, 2, 3, 4]);
            actor.close();
            actor.close();
            assert_eq!(actor.try_send(9).err().unwrap().message, 9);
            assert!(actor.send(10).await.is_err());
            replay(driver.drain_current().await);
            actor.job().join().await?;
            assert!(dropped.get());
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn actor_close_interrupts_handler_and_discards_queue() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let h = handler(true, false);
            let seen = h.seen.clone();
            let actor = RuntimeActor::new(core.clone(), parent.clone(), core.clock(), h, "core.Actor.mailbox");
            assert!(actor.try_send(1).is_ok());
            assert!(actor.try_send(2).is_ok());
            replay(driver.drain_current().await);
            actor.close();
            replay(driver.drain_current().await);
            assert_eq!(*seen.borrow(), [1]);
            assert!(matches!(completed(&actor.job()), Completion::Cancelled(_)));
            replay(driver.assert_quiescent());
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn failed_receive_ends_actor_and_releases_handler_after_callbacks() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let h = handler(false, true);
            let seen = h.seen.clone();
            let dropped = h.dropped.clone();
            let callback_ran = Rc::new(Cell::new(false));
            let actor = RuntimeActor::new(core.clone(), parent.clone(), core.clock(), h, "core.Actor.mailbox");
            let in_callback = dropped.clone();
            let ran = callback_ran.clone();
            actor.job().invoke_on_completion(Box::new(move |_| {
                assert!(!in_callback.get());
                ran.set(true);
            }));
            assert!(actor.try_send(1).is_ok());
            assert!(actor.try_send(2).is_ok());
            replay(driver.drain_current().await);
            assert_eq!(*seen.borrow(), [1]);
            assert!(callback_ran.get());
            assert!(dropped.get());
            assert!(matches!(completed(&actor.job()), Completion::Failed(_)));
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn provider_channel_close_drains_cancel_discards_and_cancel_overrides_close() {
    run(|core, driver| {
        Box::pin(async move {
            let channel = core.event_channel("core.DeviceProvider.events");
            assert!(channel.try_send(1).is_ok());
            channel.close(None);
            assert_eq!(channel.receive().await, Ok(Some(1)));
            assert_eq!(channel.receive().await, Ok(None));
            let discarded = core.event_channel("core.DeviceProvider.events");
            assert!(discarded.try_send(2).is_ok());
            discarded.close(None);
            let reason = cancelled("discard");
            discarded.cancel(reason.clone());
            assert_eq!(discarded.receive().await, Err(VendorError::Cancelled(reason)));
            assert_eq!(
                EventChannel::state(discarded.as_ref()),
                MailboxState {
                    queued: 0,
                    closed_for_send: true,
                    discarded: true
                }
            );
            replay(driver.assert_quiescent());
            Ok(())
        })
    });
}
#[test]
fn retry_counts_attempts_and_rethrows_last_failure() {
    run(|core, _| {
        Box::pin(async move {
            let parent = core.job(JobKind::Ordinary, None);
            let calls = Cell::new(0);
            let result: OperationResult<()> = with_retry(3, 0, core.clock().as_ref(), parent.clone(), || {
                calls.set(calls.get() + 1);
                std::future::ready(Err(failed(&format!("{}", calls.get())).into()))
            })
            .await;
            assert_eq!(calls.get(), 3);
            assert_eq!(result, Err(failed("3").into()));
            let result = with_retry(1, 0, core.clock().as_ref(), parent.clone(), || {
                std::future::ready(Ok(42))
            })
            .await;
            assert_eq!(result, Ok(42));
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn retry_cancellation_short_circuits_and_delay_observes_job() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let calls = Rc::new(Cell::new(0));
            let count = calls.clone();
            let reason = cancelled("retry");
            let expected = reason.clone();
            let child = core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        with_retry(3, 10, ctx.clock.as_ref(), ctx.job.clone(), || {
                            count.set(count.get() + 1);
                            std::future::ready(Err::<(), _>(failed("retry").into()))
                        })
                        .await
                    })
                }),
            );
            replay(driver.drain_current().await);
            assert_eq!(calls.get(), 1);
            child.cancel(reason);
            replay(driver.drain_current().await);
            assert_eq!(completed(&child), Completion::Cancelled(expected));
            assert_eq!(calls.get(), 1);
            let direct = cancelled("direct");
            let result: OperationResult<()> = with_retry(3, 0, core.clock().as_ref(), parent.clone(), || {
                std::future::ready(Err(direct.clone().into()))
            })
            .await;
            assert_eq!(result, Err(direct.into()));
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn completion_logging_has_exact_three_branches() {
    use marathon_core::executor::completion_logging::completion_log;
    let logs = [
        completion_log(Completion::Success),
        completion_log(Completion::Cancelled(cancelled("log"))),
        completion_log(Completion::Failed(failed("log"))),
    ];
    assert_eq!(
        logs.iter().map(|log| log.message).collect::<Vec<_>>(),
        [
            "CoroutineContext finished",
            "CoroutineContext cancelled",
            "CoroutineContext closing due to unrecoverable error",
        ]
    );
    assert_eq!(logs[0].level, marathon_logging::Level::DEBUG);
    assert_eq!(logs[2].level, marathon_logging::Level::ERROR);
    assert!(logs[1].exception.is_some());
}
#[test]
fn vendor_work_and_blocking_work_run_off_core_and_remain_joinable() {
    CoreExecutor::run(|core| {
        Box::pin(async move {
            let thread = SharedDeferred::new();
            let out = thread.clone();
            let work = core.vendor().spawn(Box::pin(async move {
                out.complete(Ok(std::thread::current().name().unwrap_or("").to_owned()));
                Ok(())
            }));
            assert_ne!(thread.wait().await.unwrap().as_str(), "marathon-core");
            work.join().await.unwrap();
            let ran = Arc::new(AtomicBool::new(false));
            let out = ran.clone();
            let blocking = core.vendor().spawn_blocking(Box::new(move || {
                out.store(true, Ordering::SeqCst);
                Ok(())
            }));
            blocking.cancel(cancelled("awaiter"));
            blocking.join().await.unwrap();
            assert!(ran.load(Ordering::SeqCst));
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn vendor_task_cancel_resumes_explicit_signal_and_awaits_cleanup() {
    run(|core, driver| {
        Box::pin(async move {
            let cleaned = Arc::new(AtomicBool::new(false));
            let flag = cleaned.clone();
            let clock = core.vendor_clock();
            let task = core.vendor().spawn_cancellable(Box::new(move |signal| {
                Box::pin(async move {
                    let result = clock.delay(100, signal).await;
                    flag.store(true, Ordering::SeqCst);
                    result
                })
            }));
            replay(driver.drain_current().await);
            let reason = cancelled("vendor");
            task.cancel(reason.clone());
            replay(driver.drain_current().await);
            assert_eq!(task.join().await, Err(VendorError::Cancelled(reason)));
            assert!(cleaned.load(Ordering::SeqCst));
            Ok(())
        })
    });
}
#[test]
fn already_cancelled_parent_still_runs_actor_started_and_discards_first_message() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Ordinary, None);
            parent.cancel(cancelled("before actor"));
            let h = handler(false, false);
            let started = h.started.clone();
            let dropped = h.dropped.clone();
            let actor = RuntimeActor::new(core.clone(), parent, core.clock(), h, "core.Actor.mailbox");
            assert_eq!(started.get(), 1);
            assert!(actor.try_send(1).is_err());
            replay(driver.drain_current().await);
            assert!(dropped.get());
            Ok(())
        })
    });
}
#[test]
fn provider_receive_cancellation_preserves_cell_and_other_waiters() {
    run(|core, driver| {
        Box::pin(async move {
            let channel = core.event_channel("core.DeviceProvider.events");
            let receiver = channel.clone();
            let parent = core.job(JobKind::Supervisor, None);
            let waiter = core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        let result = receiver.receive_in(ctx.job.cancellation_signal()).await;
                        assert!(matches!(result, Err(VendorError::Cancelled(_))));
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            waiter.cancel(cancelled("await only"));
            replay(driver.drain_current().await);
            assert!(channel.try_send(1).is_ok());
            assert_eq!(channel.receive().await, Ok(Some(1)));
            channel.close(None);
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn actor_panic_retains_handler_until_completion_callbacks() {
    run(|core, driver| {
        Box::pin(async move {
            struct Panicking {
                dropped: Rc<Cell<bool>>,
            }
            impl Drop for Panicking {
                fn drop(&mut self) {
                    self.dropped.set(true);
                }
            }
            impl ActorHandler<()> for Panicking {
                fn receive(&mut self, _: (), _: ActorContext<()>) -> LocalFuture<'_, OperationResult<()>> {
                    Box::pin(async { panic!("actor handler") })
                }
            }
            let parent = core.job(JobKind::Supervisor, None);
            let dropped = Rc::new(Cell::new(false));
            let actor = RuntimeActor::new(
                core.clone(),
                parent.clone(),
                core.clock(),
                Panicking {
                    dropped: dropped.clone(),
                },
                "core.Actor.mailbox",
            );
            let callback = dropped.clone();
            let saw = Rc::new(Cell::new(false));
            let out = saw.clone();
            actor.job().invoke_on_completion(Box::new(move |_| {
                assert!(!callback.get());
                out.set(true);
            }));
            assert!(actor.try_send(()).is_ok());
            replay(driver.drain_current().await);
            assert!(saw.get());
            assert!(dropped.get());
            assert!(matches!(completed(&actor.job()), Completion::Failed(_)));
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn completion_events_keep_their_level_message_and_cause() {
    let log = marathon_core::executor::completion_logging::completion_log(Completion::Success);
    assert_eq!(log.level, marathon_logging::Level::DEBUG);
    assert_eq!(log.message, "CoroutineContext finished");
    assert!(log.exception.is_none());
}
