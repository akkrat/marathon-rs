//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDeviceBatchTest.kt
use marathon_error::FailureKind;
mod simulator_support;
mod support;
use marathon_apple::{
    apple_device::AppleDevice,
    cmd::{cancelled, failure},
    test::TestRequest,
    xcodebuild_session::XcodebuildSession,
};
use marathon_config::vendor::{GrantLifecycle, IosPermission};
use marathon_core::{
    error::VendorError,
    executor::shared::{SharedDeferred, Signal},
    vendor::{Device, VendorContext},
};
use simulator_support::*;
use std::sync::{Arc, atomic::Ordering};
#[tokio::test(start_paused = true)]
async fn batches_in_which_no_test_finished_take_the_simulator_out_of_the_pool() -> TestResult {
    let h = Harness::new(|v| v.max_batches_without_progress = 3)?;
    let (d, _) = prepared(&h).await?;
    assert!(run(&h, &d).await.0.is_ok());
    assert_eq!(run(&h, &d).await.1.unwrap().uncompleted.len(), 2);
    assert!(!d.lost());
    assert!(matches!(run(&h, &d).await.0, Err(VendorError::DeviceLost(_))));
    assert!(d.lost());
    d.dispose().await.unwrap();
    assert!(!d.lost());
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn by_default_batches_in_which_no_test_finished_keep_the_simulator() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, _) = prepared(&h).await?;
    for _ in 0..5 {
        assert!(run(&h, &d).await.0.is_ok());
    }
    assert!(!d.lost());
    h.finish(&[d]).await;
    Ok(())
}
// D47's witness: why this setting doesn't excuse an unlimited uncompletedTestRetryQuota. The batch comes back uncompleted
// each time (DeviceActor's recoverable path) and no count moves, so the simulator takes the next batch for ever.
#[tokio::test(start_paused = true)]
async fn an_unclassified_error_is_not_counted_as_a_batch_without_progress() -> TestResult {
    let h = Harness::new(|v| {
        v.max_batches_without_progress = 1;
        v.permissions.lifecycle = GrantLifecycle::BeforeEachBatch;
        v.permissions.grant = vec![IosPermission::Calendar];
    })?;
    let (d, _) = prepared(&h).await?;
    let boot = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |command| {
        if command.get(2).is_some_and(|s| s == "privacy") {
            return Box::pin(async { Err(VendorError::Other(failure(FailureKind::Io, "probe"))) });
        }
        let reply = boot.answer(&command);
        Box::pin(async move { Ok(reply) })
    });
    for _ in 0..5 {
        let (r, results) = run(&h, &d).await;
        assert!(matches!(r, Err(VendorError::Other(_))), "{r:?}");
        assert!(results.is_none());
        assert!(!d.lost());
        assert!(!d.retired());
    }
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_batch_in_which_a_test_passed_or_failed_keeps_the_simulator() -> TestResult {
    let h = Harness::new(|v| v.max_batches_without_progress = 3)?;
    let (d, s) = prepared(&h).await?;
    for _ in 0..2 {
        assert!(run(&h, &d).await.0.is_ok());
    }
    s.output(failed(1));
    assert_eq!(run(&h, &d).await.1.unwrap().failed.len(), 1);
    s.output(vec![]);
    for _ in 0..2 {
        assert!(run(&h, &d).await.0.is_ok());
    }
    s.output(passed(2));
    assert_eq!(run(&h, &d).await.1.unwrap().finished.len(), 1);
    s.output(vec![]);
    for _ in 0..2 {
        assert!(run(&h, &d).await.0.is_ok());
    }
    assert!(run(&h, &d).await.0.is_err());
    assert!(d.lost());
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_failed_test_run_in_which_a_test_failed_is_progress() -> TestResult {
    let h = Harness::new(|v| v.max_batches_without_progress = 3)?;
    let (d, s) = prepared(&h).await?;
    for _ in 0..2 {
        assert!(run(&h, &d).await.0.is_ok());
    }
    let mut lines = failed(1);
    lines.push("Test runner exited".into());
    s.output(lines);
    let (r, results) = run(&h, &d).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(results.unwrap().failed.len(), 1);
    s.output(vec![]);
    for _ in 0..2 {
        assert!(run(&h, &d).await.0.is_ok());
    }
    assert!(!d.lost());
    assert!(run(&h, &d).await.0.is_err());
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn batches_stopped_by_the_idle_timeout_before_any_test_finished_take_the_simulator_out_of_the_pool() -> TestResult
{
    let h = Harness::new(|v| v.max_batches_without_progress = 3)?;
    let (d, s) = prepared(&h).await?;
    *s.plan.lock().unwrap() = OutputPlan {
        lines: vec!["Test Case '-[Suite test1]' started.".into()],
        failure: Some(failure(FailureKind::Timeout, "idle timeout reached")),
        ..Default::default()
    };
    for _ in 0..2 {
        let e = run(&h, &d).await.0.unwrap_err();
        assert_eq!(e.failure().kind.description().to_owned(), "timed out");
        assert!(!d.lost());
    }
    let e = run(&h, &d).await.0.unwrap_err();
    assert!(matches!(e, VendorError::DeviceLost(_)));
    assert!(e.failure().stack_trace().to_owned().contains("Suppressed: timed out"));
    assert!(d.lost());
    h.finish(&[d]).await;
    Ok(())
}
/// D39 (U165) with the default settings: three silent batches, where Kotlin kept the simulator for ever.
#[tokio::test(start_paused = true)]
async fn silent_batches_take_the_simulator_out_with_the_default_settings_d39() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    *s.plan.lock().unwrap() = OutputPlan {
        lines: vec!["Test Case '-[Suite test1]' started.".into()],
        failure: Some(failure(FailureKind::Timeout, "idle timeout reached")),
        ..Default::default()
    };
    for _ in 0..2 {
        let e = run(&h, &d).await.0.unwrap_err();
        assert_eq!(e.failure().kind.description().to_owned(), "timed out");
        assert!(!d.lost());
    }
    let e = run(&h, &d).await.0.unwrap_err();
    assert!(matches!(e, VendorError::DeviceLost(_)));
    assert!(e.failure().stack_trace().to_owned().contains("idle timeout reached"));
    assert!(d.lost());
    h.finish(&[d]).await;
    Ok(())
}
/// D39 (U165): a batch in which a test finished keeps the simulator however often its output then goes silent.
#[tokio::test(start_paused = true)]
async fn silent_batches_with_progress_keep_the_simulator_d39() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    let mut lines = failed(1);
    lines.push("Test Case '-[Suite test2]' started.".into());
    *s.plan.lock().unwrap() = OutputPlan {
        lines,
        failure: Some(failure(FailureKind::Timeout, "idle timeout reached")),
        ..Default::default()
    };
    for _ in 0..10 {
        let e = run(&h, &d).await.0.unwrap_err();
        assert_eq!(e.failure().kind.description().to_owned(), "timed out");
        assert!(!d.lost());
    }
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_cancelled_batch_doesn_t_lose_the_simulator() -> TestResult {
    let h = Harness::new(|_| {})?;
    for target in [
        "marathon_apple::apple_device_test_runner",
        "marathon_apple::listener::debug",
    ] {
        h.services
            .logging
            .registry
            .lock()
            .unwrap()
            .set_level(target, marathon_logging::Level::DEBUG);
    }
    let (d, s) = prepared(&h).await?;
    s.plan.lock().unwrap().hang = true;
    let signal = Signal::new();
    let op = run_with(
        &h,
        &d,
        VendorContext {
            cancellation: signal.clone(),
            ..h.services.context.clone()
        },
    );
    tokio::pin!(op);
    tokio::select! {r=&mut op=>panic!("{r:?}"),_=s.opened.wait()=>{}}
    signal.cancel(cancelled("caller"));
    let (r, _) = op.await;
    assert!(matches!(r, Err(VendorError::Cancelled(_))));
    assert!(!d.lost());
    assert!(!d.retired());
    h.finish(std::slice::from_ref(&d)).await;
    let logs = h.logs.lock().unwrap();
    assert!(
        logs.iter()
            .all(|(target, _, _, _)| target != "marathon_apple::batch_outcome")
    );
    assert!(
        logs.iter()
            .any(|(_, level, message, _)| *level == marathon_logging::Level::DEBUG
                && message.starts_with("Batch cancelled: "))
    );
    assert!(
        logs.iter()
            .all(|(_, _, message, _)| !message.contains("got stuck") && !message.contains("DeferredCoroutine"))
    );
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_device_level_failure_that_a_new_setup_can_fix_loses_the_simulator_for_its_recovery() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    s.output(vec!["CoreSimulatorService connection became invalid".into()]);
    assert!(matches!(run(&h, &d).await.0, Err(VendorError::DeviceLost(_))));
    assert!(d.lost());
    assert!(!d.retired());
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_dead_system_app_takes_the_simulator_out_without_a_recovery() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    s.output(vec!["Terminating since there is no system app".into()]);
    assert!(run(&h, &d).await.0.is_err());
    assert!(!d.lost());
    assert!(d.retired());
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_dead_system_app_takes_the_simulator_out_even_if_its_run_is_cancelled_before_it_ends() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    let gate = SharedDeferred::new();
    *s.plan.lock().unwrap() = OutputPlan {
        lines: vec!["Terminating since there is no system app".into()],
        close_gate: Some(gate.clone()),
        ..Default::default()
    };
    let signal = Signal::new();
    let op = run_with(
        &h,
        &d,
        VendorContext {
            cancellation: signal.clone(),
            ..h.services.context.clone()
        },
    );
    tokio::pin!(op);
    tokio::select! {r=&mut op=>panic!("{r:?}"),_=s.closing.wait()=>{}}
    assert!(d.retired());
    signal.cancel(cancelled("caller"));
    gate.complete(Ok(()));
    assert!(op.await.0.is_err());
    assert!(d.retired());
    assert!(!d.lost());
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_simulator_lost_while_a_test_parsing_session_of_it_has_not_closed_is_taken_out_without_a_recovery()
-> TestResult {
    use marathon_config::vendor::{GrantLifecycle, IosPermission};
    let h = Harness::new(|v| {
        v.permissions.lifecycle = GrantLifecycle::BeforeEachBatch;
        v.permissions.grant = vec![IosPermission::Calendar];
    })?;
    let (d, s) = prepared(&h).await?;
    let gate = SharedDeferred::new();
    *s.plan.lock().unwrap() = OutputPlan {
        lines: vec!["Test runner exited".into()],
        close_gate: Some(gate.clone()),
        ..Default::default()
    };
    let parsing = XcodebuildSession::new(h.services.runtime.clone(), h.services.logging.clone());
    let events = d
        .execute_test_request(
            TestRequest::new("/tmp/marathon", "/tmp/marathon/parse.xctestrun", false),
            parsing.clone(),
            &h.services.context,
        )
        .await
        .unwrap();
    events.receive(&h.services.context).await.unwrap();
    assert!(parsing.stop(support::duration(20), &h.services.context).await.is_some());
    let boot = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |c| {
        if c.get(2).is_some_and(|s| s == "privacy") {
            return Box::pin(async {
                Err(VendorError::Other(
                    marathon_test_support::apple::fake_simulator_worker::connection_exception("channel open failed"),
                ))
            });
        }
        let a = boot.answer(&c);
        Box::pin(async move { Ok(a) })
    });
    assert!(run(&h, &d).await.0.is_err());
    assert!(d.retired());
    assert!(!d.lost());
    gate.complete(Ok(()));
    parsing.join_producer().await.unwrap();
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_simulator_whose_xcodebuild_session_doesn_t_close_is_taken_out_without_a_recovery() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    let gate = SharedDeferred::new();
    s.plan.lock().unwrap().close_gate = Some(gate.clone());
    assert!(run(&h, &d).await.0.is_err());
    assert!(d.retired());
    assert!(!d.lost());
    gate.complete(Ok(()));
    assert!(
        d.await_session_closed(support::duration(1000), &h.services.context)
            .await
            .unwrap()
    );
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_simulator_waits_for_a_batch_only_between_its_batches_while_it_is_in_the_pool() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    assert!(d.waiting_since().is_none());
    s.output(passed(1));
    assert!(run(&h, &d).await.0.is_ok());
    assert!(d.waiting_since().is_some());
    s.plan.lock().unwrap().hang = true;
    let signal = Signal::new();
    let op = run_with(
        &h,
        &d,
        VendorContext {
            cancellation: signal.clone(),
            ..h.services.context.clone()
        },
    );
    tokio::pin!(op);
    assert!(futures_util::poll!(op.as_mut()).is_pending());
    assert!(d.waiting_since().is_none());
    signal.cancel(cancelled("done"));
    assert!(op.await.0.is_err());
    assert!(d.waiting_since().is_some());
    d.dispose().await.unwrap();
    assert!(d.waiting_since().is_none());
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_disposed_simulator_is_not_lost() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    d.dispose().await.unwrap();
    assert!(matches!(run(&h, &d).await.0, Err(VendorError::DeviceLost(_))));
    assert!(!d.lost());
    assert_eq!(s.count.load(Ordering::SeqCst), 0);
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test]
async fn a_wedged_batch_that_keeps_writing_output_ends_at_the_batch_timeout() -> TestResult {
    use marathon_apple::cmd::{CommandExecutor, CommandHost, CommandRequest, CommandSession};
    use marathon_core::runtime::VendorFuture;
    use std::sync::Mutex;
    struct Worker {
        fake: Arc<marathon_test_support::apple::FakeSimulatorWorker>,
        process: marathon_apple::cmd::local::KotlinProcessCommandExecutor,
        timeouts: Mutex<Vec<i64>>,
    }
    impl CommandExecutor for Worker {
        fn host(&self) -> &CommandHost {
            self.fake.host()
        }
        fn component(&self) -> &marathon_logging::Component {
            self.fake.component()
        }
        fn connected(&self) -> bool {
            self.fake.connected()
        }
        fn log_error(&self, text: String) {
            self.fake.log_error(text)
        }
        fn close(&self) -> VendorFuture<'_, ()> {
            self.process.close()
        }
        fn execute(&self, mut request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandSession> {
            if request.command.get(2).is_some_and(|s| s == "test-without-building") {
                self.timeouts.lock().unwrap().push(request.timeout.to_millis().unwrap());
                request.workdir = None;
                request.command = vec![
                    "sh".into(),
                    "-c".into(),
                    "while true; do echo still running; sleep 0.05; done".into(),
                ];
                self.process.execute(request, context)
            } else {
                self.fake.execute(request, context)
            }
        }
    }
    let mut h = Harness::new(|_| {})?;
    Arc::make_mut(&mut h.services.configuration).test_batch_timeout_millis = 1000;
    Arc::make_mut(&mut h.services.configuration).test_output_timeout_millis = 600_000;
    let logging = h.services.logging.clone();
    let worker = Arc::new(Worker {
        fake: h.worker.clone(),
        process: marathon_apple::cmd::local::KotlinProcessCommandExecutor::new(
            Arc::new(support::Runtime),
            logging.registry.clone(),
            move |logger, message| (logging.sink)(logger, marathon_logging::Level::ERROR, message, None),
            false,
            128,
        ),
        timeouts: Mutex::default(),
    });
    let d = h
        .device_on(
            worker.clone(),
            "a",
            Arc::new(
                marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext::new(
                    marathon_config::devices::Transport::Local,
                    h.clock.clone(),
                ),
            ),
        )
        .await?;
    h.prepare(&d).await.unwrap();
    let (r, result) = support::watchdog(run(&h, &d)).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(result.unwrap().uncompleted.len(), 2);
    assert_eq!(*worker.timeouts.lock().unwrap(), [1000]);
    h.finish(&[d]).await;
    worker.close().await.unwrap();
    let logs = h.logs.lock().unwrap();
    let warnings = logs
        .iter()
        .filter(|(target, _, _, _)| target == "marathon_apple::batch_outcome")
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].2.ends_with(": marathon ended the session: batch timeout"));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn empty_batches_warn_once_with_exit_signal_missing_status_and_batch_timeout_x57() -> TestResult {
    use marathon_apple::cmd::base_command::CommandTermination;
    use marathon_logging::{Level, Vendor};
    for (status, termination, reason) in [
        (Some(65), None, "xcodebuild exited with status 65"),
        (Some(0), None, "xcodebuild exited with status 0"),
        (None, None, "xcodebuild ended without an exit status"),
        (
            None,
            Some(CommandTermination::Signal("KILL".into())),
            "xcodebuild was terminated by signal KILL",
        ),
        (
            None,
            Some(CommandTermination::BatchTimeout),
            "marathon ended the session: batch timeout",
        ),
    ] {
        let h = Harness::new(|_| {})?;
        h.services
            .logging
            .registry
            .lock()
            .unwrap()
            .configure(false, Vendor::Apple { compact_output: false });
        let (d, s) = prepared(&h).await?;
        *s.plan.lock().unwrap() = OutputPlan {
            exit_status: Some(status),
            termination,
            delay_millis: 30_000,
            ..Default::default()
        };
        let (outcome, results) = run(&h, &d).await;
        assert!(outcome.is_ok(), "{outcome:?}");
        let results = results.unwrap();
        assert!(results.finished.is_empty() && results.failed.is_empty());
        assert_eq!(results.uncompleted.len(), 2);
        h.finish(std::slice::from_ref(&d)).await;
        let logs = h.logs.lock().unwrap();
        let warnings = logs
            .iter()
            .filter(|(target, _, _, _)| target == "marathon_apple::batch_outcome")
            .collect::<Vec<_>>();
        assert_eq!(warnings.len(), 1, "{logs:?}");
        assert_eq!(warnings[0].1, Level::WARN);
        assert_eq!(
            warnings[0].2,
            format!(
                "Apple test batch on {} returned no results for 2 tests after 30000 ms: {reason}",
                d.serial()
            )
        );
        assert!(warnings[0].3.is_none());
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn empty_batches_warn_with_the_known_output_timeout_or_lost_connection_x57() -> TestResult {
    use marathon_apple::cmd::remote::ssh::sshj::error::CONNECTION_EXCEPTION;
    for (kind, message) in [
        (FailureKind::Timeout, "idle timeout PT30S reached"),
        (CONNECTION_EXCEPTION, "worker connection closed"),
    ] {
        let h = Harness::new(|_| {})?;
        let (d, s) = prepared(&h).await?;
        *s.plan.lock().unwrap() = OutputPlan {
            lines: vec!["Test Case '-[Suite test1]' started.".into()],
            failure: Some(failure(kind.clone(), message)),
            delay_millis: 30_000,
            ..Default::default()
        };
        let (outcome, results) = run(&h, &d).await;
        assert!(outcome.is_err());
        assert_eq!(results.unwrap().uncompleted.len(), 2);
        h.finish(std::slice::from_ref(&d)).await;
        let logs = h.logs.lock().unwrap();
        let warnings = logs
            .iter()
            .filter(|(target, _, _, _)| target == "marathon_apple::batch_outcome")
            .collect::<Vec<_>>();
        assert_eq!(warnings.len(), 1, "{logs:?}");
        let reason = if kind == FailureKind::Timeout {
            "marathon ended the session: test output timeout (timed out: idle timeout PT30S reached)"
        } else {
            "marathon ended the session: lost connection to the worker (SSH connection failure: worker connection closed)"
        };
        assert_eq!(
            warnings[0].2,
            format!(
                "Apple test batch on {} returned no results for 2 tests after 30000 ms: {reason}",
                d.serial()
            )
        );
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn partial_batches_do_not_warn_about_empty_results_x57() -> TestResult {
    for lines in [passed(1), failed(1)] {
        let h = Harness::new(|_| {})?;
        let (d, s) = prepared(&h).await?;
        *s.plan.lock().unwrap() = OutputPlan {
            lines,
            failure: Some(failure(FailureKind::Timeout, "idle timeout PT30S reached")),
            ..Default::default()
        };
        let (_, results) = run(&h, &d).await;
        let results = results.unwrap();
        assert_eq!(results.finished.len() + results.failed.len(), 1);
        assert_eq!(results.uncompleted.len(), 1);
        h.finish(&[d]).await;
        assert!(
            h.logs
                .lock()
                .unwrap()
                .iter()
                .all(|(target, _, _, _)| target != "marathon_apple::batch_outcome")
        );
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn results_for_tests_outside_the_batch_do_not_hide_its_empty_outcome_x57() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    s.output(passed(3));
    let (_, results) = run(&h, &d).await;
    let results = results.unwrap();
    assert_eq!(results.finished.len(), 1);
    assert_eq!(results.uncompleted.len(), 2);
    h.finish(&[d]).await;
    assert_eq!(
        h.logs
            .lock()
            .unwrap()
            .iter()
            .filter(|(target, _, _, _)| target == "marathon_apple::batch_outcome")
            .count(),
        1
    );
    Ok(())
}
