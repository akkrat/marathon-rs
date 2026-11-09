//! Port of vendor/vendor-apple/macos/src/test/kotlin/com/malinskiy/marathon/apple/macos/MacosDeviceBatchTest.kt
mod support;
use marathon_apple::{apple_device::AppleDevice, cmd::cancelled};
use marathon_core::{error::VendorError, executor::shared::Signal, vendor::Device};
use std::{sync::Arc, time::Duration};
use support::*;
use tokio::time::timeout;
async fn prepared(h: &Harness) -> R<Arc<MacosDevice>> {
    let device = h.device();
    v(device.setup(&h.context).await)?;
    v(device
        .prepare(h.options.dependencies.configuration.clone(), h.context.clone())
        .await)?;
    Ok(device)
}
use marathon_macos::MacosDevice;
async fn start(
    h: &Harness,
    device: Arc<MacosDevice>,
    running: &Running,
) -> R<(
    Arc<Signal>,
    tokio::task::JoinHandle<marathon_core::error::VendorResult<()>>,
)> {
    let signal = Signal::new();
    let mut context = h.context.clone();
    context.cancellation = signal.clone();
    let task = tokio::spawn(batch(device, context));
    timeout(Duration::from_secs(60), running.opened.notified()).await?;
    Ok((signal, task))
}
async fn still_waiting<T>(future: impl std::future::Future<Output = T>) {
    assert!(timeout(Duration::from_millis(40), future).await.is_err());
}
#[tokio::test]
async fn a_batch_cancelled_while_its_xcodebuild_session_is_open_ends_only_after_the_session_has_closed() -> R {
    let h = Harness::new("")?;
    let d = prepared(&h).await?;
    let running = Running::install(&h, vec![]);
    let (signal, mut task) = start(&h, d.clone(), &running).await?;
    signal.cancel(cancelled("caller"));
    still_waiting(&mut task).await;
    running.release.notify_one();
    assert!(matches!(
        timeout(Duration::from_secs(60), task).await??,
        Err(VendorError::Cancelled(_))
    ));
    assert_eq!(
        *running.events.lock().unwrap(),
        ["xcodebuild 1 started", "xcodebuild 1 closed"]
    );
    v(d.dispose().await)?;
    v(d.join_pending().await)?;
    Ok(())
}
#[tokio::test]
async fn a_cancelled_batch_that_has_not_finished_in_time_is_not_waited_for_any_longer_and_disposal_still_waits_for_it()
-> R {
    let mut h = Harness::new("")?;
    h.options.cancelled_batch_timeout = runtime::duration(120);
    let d = prepared(&h).await?;
    let running = Running::install(&h, vec![]);
    let (signal, task) = start(&h, d.clone(), &running).await?;
    signal.cancel(cancelled("caller"));
    assert!(matches!(
        timeout(Duration::from_secs(60), task).await??,
        Err(VendorError::Cancelled(_))
    ));
    assert_eq!(running.events.lock().unwrap().len(), 1);
    let device = d.clone();
    let mut disposal = tokio::spawn(async move { device.dispose().await });
    still_waiting(&mut disposal).await;
    running.release.notify_one();
    v(timeout(Duration::from_secs(60), disposal).await??)?;
    assert_eq!(running.events.lock().unwrap().len(), 2);
    v(d.join_pending().await)?;
    Ok(())
}
#[tokio::test]
async fn disposal_during_the_cancellation_of_a_batch_returns_only_after_its_xcodebuild_session_has_closed() -> R {
    let h = Harness::new("")?;
    let d = prepared(&h).await?;
    let running = Running::install(&h, vec![]);
    let (signal, task) = start(&h, d.clone(), &running).await?;
    signal.cancel(cancelled("caller"));
    timeout(Duration::from_secs(60), running.closing.notified()).await?;
    let device = d.clone();
    let mut disposal = tokio::spawn(async move { device.dispose().await });
    still_waiting(&mut disposal).await;
    running.release.notify_one();
    v(timeout(Duration::from_secs(60), disposal).await??)?;
    assert_eq!(running.events.lock().unwrap().len(), 2);
    assert!(matches!(task.await?, Err(VendorError::Cancelled(_))));
    v(d.join_pending().await)?;
    Ok(())
}
#[tokio::test]
async fn disposal_stops_waiting_for_a_batch_that_has_not_finished_in_time() -> R {
    let mut h = Harness::new("")?;
    h.options.cancelled_batch_timeout = runtime::duration(100);
    let d = prepared(&h).await?;
    let running = Running::install(&h, vec![]);
    let (_, task) = start(&h, d.clone(), &running).await?;
    v(timeout(Duration::from_secs(60), d.dispose()).await?)?;
    assert_eq!(running.events.lock().unwrap().len(), 1);
    running.release.notify_one();
    assert!(matches!(
        timeout(Duration::from_secs(60), task).await??,
        Err(VendorError::DeviceLost(_))
    ));
    assert_eq!(running.events.lock().unwrap().len(), 2);
    v(d.join_pending().await)?;
    Ok(())
}
#[tokio::test]
async fn a_device_that_is_being_disposed_of_takes_no_further_batch() -> R {
    let h = Harness::new("")?;
    let d = prepared(&h).await?;
    let running = Running::install(&h, vec![]);
    let (_, task) = start(&h, d.clone(), &running).await?;
    let device = d.clone();
    let mut disposal = tokio::spawn(async move { device.dispose().await });
    timeout(Duration::from_secs(60), running.closing.notified()).await?;
    let err = batch(d.clone(), h.context.clone())
        .await
        .err()
        .ok_or("expected failure")?;
    assert!(matches!(err, VendorError::DeviceLost(_)));
    assert!(
        err.failure()
            .cause
            .as_ref()
            .unwrap()
            .message
            .as_ref()
            .unwrap()
            .to_owned()
            .ends_with("has been disposed")
    );
    still_waiting(&mut disposal).await;
    running.release.notify_one();
    v(disposal.await?)?;
    assert!(matches!(
        batch(d.clone(), h.context.clone()).await,
        Err(VendorError::DeviceLost(_))
    ));
    assert_eq!(running.started.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(matches!(task.await?, Err(VendorError::DeviceLost(_))));
    v(d.join_pending().await)?;
    Ok(())
}
#[tokio::test]
async fn a_batch_level_failure_still_drops_the_device_once_its_session_has_closed() -> R {
    let h = Harness::new("")?;
    let d = prepared(&h).await?;
    let running = Running::install(&h, vec!["Test runner exited before starting test execution".into()]);
    running.release.notify_one();
    let error = timeout(Duration::from_secs(60), batch(d.clone(), h.context.clone()))
        .await?
        .err()
        .ok_or("expected failure")?;
    assert!(matches!(error, VendorError::DeviceLost(_)));
    assert_eq!(
        error.failure().cause.as_ref().unwrap().class.to_owned(),
        "com.malinskiy.marathon.apple.logparser.parser.DeviceFailureException"
    );
    assert_eq!(
        *running.events.lock().unwrap(),
        ["xcodebuild 1 started", "xcodebuild 1 closed"]
    );
    v(d.dispose().await)?;
    v(d.join_pending().await)?;
    Ok(())
}
#[tokio::test]
async fn concurrent_disposal_joins_one_completion_and_result_delivery_is_independent() -> R {
    let h = Harness::new("")?;
    let d = prepared(&h).await?;
    let running = Running::install(&h, vec![]);
    let result = marathon_core::executor::shared::SharedDeferred::new();
    let task = tokio::spawn(execute(d.clone(), h.context.clone(), result.clone()));
    timeout(Duration::from_secs(60), running.opened.notified()).await?;
    let device = d.clone();
    let mut first = tokio::spawn(async move { device.dispose().await });
    timeout(Duration::from_secs(60), running.closing.notified()).await?;
    let device = d.clone();
    let mut second = tokio::spawn(async move { device.dispose().await });
    use marathon_core::runtime::Deferred;
    assert!(result.completed().is_some());
    still_waiting(&mut first).await;
    still_waiting(&mut second).await;
    running.release.notify_one();
    v(first.await?)?;
    v(second.await?)?;
    assert!(matches!(task.await?, Err(VendorError::DeviceLost(_))));
    v(d.dispose().await)?;
    v(d.join_pending().await)?;
    Ok(())
}

#[tokio::test]
async fn default_budgets_and_fresh_disposal_wait_use_virtual_time() -> R {
    let mut h = Harness::new("")?;
    assert_eq!(h.options.session_close_timeout.to_millis(), Some(60_000));
    assert_eq!(h.options.cancelled_batch_timeout.to_millis(), Some(70_000));
    assert_eq!(marathon_macos::device::CANCELLED_BATCH_GRACE.to_millis(), Some(10_000));
    // Keep the shared session's independent timeout from ending this blocked batch first.
    h.options.session_close_timeout = runtime::duration(1_000_000);
    let d = prepared(&h).await?;
    let running = Running::install(&h, vec![]);
    let (signal, task) = start(&h, d.clone(), &running).await?;
    tokio::time::pause();
    signal.cancel(cancelled("caller"));
    running.closing.notified().await;
    tokio::time::advance(Duration::from_secs(69)).await;
    assert!(!task.is_finished());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(matches!(task.await?, Err(VendorError::Cancelled(_))));
    let device = d.clone();
    let disposal = tokio::spawn(async move { device.dispose().await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(69)).await;
    assert!(!disposal.is_finished());
    tokio::time::advance(Duration::from_secs(1)).await;
    v(disposal.await?)?;
    let warnings = h
        .logs
        .records()
        .into_iter()
        .filter(|r| r.message.to_owned().contains("not waiting for it any longer"))
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 2);
    assert!(
        warnings
            .iter()
            .all(|r| r.level == jcompat::logging::Level::Warn && r.message.to_owned().contains("PT1M10S"))
    );
    running.release.notify_one();
    v(d.join_pending().await)?;
    tokio::time::resume();
    Ok(())
}
#[tokio::test]
async fn dropping_a_disposal_waiter_does_not_abandon_the_shared_completion() -> R {
    let h = Harness::new("")?;
    let d = prepared(&h).await?;
    let running = Running::install(&h, vec![]);
    let (_, task) = start(&h, d.clone(), &running).await?;
    let device = d.clone();
    let first = tokio::spawn(async move { device.dispose().await });
    timeout(Duration::from_secs(60), running.closing.notified()).await?;
    first.abort();
    assert!(first.await.err().is_some_and(|e| e.is_cancelled()));
    running.release.notify_one();
    v(timeout(Duration::from_secs(60), d.dispose()).await?)?;
    let error = task.await?.err().ok_or("disposal cancellation")?;
    assert!(matches!(error, VendorError::DeviceLost(_)));
    let oracle: serde_json::Value = include_str!("../../../fixtures/oracle/macos/probes.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|r| r["case"] == "dispose-execute")
        .ok_or("disposal oracle")?;
    let mut chain = vec![];
    let mut current = Some(error.failure());
    while let Some(f) = current {
        let message = f.message.clone();
        chain.push(serde_json::json!({"class":f.class.to_owned(),"message":message}));
        current = f.cause.as_deref();
    }
    let expected = oracle["error"].to_string().replace(
        "LazyDeferredCoroutine{Cancelled}@<identity>",
        "LazyDeferredCoroutine{Cancelled}",
    );
    assert_eq!(
        serde_json::json!(chain),
        serde_json::from_str::<serde_json::Value>(&expected)?
    );
    v(d.join_pending().await)?;
    Ok(())
}
#[tokio::test]
async fn preparation_continues_after_its_caller_is_cancelled() -> R {
    let h = Harness::new("")?;
    let d = h.device();
    v(d.setup(&h.context).await)?;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (en, re) = (entered.clone(), release.clone());
    let original = h.worker.fake.before.lock().unwrap().clone();
    *h.worker.fake.before.lock().unwrap() = Arc::new(move |cmd| {
        let (en, re, original, first) = (en.clone(), re.clone(), original.clone(), first.clone());
        Box::pin(async move {
            if cmd.first().is_some_and(|s| s == "rm") && first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                en.notify_one();
                re.notified().await;
            }
            original(cmd).await
        })
    });
    let signal = Signal::new();
    let mut context = h.context.clone();
    context.cancellation = signal.clone();
    let device = d.clone();
    let config = h.options.dependencies.configuration.clone();
    let task = tokio::spawn(async move { device.prepare(config, context).await });
    timeout(Duration::from_secs(60), entered.notified()).await?;
    signal.cancel(cancelled("caller"));
    assert!(matches!(task.await?, Err(VendorError::Cancelled(_))));
    v(d.dispose().await)?;
    assert!(h.bridge.sent.lock().unwrap().is_empty());
    release.notify_one();
    v(timeout(Duration::from_secs(60), d.join_pending()).await?)?;
    assert!(
        h.bridge
            .sent
            .lock()
            .unwrap()
            .iter()
            .any(|(p, _)| p.ends_with(".xctestrun"))
    );
    Ok(())
}
#[tokio::test]
async fn inline_bundle_pull_finishes_after_result_delivery_and_before_execute_returns() -> R {
    use marathon_core::runtime::Deferred;
    let mut h = Harness::new("")?;
    Arc::make_mut(&mut h.options.vendor).xcresult.pulling_policy = marathon_config::vendor::PullingPolicy::Always;
    Arc::make_mut(&mut h.options.vendor).xcresult.remote_clean = true;
    let d = prepared(&h).await?;
    let before = h.worker.requests.lock().unwrap().len();
    h.bridge.block_pull.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = marathon_core::executor::shared::SharedDeferred::new();
    let mut task = tokio::spawn(execute(d.clone(), h.context.clone(), result.clone()));
    timeout(Duration::from_secs(60), h.bridge.pull_entered.notified()).await?;
    assert!(result.completed().is_some());
    still_waiting(&mut task).await;
    h.bridge.pull_release.notify_one();
    v(timeout(Duration::from_secs(60), task).await??)?;
    let oracle: serde_json::Value = include_str!("../../../fixtures/oracle/macos/probes.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|r| r["case"] == "inline-xcresult")
        .ok_or("inline oracle")?;
    let expected = &oracle["requests"].as_array().ok_or("requests")?[6..];
    let actual=h.worker.requests.lock().unwrap()[before..].iter().map(|r| {
        let mut row=serde_json::json!({"argv":r.command,"timeout":r.timeout.to_string(),"idle":r.idle_timeout.to_string(),"env":r.env});
        if let Some(workdir)=&r.workdir {row["workdir"]=serde_json::json!(workdir);}
        row
    }).collect::<Vec<_>>();
    assert_eq!(actual, expected);
    assert_eq!(
        oracle["events"],
        serde_json::json!(["pull:result-completed=true", "execute-returned"])
    );
    v(d.dispose().await)?;
    v(d.join_pending().await)?;
    Ok(())
}
#[tokio::test]
async fn blocking_session_close_does_not_block_the_single_device_thread() -> R {
    use marathon_apple::cmd::{BaseCommand, CommandSession, SessionControl};
    let h = Harness::new("")?;
    let d = prepared(&h).await?;
    let opened = Arc::new(tokio::sync::Notify::new());
    let closing = Arc::new(tokio::sync::Notify::new());
    let latch = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let (op, cl, la, io) = (
        opened.clone(),
        closing.clone(),
        latch.clone(),
        h.options.dependencies.io.clone(),
    );
    *h.worker.fake.session.lock().unwrap() = Arc::new(move |cmd| {
        if cmd.get(2).is_none_or(|s| s != "test-without-building") {
            return Ok(None);
        }
        op.notify_one();
        let (cl, la) = (cl.clone(), la.clone());
        Ok(Some(CommandSession::new(
            BaseCommand::new(100, io.clone()),
            SessionControl {
                is_alive: Box::new(|| true),
                terminate: Box::new(|| Ok(())),
                kill: Box::new(|| Ok(())),
                close: Box::new(move || {
                    let (cl, la) = (cl.clone(), la.clone());
                    Box::pin(async move {
                        tokio::task::spawn_blocking(move || {
                            let (lock, condition) = &*la;
                            let released = lock.lock().unwrap();
                            cl.notify_one();
                            let (released, _) = condition
                                .wait_timeout_while(released, Duration::from_secs(60), |r| !*r)
                                .unwrap();
                            assert!(*released, "blocked close watchdog");
                        })
                        .await
                        .map_err(|e| VendorError::Other(marathon_apple::cmd::failure("TestTask", e.to_string())))?;
                        Ok(())
                    })
                }),
            },
        )))
    });
    let signal = Signal::new();
    let mut context = h.context.clone();
    context.cancellation = signal.clone();
    let mut task = tokio::spawn(batch(d.clone(), context));
    timeout(Duration::from_secs(60), opened.notified()).await?;
    signal.cancel(cancelled("caller"));
    timeout(Duration::from_secs(60), closing.notified()).await?;
    still_waiting(&mut task).await;
    *latch.0.lock().unwrap() = true;
    latch.1.notify_all();
    assert!(matches!(
        timeout(Duration::from_secs(60), task).await??,
        Err(VendorError::Cancelled(_))
    ));
    v(d.dispose().await)?;
    v(d.join_pending().await)?;
    Ok(())
}

#[tokio::test]
async fn only_first_concurrent_disposer_logs_join_failure() -> R {
    let mut h = Harness::new("")?;
    // Overflow fails the cancelled-batch wait without waiting on a real timeout.
    h.options.cancelled_batch_timeout = jcompat::jtime::duration_of(i64::MAX, 0).ok_or("duration")?;
    let d = prepared(&h).await?;
    let running = Running::install(&h, vec![]);
    let (_, task) = start(&h, d.clone(), &running).await?;
    let (first, second) = tokio::join!(d.dispose(), d.dispose());
    v(first)?;
    v(second)?;
    v(d.dispose().await)?;
    let errors = h
        .logs
        .records()
        .into_iter()
        .filter(|record| record.message.to_owned().starts_with("Error disposing"))
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].level, jcompat::logging::Level::Debug);
    running.release.notify_one();
    let error = task.await?.err().ok_or("cancelled execution overflow")?;
    assert_eq!(error.failure().class, "java.lang.ArithmeticException".to_owned());
    v(d.join_pending().await)?;
    Ok(())
}
/// U163: the installer doesn't retry a stalled send, and core takes the device out of its pool on this error.
#[tokio::test]
async fn a_stalled_send_ends_prepare_after_one_send_u163() -> R {
    let h = Harness::new("")?;
    h.bridge.stalled.store(true, std::sync::atomic::Ordering::SeqCst);
    let device = h.device();
    v(device.setup(&h.context).await)?;
    let error = device
        .prepare(h.options.dependencies.configuration.clone(), h.context.clone())
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            VendorError::LeavePool {
                reason: marathon_core::error::LeavePoolReason::TransferStalled,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(h.bridge.stalled_sends.load(std::sync::atomic::Ordering::SeqCst), 1);
    Ok(())
}
/// D54: a transfer for a Mac whose connection is lost fails as a connection failure, not as a cancellation, and a receive
/// is the `false` its listener deals with. Kotlin's waits for rsync, without a limit if the Mac stopped answering (A104).
#[tokio::test]
async fn a_lost_connection_fails_a_transfer_in_flight_as_an_ordinary_error_d54() -> R {
    use marathon_apple::cmd::remote::ssh::sshj::error::CONNECTION_EXCEPTION;
    use std::sync::atomic::Ordering;
    let h = Harness::new("")?;
    let device = h.device();
    h.bridge.hang.store(true, Ordering::SeqCst);
    let src = h.root.path().join("MacApp.app");
    let send = {
        let (device, src, context) = (device.clone(), src.clone(), h.context.clone());
        tokio::spawn(async move {
            device
                .push_folder(&src, "/tmp/marathon/shared/MacApp.app", &context)
                .await
        })
    };
    timeout(Duration::from_secs(60), h.bridge.hanging.notified()).await?;
    let keep_alive = "Did not receive any keep-alive response for 60 seconds";
    h.worker
        .fake
        .lose(marathon_test_support::apple::fake_simulator_worker::connection_exception(keep_alive));
    let error = timeout(Duration::from_secs(60), send)
        .await
        .map_err(|_| "the lost connection did not end the send")??
        .unwrap_err();
    let VendorError::Other(failure) = error else {
        panic!("{error:?}");
    };
    assert_eq!(failure.class.to_owned(), CONNECTION_EXCEPTION);
    assert_eq!(
        failure.message.map(|m| m.to_owned()),
        Some("Lost the connection to local during a transfer".into())
    );
    assert_eq!(
        failure.cause.and_then(|c| c.message).map(|m| m.to_owned()),
        Some(keep_alive.into())
    );
    let back = h.root.path().join("back");
    assert_eq!(
        timeout(
            Duration::from_secs(60),
            device.pull_folder("/tmp/marathon/result.xcresult", &back, &h.context)
        )
        .await?,
        Ok(false)
    );
    // The caller's cancellation still returns as one.
    h.worker.fake.connected.store(true, Ordering::SeqCst);
    let other = Harness::new("")?;
    other.bridge.hang.store(true, Ordering::SeqCst);
    let device = other.device();
    let signal = Signal::new();
    let context = marathon_core::vendor::VendorContext {
        cancellation: signal.clone(),
        ..other.context.clone()
    };
    let send = {
        let (device, src) = (device.clone(), src.clone());
        tokio::spawn(async move {
            device
                .push_folder(&src, "/tmp/marathon/shared/MacApp.app", &context)
                .await
        })
    };
    timeout(Duration::from_secs(60), other.bridge.hanging.notified()).await?;
    let reason = cancelled("the run was cancelled");
    signal.cancel(reason.clone());
    assert_eq!(
        timeout(Duration::from_secs(60), send).await??,
        Err(VendorError::Cancelled(reason))
    );
    // A receive doesn't take it: under the cancelled context the pull goes on until the connection is lost.
    let context = marathon_core::vendor::VendorContext {
        cancellation: signal,
        ..other.context.clone()
    };
    let pull = {
        let (device, back) = (device.clone(), other.root.path().join("back"));
        tokio::spawn(async move {
            device
                .pull_folder("/tmp/marathon/result.xcresult", &back, &context)
                .await
        })
    };
    timeout(Duration::from_secs(60), other.bridge.hanging.notified()).await?;
    other
        .worker
        .fake
        .lose(marathon_test_support::apple::fake_simulator_worker::connection_exception(keep_alive));
    assert_eq!(timeout(Duration::from_secs(60), pull).await??, Ok(false));
    Ok(())
}
