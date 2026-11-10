//! Port of core/src/main/kotlin/com/malinskiy/marathon/actor/Actor.kt
//! and execution/Scheduler.kt, compared with tools/oracle/runtime-probes.
mod support;
use marathon_core::{
    error::*,
    executor::{CoreExecutor, SharedDeferred, Signal},
    runtime::*,
    time::*,
};
use std::{
    cell::Cell,
    collections::BTreeMap,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use support::*;
fn corpus() -> BTreeMap<String, String> {
    include_str!("../../../fixtures/oracle/runtime/coroutines-1.8.1.jsonl")
        .lines()
        .map(|line| {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            (
                row["case"].as_str().unwrap().to_owned(),
                row["value"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}
#[test]
fn pinned_coroutines_corpus_matches_runtime() {
    let actual = CoreExecutor::run_virtual(|core, driver| {
        Box::pin(async move {
            let mut rows = BTreeMap::new();
            let parent = core.job(JobKind::Supervisor, None);
            for ms in [0, -1, i64::MIN, 1, i64::MAX / 2, i64::MAX] {
                let task = core.spawn(
                    parent.clone(),
                    Box::new(move |ctx| {
                        Box::pin(async move {
                            ctx.clock.delay(ms, ctx.job).await?;
                            Ok(())
                        })
                    }),
                );
                replay(driver.drain_current().await);
                rows.insert(
                    format!("delay:{ms}"),
                    if matches!(task.state(), JobState::Completed(_)) {
                        "immediate"
                    } else {
                        "pending"
                    }
                    .to_owned(),
                );
                task.cancel(cancelled("probe"));
                replay(driver.drain_current().await);
                let timeouts = core.timeouts();
                let entered = Rc::new(Cell::new(false));
                let flag = entered.clone();
                let task = core.spawn(
                    parent.clone(),
                    Box::new(move |ctx| {
                        Box::pin(async move {
                            timeouts
                                .with_timeout_or_null(
                                    ctx,
                                    ms,
                                    Box::new(move |ctx| {
                                        Box::pin(async move {
                                            flag.set(true);
                                            ctx.clock.delay(i64::MAX, ctx.job).await?;
                                            Ok(())
                                        })
                                    }),
                                )
                                .await?;
                            Ok(())
                        })
                    }),
                );
                replay(driver.drain_current().await);
                rows.insert(
                    format!("timeout:{ms}"),
                    format!(
                        "{}:{}",
                        if matches!(task.state(), JobState::Completed(_)) {
                            "immediate"
                        } else {
                            "pending"
                        },
                        entered.get()
                    ),
                );
                task.cancel(cancelled("probe"));
                replay(driver.drain_current().await);
            }
            let out = Rc::new(std::cell::RefCell::new(None));
            let result = out.clone();
            let outer = core.timeouts();
            let inner = outer.clone();
            core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        *result.borrow_mut() = Some(
                            outer
                                .with_timeout_or_null(
                                    ctx,
                                    100,
                                    Box::new(move |ctx| {
                                        Box::pin(async move {
                                            inner
                                                .with_timeout(
                                                    ctx,
                                                    1,
                                                    Box::new(|ctx| {
                                                        Box::pin(async move {
                                                            ctx.clock.delay(10, ctx.job).await?;
                                                            Ok(())
                                                        })
                                                    }),
                                                )
                                                .await
                                        })
                                    }),
                                )
                                .await,
                        );
                        Ok(())
                    })
                }),
            );
            replay(driver.advance_to(1).await);
            rows.insert(
                "nested-timeout".into(),
                if matches!(
                    &*out.borrow(),
                    Some(Err(OperationError::Cancelled(Cancellation::Timeout { .. })))
                ) {
                    "inner"
                } else {
                    "null"
                }
                .into(),
            );
            let owner = core.job(JobKind::Ordinary, None);
            let a = core.job(JobKind::Ordinary, Some(owner.clone()));
            rows.insert(
                "single:first".into(),
                Rc::ptr_eq(&owner.children_snapshot()[0], &a).to_string(),
            );
            a.complete();
            owner.complete();
            let cell = SharedDeferred::new();
            rows.insert("deferred:value-wins".into(), cell.complete(Ok(7)).to_string());
            rows.insert(
                "deferred:error-loses".into(),
                cell.complete(Err(VendorError::Other(failed("later")))).to_string(),
            );
            rows.insert(
                "deferred:repeat".into(),
                format!("{},{}", cell.wait().await.unwrap(), cell.wait().await.unwrap()),
            );
            let cell = SharedDeferred::new();
            cell.cancel(cancelled("cell"));
            rows.insert("deferred:cancel-wins".into(), (!cell.complete(Ok(8))).to_string());
            let cell = SharedDeferred::new();
            let waiting = cell.clone();
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
            waiter.cancel(cancelled("wait"));
            replay(driver.drain_current().await);
            rows.insert(
                "deferred:await-isolation".into(),
                (cell.complete(Ok(9)) && *cell.wait().await.unwrap() == 9).to_string(),
            );
            let ch = core.event_channel("core.DeviceProvider.events");
            assert!(ch.try_send(1).is_ok());
            ch.close(None);
            rows.insert(
                "channel:close-drains".into(),
                ch.receive().await.unwrap().unwrap().to_string(),
            );
            rows.insert("channel:send-closed".into(), ch.try_send(2).is_err().to_string());
            let ch = core.event_channel("core.DeviceProvider.events");
            assert!(ch.try_send(1).is_ok());
            ch.cancel(cancelled("channel"));
            rows.insert(
                "channel:cancel-discards".into(),
                (EventChannel::state(ch.as_ref()).queued == 0 && ch.receive().await.is_err()).to_string(),
            );
            let ordinary = core.job(JobKind::Ordinary, None);
            let child = core.job(JobKind::Ordinary, Some(ordinary.clone()));
            child.cancel(cancelled("child"));
            rows.insert(
                "job:child-cancel-isolated".into(),
                matches!(ordinary.state(), JobState::Active).to_string(),
            );
            ordinary.complete();
            let ordinary = core.job(JobKind::Ordinary, None);
            core.job(JobKind::Ordinary, Some(ordinary.clone())).complete();
            rows.insert(
                "job:explicit-completion".into(),
                matches!(ordinary.state(), JobState::Active).to_string(),
            );
            ordinary.complete();
            let ordinary = core.job(JobKind::Ordinary, None);
            ordinary.cancel(cancelled("standalone"));
            rows.insert(
                "job:cancel-completes-standalone".into(),
                matches!(ordinary.state(), JobState::Completed(_)).to_string(),
            );
            replay(driver.drain_current().await);
            parent.complete();
            replay(driver.finish());
            Ok(rows)
        })
    })
    .unwrap();
    let mut expected = corpus();
    // D65 retires only these synthetic cursor comparisons, keeping every other
    // delay, timeout, deferred, snapshot and failure row in the immutable corpus.
    for key in ["single:attached-observed", "list:second", "list:attached-observed"] {
        assert!(expected.remove(key).is_some(), "historical row {key}");
    }
    assert_eq!(actual, expected);
}
#[test]
fn vendor_signed_edges_match_same_jvm_corpus() {
    let expected = corpus();
    run(move |core, driver| {
        Box::pin(async move {
            for ms in [0, -1, i64::MIN, 1, i64::MAX / 2, i64::MAX] {
                let clock = core.vendor_clock();
                let done = SharedDeferred::new();
                let out = done.clone();
                let task = core.vendor().spawn_cancellable(Box::new(move |signal| {
                    Box::pin(async move {
                        out.complete(clock.delay(ms, signal).await);
                        Ok(())
                    })
                }));
                replay(driver.drain_current().await);
                assert_eq!(
                    if done.completed().is_some() {
                        "immediate"
                    } else {
                        "pending"
                    },
                    expected[&format!("delay:{ms}")]
                );
                task.cancel(cancelled("probe"));
                replay(driver.drain_current().await);
                let entered = Arc::new(AtomicBool::new(false));
                let flag = entered.clone();
                let clock = core.vendor_clock();
                let delay = clock.clone();
                let done = SharedDeferred::new();
                let out = done.clone();
                let signal = Signal::new();
                let cancellation = signal.clone();
                core.vendor().spawn(Box::pin(async move {
                    out.complete(
                        clock
                            .with_timeout_or_null(
                                signal,
                                ms,
                                Box::new(move |scope| {
                                    Box::pin(async move {
                                        flag.store(true, Ordering::SeqCst);
                                        delay.delay(i64::MAX, scope).await
                                    })
                                }),
                            )
                            .await,
                    );
                    Ok(())
                }));
                replay(driver.drain_current().await);
                assert_eq!(
                    format!(
                        "{}:{}",
                        if done.completed().is_some() {
                            "immediate"
                        } else {
                            "pending"
                        },
                        entered.load(Ordering::SeqCst)
                    ),
                    expected[&format!("timeout:{ms}")]
                );
                cancellation.cancel(cancelled("probe"));
                replay(driver.drain_current().await);
            }
            Ok(())
        })
    });
}
#[test]
fn structured_coroutines_corpus_matches_runtime() {
    let mut actual = CoreExecutor::run_virtual(|core, driver| {
        Box::pin(async move {
            let mut rows = BTreeMap::new();
            let parent = core.job(JobKind::Supervisor, None);
            for (case, children) in [("timeout:waits-children", true), ("timeout:equal-delay", false)] {
                let result = Rc::new(std::cell::RefCell::new(None));
                let out = result.clone();
                let timeouts = core.timeouts();
                let start = core.clock().now_millis();
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
                                                if children {
                                                    ctx.core.spawn(
                                                        ctx.job,
                                                        Box::new(|ctx| {
                                                            Box::pin(async move {
                                                                ctx.clock.delay(10, ctx.job).await?;
                                                                Ok(())
                                                            })
                                                        }),
                                                    );
                                                } else {
                                                    ctx.clock.delay(5, ctx.job).await?;
                                                }
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
                replay(driver.advance_to(start + 5).await);
                rows.insert(case.into(), (*result.borrow() == Some(Ok(None))).to_string());
            }
            let cancelled_job = core.job(JobKind::Ordinary, None);
            cancelled_job.cancel(cancelled("zero"));
            rows.insert(
                "delay:cancelled-zero".into(),
                core.clock().delay(0, cancelled_job).await.is_ok().to_string(),
            );
            let job = core.job(JobKind::Ordinary, None);
            let seen = Rc::new(std::cell::RefCell::new(Vec::new()));
            for n in [1, 2] {
                let out = seen.clone();
                job.invoke_on_completion(Box::new(move |_| out.borrow_mut().push(n)));
            }
            job.complete();
            let out = seen.clone();
            job.invoke_on_completion(Box::new(move |_| out.borrow_mut().push(3)));
            rows.insert(
                "job:callback-order".into(),
                seen.borrow()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            );
            let owner = core.job(JobKind::Ordinary, Some(parent.clone()));
            core.spawn(
                owner.clone(),
                Box::new(|ctx| {
                    Box::pin(async move {
                        ctx.clock.delay(1, ctx.job).await?;
                        Err(failed("first").into())
                    })
                }),
            );
            core.spawn(
                owner.clone(),
                Box::new(|ctx| {
                    Box::pin(async move {
                        let _ = ctx.clock.delay(100, ctx.job).await;
                        Err(failed("second").into())
                    })
                }),
            );
            let start = core.clock().now_millis();
            replay(driver.advance_to(start + 1).await);
            let message = match completed(&owner) {
                Completion::Failed(f) => f.message.unwrap(),
                _ => "none".into(),
            };
            rows.insert("job:first-failure".into(), message);
            let zero = core
                .timeouts()
                .with_timeout(
                    core.context(parent.clone()),
                    0,
                    Box::new(|_| Box::pin(async { Ok(()) })),
                )
                .await;
            let message = match zero {
                Err(OperationError::Cancelled(Cancellation::Timeout { detail, .. })) => detail.message.unwrap(),
                _ => "none".into(),
            };
            rows.insert("timeout:zero-message".into(), message);
            let max_result = Rc::new(std::cell::RefCell::new(None));
            let out = max_result.clone();
            let timeouts = core.timeouts();
            core.spawn(
                parent.clone(),
                Box::new(move |ctx| {
                    Box::pin(async move {
                        *out.borrow_mut() = Some(
                            timeouts
                                .with_timeout_or_null(
                                    ctx,
                                    i64::MAX,
                                    Box::new(|ctx| {
                                        Box::pin(async move {
                                            ctx.clock.delay(i64::MAX, ctx.job).await?;
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
            let forever = core.spawn(
                parent.clone(),
                Box::new(|ctx| {
                    Box::pin(async move {
                        ctx.clock.delay(i64::MAX, ctx.job).await?;
                        Ok(())
                    })
                }),
            );
            replay(driver.advance_to(i64::MAX).await);
            rows.insert(
                "delay:max-after-advance".into(),
                matches!(forever.state(), JobState::Completed(_)).to_string(),
            );
            rows.insert(
                "timeout:max-after-advance".into(),
                (*max_result.borrow() == Some(Ok(None))).to_string(),
            );
            forever.cancel(cancelled("done"));
            replay(driver.drain_current().await);
            parent.complete();
            Ok(rows)
        })
    })
    .unwrap();
    let mut expected: BTreeMap<String, String> =
        include_str!("../../../fixtures/oracle/runtime/coroutines-1.8.1-structured.jsonl")
            .lines()
            .map(|line| {
                let row: serde_json::Value = serde_json::from_str(line).unwrap();
                (
                    row["case"].as_str().unwrap().to_owned(),
                    row["value"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
    // D64 retires disposal only; keep the immutable JVM row as historical evidence
    // and compare every other structured coroutine row without an allowance.
    assert_eq!(expected.remove("job:callback-order").as_deref(), Some("1,3"));
    assert_eq!(actual.remove("job:callback-order").as_deref(), Some("1,2,3"));
    assert_eq!(actual, expected);
}
