//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/Scheduler.kt
//! and extension/Timeout.kt (deterministic cancellation/ordering coverage).
mod support;
use marathon_core::{
    error::*,
    executor::{SharedDeferred, Signal},
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
fn single_child_traversal_misses_later_attachment() {
    run(|core, driver| {
        Box::pin(async move {
            let owner = core.job(JobKind::Ordinary, None);
            let first = core.job(JobKind::Ordinary, Some(owner.clone()));
            let parent = core.job(JobKind::Supervisor, None);
            let observed = owner.clone();
            let traversal = core.spawn(
                parent.clone(),
                Box::new(move |_| {
                    Box::pin(async move {
                        observed.join_observed_children().await?;
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            let late = core.job(JobKind::Ordinary, Some(owner.clone()));
            first.complete();
            replay(driver.drain_current().await);
            assert_eq!(completed(&traversal), Completion::Success);
            assert_eq!(late.state(), JobState::Active);
            late.complete();
            owner.complete();
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn list_traversal_reads_successor_after_each_join() {
    run(|core, driver| {
        Box::pin(async move {
            let owner = core.job(JobKind::Ordinary, None);
            let first = core.job(JobKind::Ordinary, Some(owner.clone()));
            let second = core.job(JobKind::Ordinary, Some(owner.clone()));
            let parent = core.job(JobKind::Supervisor, None);
            let observed = owner.clone();
            let traversal = core.spawn(
                parent.clone(),
                Box::new(move |_| {
                    Box::pin(async move {
                        observed.join_observed_children().await?;
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            let late = core.job(JobKind::Ordinary, Some(owner.clone()));
            first.complete();
            replay(driver.drain_current().await);
            second.complete();
            replay(driver.drain_current().await);
            assert_eq!(traversal.state(), JobState::Active);
            late.complete();
            replay(driver.drain_current().await);
            assert_eq!(completed(&traversal), Completion::Success);
            owner.complete();
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn snapshot_does_not_observe_attachment_during_list_join() {
    run(|core, driver| {
        Box::pin(async move {
            let owner = core.job(JobKind::Ordinary, None);
            let first = core.job(JobKind::Ordinary, Some(owner.clone()));
            let second = core.job(JobKind::Ordinary, Some(owner.clone()));
            let parent = core.job(JobKind::Supervisor, None);
            let observed = owner.clone();
            let traversal = core.spawn(
                parent.clone(),
                Box::new(move |_| {
                    Box::pin(async move {
                        for child in observed.children_snapshot() {
                            child.join().await?;
                        }
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            let late = core.job(JobKind::Ordinary, Some(owner.clone()));
            first.complete();
            second.complete();
            replay(driver.drain_current().await);
            assert_eq!(completed(&traversal), Completion::Success);
            assert_eq!(late.state(), JobState::Active);
            late.complete();
            owner.complete();
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn timeout_owns_child_context_and_awaits_protected_cleanup() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let timeouts = core.timeouts();
            let cleaned = Rc::new(Cell::new(false));
            let flag = cleaned.clone();
            let outcome = Rc::new(RefCell::new(None));
            let out = outcome.clone();
            let task = core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        let caller = ctx.job.clone();
                        let result = timeouts
                            .with_timeout_or_null(
                                ctx,
                                10,
                                Box::new(move |scope| {
                                    Box::pin(async move {
                                        assert!(!Rc::ptr_eq(&caller, &scope.job));
                                        let result = scope.clock.delay(100, scope.job.clone()).await;
                                        let cleanup = scope.core.spawn_cleanup(Box::new(move |ctx| {
                                            Box::pin(async move {
                                                ctx.clock.delay(5, ctx.job).await?;
                                                flag.set(true);
                                                Ok(())
                                            })
                                        }));
                                        cleanup.join().await?;
                                        result?;
                                        Ok(())
                                    })
                                }),
                            )
                            .await;
                        *out.borrow_mut() = Some(result);
                        Ok(())
                    })
                }),
            );
            replay(driver.advance_to(10).await);
            assert_eq!(task.state(), JobState::Active);
            assert!(!cleaned.get());
            assert!(outcome.borrow().is_none());
            replay(driver.advance_to(15).await);
            assert!(cleaned.get());
            assert_eq!(*outcome.borrow(), Some(Ok(None)));
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn nested_core_timeout_is_not_caught_by_outer_or_null() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let outer = core.timeouts();
            let inner = outer.clone();
            let output = Rc::new(RefCell::new(None));
            let out = output.clone();
            core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        let result = outer
                            .with_timeout_or_null(
                                ctx,
                                100,
                                Box::new(move |ctx| {
                                    Box::pin(async move {
                                        inner
                                            .with_timeout(
                                                ctx,
                                                5,
                                                Box::new(|ctx| {
                                                    Box::pin(async move {
                                                        ctx.clock.delay(20, ctx.job).await?;
                                                        Ok(())
                                                    })
                                                }),
                                            )
                                            .await
                                    })
                                }),
                            )
                            .await;
                        *out.borrow_mut() = Some(result);
                        Ok(())
                    })
                }),
            );
            replay(driver.advance_to(5).await);
            assert!(matches!(
                &*output.borrow(),
                Some(Err(OperationError::Cancelled(Cancellation::Timeout { .. })))
            ));
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn external_core_cancel_preserves_reason_through_timeout() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let timeouts = core.timeouts();
            let output = Rc::new(RefCell::new(None));
            let out = output.clone();
            let task = core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        let result = timeouts
                            .with_timeout_or_null(
                                ctx,
                                100,
                                Box::new(|ctx| {
                                    Box::pin(async move {
                                        ctx.clock.delay(200, ctx.job).await?;
                                        Ok(())
                                    })
                                }),
                            )
                            .await;
                        *out.borrow_mut() = Some(result);
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            let reason = cancelled("external");
            task.cancel(reason.clone());
            replay(driver.drain_current().await);
            assert_eq!(*output.borrow(), Some(Err(reason.into())));
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn timers_win_before_equal_timestamp_external_completion() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let timeouts = core.timeouts();
            let result = Rc::new(RefCell::new(None));
            let out = result.clone();
            let cell = SharedDeferred::new();
            let waiting = cell.clone();
            core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        let value = timeouts
                            .with_timeout_or_null(
                                ctx,
                                10,
                                Box::new(move |ctx| {
                                    Box::pin(async move {
                                        waiting
                                            .wait_in(ctx.job.cancellation_signal())
                                            .await
                                            .map_err(|e| match e {
                                                VendorError::Cancelled(c) => OperationError::Cancelled(c),
                                                _ => failed("cell").into(),
                                            })?;
                                        Ok(())
                                    })
                                }),
                            )
                            .await;
                        *out.borrow_mut() = Some(value);
                        Ok(())
                    })
                }),
            );
            replay(
                driver
                    .event(10, move || {
                        cell.complete(Ok(()));
                        Ok(())
                    })
                    .await,
            );
            assert_eq!(*result.borrow(), Some(Ok(None)));
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn body_completion_before_deadline_cancels_unused_timer() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let timeouts = core.timeouts();
            let output = Rc::new(RefCell::new(None));
            let out = output.clone();
            core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        let result = timeouts
                            .with_timeout_or_null(
                                ctx,
                                10,
                                Box::new(|ctx| {
                                    Box::pin(async move {
                                        ctx.clock.delay(5, ctx.job).await?;
                                        Ok(())
                                    })
                                }),
                            )
                            .await;
                        *out.borrow_mut() = Some(result);
                        Ok(())
                    })
                }),
            );
            replay(driver.advance_to(5).await);
            assert_eq!(*output.borrow(), Some(Ok(Some(()))));
            assert_eq!(driver.pending_timers(), 0);
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn cross_executor_vendor_delay_continuation_and_bridge_are_drained() {
    run(|core, driver| {
        Box::pin(async move {
            let clock = core.vendor_clock();
            let parent = core.job(JobKind::Supervisor, None);
            let cell = SharedDeferred::new();
            let output = cell.clone();
            let vendor = core.vendor().spawn_cancellable(Box::new(move |signal| {
                Box::pin(async move {
                    clock.delay(7, signal).await?;
                    output.complete(Ok(42));
                    Ok(())
                })
            }));
            let observed = Rc::new(Cell::new(0));
            let out = observed.clone();
            core.spawn(
                parent.clone(),
                Box::new(move |_| {
                    Box::pin(async move {
                        out.set(*cell.wait().await.unwrap());
                        Ok(())
                    })
                }),
            );
            replay(driver.advance_to(6).await);
            assert_eq!(observed.get(), 0);
            replay(driver.advance_to(7).await);
            assert_eq!(observed.get(), 42);
            assert_eq!(vendor.join().await, Ok(()));
            replay(driver.assert_quiescent());
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn vendor_timeout_awaits_cleanup_on_shared_timeline() {
    run(|core, driver| {
        Box::pin(async move {
            let clock = core.vendor_clock();
            let body_clock = clock.clone();
            let cleanup_clock = clock.clone();
            let cleaned = Arc::new(AtomicBool::new(false));
            let flag = cleaned.clone();
            let output = SharedDeferred::new();
            let out = output.clone();
            core.vendor().spawn_cancellable(Box::new(move |signal| {
                Box::pin(async move {
                    let result = clock
                        .with_timeout_or_null(
                            signal,
                            5,
                            Box::new(move |scope| {
                                Box::pin(async move {
                                    let result = body_clock.delay(10, scope).await;
                                    cleanup_clock.delay(3, Signal::new()).await?;
                                    flag.store(true, Ordering::SeqCst);
                                    result
                                })
                            }),
                        )
                        .await;
                    out.complete(result);
                    Ok(())
                })
            }));
            replay(driver.advance_to(5).await);
            assert!(output.completed().is_none());
            assert!(!cleaned.load(Ordering::SeqCst));
            replay(driver.advance_to(8).await);
            assert!(cleaned.load(Ordering::SeqCst));
            assert_eq!(output.completed(), Some(Ok(Arc::new(None))));
            Ok(())
        })
    });
}
#[test]
fn nested_vendor_timeout_keeps_original_scope_and_external_cancel() {
    run(|core, driver| {
        Box::pin(async move {
            let clock = core.vendor_clock();
            let inner = clock.clone();
            let delay = clock.clone();
            let output = SharedDeferred::new();
            let out = output.clone();
            core.vendor().spawn_cancellable(Box::new(move |signal| {
                Box::pin(async move {
                    let result = clock
                        .with_timeout_or_null(
                            signal,
                            50,
                            Box::new(move |scope| {
                                Box::pin(async move {
                                    inner
                                        .with_timeout(
                                            scope,
                                            5,
                                            Box::new(move |scope| {
                                                Box::pin(async move { delay.delay(10, scope).await })
                                            }),
                                        )
                                        .await
                                })
                            }),
                        )
                        .await;
                    out.complete(result);
                    Ok(())
                })
            }));
            replay(driver.advance_to(5).await);
            assert!(matches!(
                output.completed(),
                Some(Err(VendorError::Cancelled(Cancellation::Timeout { .. })))
            ));
            let clock = core.vendor_clock();
            let delay = clock.clone();
            let output = SharedDeferred::new();
            let out = output.clone();
            let signal = Signal::new();
            let cancellation = signal.clone();
            core.vendor().spawn(Box::pin(async move {
                out.complete(
                    clock
                        .with_timeout_or_null(
                            signal,
                            100,
                            Box::new(move |scope| Box::pin(async move { delay.delay(200, scope).await })),
                        )
                        .await,
                );
                Ok(())
            }));
            replay(driver.drain_current().await);
            let reason = cancelled("vendor external");
            cancellation.cancel(reason.clone());
            replay(driver.drain_current().await);
            assert_eq!(output.completed(), Some(Err(VendorError::Cancelled(reason))));
            Ok(())
        })
    });
}
#[test]
fn vendor_timer_wins_equal_time_and_body_wins_earlier() {
    run(|core, driver| {
        Box::pin(async move {
            for (delay_ms, expected) in [(4, Some(())), (5, None)] {
                let start = core.clock().now_millis();
                let clock = core.vendor_clock();
                let delay = clock.clone();
                let output = SharedDeferred::new();
                let out = output.clone();
                core.vendor().spawn_cancellable(Box::new(move |signal| {
                    Box::pin(async move {
                        out.complete(
                            clock
                                .with_timeout_or_null(
                                    signal,
                                    5,
                                    Box::new(move |scope| Box::pin(async move { delay.delay(delay_ms, scope).await })),
                                )
                                .await,
                        );
                        Ok(())
                    })
                }));
                replay(driver.advance_to(start + 5).await);
                assert_eq!(output.completed(), Some(Ok(Arc::new(expected))));
            }
            Ok(())
        })
    });
}
#[test]
fn equal_timestamp_events_keep_delivery_order_and_future_timers_do_not_advance() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let seen = Rc::new(RefCell::new(Vec::new()));
            let timed = seen.clone();
            core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        ctx.clock.delay(10, ctx.job).await?;
                        timed.borrow_mut().push("timer");
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            assert_eq!(core.clock().now_millis(), 0);
            replay(driver.assert_quiescent());
            let one = seen.clone();
            replay(
                driver
                    .event(10, move || {
                        one.borrow_mut().push("one");
                        Ok(())
                    })
                    .await,
            );
            let two = seen.clone();
            replay(
                driver
                    .event(10, move || {
                        two.borrow_mut().push("two");
                        Ok(())
                    })
                    .await,
            );
            assert_eq!(*seen.borrow(), ["timer", "one", "two"]);
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn pending_callback_work_and_stalled_closed_buffers_fail_quiescence() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            core.spawn(parent.clone(), Box::new(|_| Box::pin(async { Ok(()) })));
            assert!(driver.assert_quiescent().is_err());
            replay(driver.drain_current().await);
            let channel = core.event_channel("core.DeviceProvider.events");
            assert!(channel.try_send(1).is_ok());
            channel.close(None);
            assert!(driver.assert_quiescent().is_err());
            assert_eq!(channel.receive().await, Ok(Some(1)));
            replay(driver.assert_quiescent());
            assert!(driver.finish().is_err());
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn vendor_first_writer_race_seals_once_without_inline_core_callbacks() {
    run(|core, driver| {
        Box::pin(async move {
            let cell = SharedDeferred::new();
            let wins = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            for value in 0..32 {
                let cell = cell.clone();
                let wins = wins.clone();
                core.vendor().spawn(Box::pin(async move {
                    if cell.complete(Ok(value)) {
                        wins.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(())
                }));
            }
            replay(driver.drain_current().await);
            assert_eq!(wins.load(Ordering::SeqCst), 1);
            let first = cell.wait().await.unwrap();
            assert_eq!(cell.wait().await.unwrap(), first);
            Ok(())
        })
    });
}
#[test]
fn core_timeout_stays_armed_while_completed_body_waits_for_children() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let timeouts = core.timeouts();
            let outcome = Rc::new(RefCell::new(None));
            let out = outcome.clone();
            let child_ref = Rc::new(RefCell::new(None));
            let child_out = child_ref.clone();
            core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        *out.borrow_mut() = Some(
                            timeouts
                                .with_timeout_or_null(
                                    ctx,
                                    5,
                                    Box::new(move |ctx| {
                                        Box::pin(async move {
                                            let child = ctx.core.spawn(
                                                ctx.job,
                                                Box::new(|ctx| {
                                                    Box::pin(async move {
                                                        ctx.clock.delay(10, ctx.job).await?;
                                                        Ok(())
                                                    })
                                                }),
                                            );
                                            *child_out.borrow_mut() = Some(child);
                                            Ok(())
                                        })
                                    }),
                                )
                                .await,
                        );
                        Ok(())
                    })
                }),
            );
            replay(driver.advance_to(5).await);
            assert_eq!(*outcome.borrow(), Some(Ok(None)));
            assert!(matches!(
                completed(child_ref.borrow().as_ref().unwrap()),
                Completion::Cancelled(Cancellation::Timeout { .. })
            ));
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn core_same_deadline_delay_loses_to_own_timeout() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let timeouts = core.timeouts();
            let outcome = Rc::new(RefCell::new(None));
            let out = outcome.clone();
            core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        *out.borrow_mut() = Some(
                            timeouts
                                .with_timeout_or_null(
                                    ctx,
                                    5,
                                    Box::new(|ctx| {
                                        Box::pin(async move {
                                            ctx.clock.delay(5, ctx.job).await?;
                                            Ok(())
                                        })
                                    }),
                                )
                                .await,
                        );
                        Ok(())
                    })
                }),
            );
            replay(driver.advance_to(5).await);
            assert_eq!(*outcome.borrow(), Some(Ok(None)));
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn delay_observes_supplied_job_and_cancellation_reason() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let supplied = core.job(JobKind::Ordinary, None);
            let delayed = supplied.clone();
            let reason = cancelled("supplied");
            let expected = reason.clone();
            let outcome = Rc::new(RefCell::new(None));
            let out = outcome.clone();
            let task = core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        *out.borrow_mut() = Some(ctx.clock.delay(10, delayed).await);
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            supplied.cancel(reason);
            replay(driver.drain_current().await);
            assert_eq!(*outcome.borrow(), Some(Err(expected)));
            assert_eq!(completed(&task), Completion::Success);
            parent.complete();
            Ok(())
        })
    });
}
#[test]
fn timeout_panics_surface_as_failures_without_leaving_children_live() {
    run(|core, driver| {
        Box::pin(async move {
            let parent = core.job(JobKind::Supervisor, None);
            let timeouts = core.timeouts();
            let outcome = Rc::new(RefCell::new(None));
            let out = outcome.clone();
            core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        *out.borrow_mut() = Some(
                            timeouts
                                .with_timeout(ctx, 5, Box::new(|_| Box::pin(async { panic!("timeout body") })))
                                .await,
                        );
                        Ok(())
                    })
                }),
            );
            replay(driver.drain_current().await);
            assert!(matches!(&*outcome.borrow(), Some(Err(OperationError::Failed(_)))));
            assert!(parent.children_snapshot().is_empty());
            parent.complete();
            Ok(())
        })
    });
}

#[test]
fn same_deadline_vendor_continuations_keep_registration_order() {
    run(|core, driver| {
        Box::pin(async move {
            for _ in 0..2_000 {
                let start = core.clock().now_millis();
                let channel = marathon_core::executor::SharedChannel::new();
                for n in 0..2 {
                    let clock = core.vendor_clock();
                    let out = channel.clone();
                    core.vendor().spawn_cancellable(Box::new(move |signal| {
                        Box::pin(async move {
                            clock.delay(1, signal).await?;
                            assert!(out.try_send(n).is_ok());
                            Ok(())
                        })
                    }));
                    replay(driver.drain_current().await);
                }
                replay(driver.advance_to(start + 1).await);
                assert_eq!(channel.receive().await, Ok(Some(0)));
                assert_eq!(channel.receive().await, Ok(Some(1)));
                replay(driver.assert_quiescent());
            }
            Ok(())
        })
    });
}
