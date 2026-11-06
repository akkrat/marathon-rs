//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/StartupPhasesTest.kt
mod simulator_support;
mod support;
use marathon_apple::cmd::{CommandResult, cancelled};
use marathon_core::{
    error::{LeavePoolReason, VendorError},
    vendor::Device,
};
use simulator_support::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use support::duration;
#[tokio::test(start_paused = true)]
async fn a_boot_that_takes_longer_than_the_former_30_checks_but_finishes_within_boot_wait_prepares_the_simulator()
-> TestResult {
    let h = Harness::new(|v| v.timeout_configuration.boot_wait = duration(30_000))?;
    h.boot.finish_after.store(40, Ordering::SeqCst);
    let d = h.device().await?;
    h.boot.reads.store(0, Ordering::SeqCst);
    h.prepare(&d).await.unwrap();
    assert_eq!(h.boot.boots.load(Ordering::SeqCst), 1);
    assert_eq!(h.boot.reads.load(Ordering::SeqCst), 41);
    assert_eq!(h.boot.polls.load(Ordering::SeqCst), 40);
    assert!(!d.lost());
    let logs = h.logs.lock().unwrap().clone();
    assert_eq!(
        logs.iter()
            .filter(|(_, _, m, _)| m.contains(" prepared in ")
                && m.contains(", boot ")
                && m.contains(" ms since start)"))
            .count(),
        1
    );
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
/// A booted simulator (state 3) whose readiness probe prints a started SpringBoard service with the
/// given exit status, for the first `failures` probes, and then succeeds.
fn probe_fails(h: &Harness, failures: usize, exit_code: Option<i32>) -> Arc<AtomicUsize> {
    let probes = Arc::new(AtomicUsize::new(0));
    let count = probes.clone();
    let plan = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |c| {
        let reply = if c.get(5).is_some_and(|s| s == "dumpstate") {
            let failed = count.fetch_add(1, Ordering::SeqCst) < failures;
            Some(CommandResult {
                stdout: vec!["0x4e03 M A com.apple.springboard.services".to_owned()],
                stderr: vec![],
                exit_code: if failed { exit_code } else { Some(0) },
            })
        } else {
            plan.answer(&c)
        };
        Box::pin(async move { Ok(reply) })
    });
    h.boot.state.store(3, Ordering::SeqCst);
    probes
}
#[tokio::test(start_paused = true)]
async fn one_failed_or_timed_out_probe_costs_one_more_poll_and_no_new_boot_d38() -> TestResult {
    for exit_code in [Some(1), None] {
        let h = Harness::new(|_| {})?;
        let d = h.device().await?;
        let probes = probe_fails(&h, 1, exit_code);
        assert!(d.boot(&h.services.context).await.unwrap());
        assert_eq!(probes.load(Ordering::SeqCst), 2);
        assert_eq!(h.boot.boots.load(Ordering::SeqCst), 0);
        assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 0);
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_probe_that_never_succeeds_is_bounded_by_boot_wait_and_erases_nothing_d38() -> TestResult {
    let h = Harness::new(|_| {})?;
    let d = h.device().await?;
    probe_fails(&h, usize::MAX, None);
    let e = h.prepare(&d).await.unwrap_err();
    assert!(matches!(
        e,
        VendorError::LeavePool {
            reason: LeavePoolReason::NotBooted,
            ..
        }
    ));
    // One setup's worst for a persistently failing probe: one boot wait, a shutdown, one fresh boot, a
    // second boot wait, a shutdown, and the simulator leaves its pool, unerased. It is marked lost, so
    // the provider can repeat this: see "The bound" in docs/wp/x17-findings.md.
    assert_eq!(h.boot.boots.load(Ordering::SeqCst), 1);
    assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 2);
    let commands = h.worker.commands.lock().unwrap().clone();
    assert!(!commands.iter().any(|c| c.iter().any(|arg| arg == "erase")));
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_boot_that_does_not_finish_within_boot_wait_is_retried_once_and_then_takes_the_simulator_out_of_its_pool()
-> TestResult {
    let h = Harness::new(|_| {})?;
    h.boot.finish_after.store(usize::MAX, Ordering::SeqCst);
    let d = h.device().await?;
    let e = h.prepare(&d).await.unwrap_err();
    assert!(matches!(
        e,
        VendorError::LeavePool {
            reason: LeavePoolReason::NotBooted,
            ..
        }
    ));
    assert!(
        e.failure()
            .message
            .as_ref()
            .unwrap()
            .to_string_lossy()
            .contains("PT0.5S")
    );
    assert_eq!(h.boot.boots.load(Ordering::SeqCst), 2);
    assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 2);
    assert_eq!(h.boot.reads.load(Ordering::SeqCst), 57);
    assert!(d.lost());
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_simulator_that_does_not_shut_down_after_a_failed_boot_is_left_as_it_is() -> TestResult {
    use marathon_apple::cmd::{BaseCommand, CommandSession, SessionControl};
    let h = Harness::new(|v| {
        v.timeout_configuration.shutdown = duration(200);
        v.timeout_configuration.shell = duration(100);
        v.timeout_configuration.boot_wait = duration(300);
    })?;
    h.boot.finish_after.store(usize::MAX, Ordering::SeqCst);
    let count = h.boot.clone();
    *h.worker.session.lock().unwrap() = Arc::new(move |cmd| {
        if cmd.get(2).is_some_and(|s| s == "shutdown") {
            count.shutdowns.fetch_add(1, Ordering::SeqCst);
            Ok(Some(CommandSession::new(
                BaseCommand::new(1, Arc::new(support::Runtime)),
                SessionControl {
                    is_alive: Box::new(|| true),
                    terminate: Box::new(|| Ok(())),
                    kill: Box::new(|| Ok(())),
                    close: Box::new(|| Box::pin(async { Ok(()) })),
                },
            )))
        } else {
            Ok(None)
        }
    });
    let d = h.device().await?;
    assert!(h.prepare(&d).await.is_err());
    assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 2);
    assert!(d.lost());
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_simulator_whose_simctl_boot_fails_is_taken_out_of_its_pool_without_waiting_for_boot_wait() -> TestResult {
    let h = Harness::new(|v| v.timeout_configuration.boot_wait = duration(60_000))?;
    h.boot.exit.store(1, Ordering::SeqCst);
    let d = h.device().await?;
    let e = h.prepare(&d).await.unwrap_err();
    assert!(
        e.failure()
            .message
            .as_ref()
            .unwrap()
            .to_string_lossy()
            .contains("simctl boot failed")
    );
    assert_eq!(h.boot.boots.load(Ordering::SeqCst), 2);
    assert!(h.clock.now() < 1000);
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_simulator_whose_state_can_t_be_read_fails_its_boot_at_once_without_simctl_boot() -> TestResult {
    let h = Harness::new(|_| {})?;
    let d = h.device().await?;
    h.boot.state.store(-1, Ordering::SeqCst);
    assert!(h.prepare(&d).await.is_err());
    assert_eq!(h.boot.boots.load(Ordering::SeqCst), 0);
    assert!(h.clock.now() < 1000);
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_boot_that_fails_once_and_then_succeeds_prepares_the_simulator() -> TestResult {
    let h = Harness::new(|_| {})?;
    h.boot.fail_first.store(true, Ordering::SeqCst);
    let d = h.device().await?;
    h.prepare(&d).await.unwrap();
    assert_eq!(h.boot.boots.load(Ordering::SeqCst), 2);
    assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 1);
    assert!(!d.lost());
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_simulator_that_boots_although_simctl_boot_failed_is_waited_for() -> TestResult {
    let h = Harness::new(|_| {})?;
    let plan = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let result = if cmd.get(2).is_some_and(|s| s == "boot") {
            plan.boots.fetch_add(1, Ordering::SeqCst);
            plan.state.store(3, Ordering::SeqCst);
            Some(failing())
        } else {
            plan.answer(&cmd)
        };
        Box::pin(async move { Ok(result) })
    });
    let d = h.device().await?;
    h.prepare(&d).await.unwrap();
    assert_eq!(h.boot.boots.load(Ordering::SeqCst), 1);
    assert!(!d.lost());
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn cancelling_a_preparation_at_the_end_of_the_run_stops_its_boot_wait_at_once() -> TestResult {
    let h = Harness::new(|_| {})?;
    h.boot.finish_after.store(usize::MAX, Ordering::SeqCst);
    let d = h.device().await?;
    let signal = marathon_core::executor::shared::Signal::new();
    let ctx = marathon_core::vendor::VendorContext {
        cancellation: signal.clone(),
        ..h.services.context.clone()
    };
    let operation = d.prepare(h.services.configuration.clone(), ctx);
    tokio::pin!(operation);
    tokio::select! { result=&mut operation=>panic!("premature {result:?}"),_=h.boot.polled.notified()=>{} }
    let reads = h.boot.reads.load(Ordering::SeqCst);
    signal.cancel(cancelled("end"));
    assert!(matches!(operation.await, Err(VendorError::Cancelled(_))));
    d.join_retained().await.unwrap();
    assert_eq!(h.boot.reads.load(Ordering::SeqCst), reads);
    assert!(!d.lost());
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn admission_is_taken_once_after_prepare_and_retires_the_device() -> TestResult {
    let h = Harness::new(|_| {})?;
    let d = h.device().await?;
    d.set_admission(Some(Arc::new(|| Ok(Some("late".into())))));
    assert!(matches!(
        h.prepare(&d).await,
        Err(VendorError::LeavePool {
            reason: LeavePoolReason::NotAdmitted,
            ..
        })
    ));
    assert!(d.retired());
    assert!(!d.lost());
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn parallel_preparation_failure_waits_for_its_cancelled_sibling_command() -> TestResult {
    use marathon_core::{executor::shared::SharedDeferred, runtime::Deferred};
    let h = Harness::new(|v| v.bundle = None)?;
    let d = h.device().await?;
    let entered = SharedDeferred::new();
    let release = SharedDeferred::new();
    let (start, gate, boot) = (entered.clone(), release.clone(), h.boot.clone());
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let pause = cmd.first().is_some_and(|s| s == "/usr/libexec/PlistBuddy")
            && cmd.get(2).is_some_and(|s| s.starts_with("'Add"));
        let (start, gate) = (start.clone(), gate.clone());
        let reply = boot.answer(&cmd);
        Box::pin(async move {
            if pause {
                start.complete(Ok(()));
                gate.wait().await?;
            }
            Ok(reply)
        })
    });
    let preparation = h.prepare(&d);
    tokio::pin!(preparation);
    tokio::select! {result=&mut preparation=>panic!("sibling was dropped: {result:?}"),_=entered.wait()=>{}}
    assert!(futures_util::poll!(preparation.as_mut()).is_pending());
    release.complete(Ok(()));
    let error = preparation.await.unwrap_err();
    assert_eq!(
        error.failure().message.as_ref().unwrap().to_string_lossy(),
        "No test bundle provided"
    );
    d.join_retained().await.unwrap();
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn wrong_state_type_after_failed_boot_warns_without_sending_shutdown() -> TestResult {
    use std::sync::atomic::AtomicUsize;
    for xml in ["<string>1</string>", "<real>1.0</real>", "<true/>"] {
        let h = Harness::new(|_| {})?;
        let d = h.device().await?;
        h.services.logging.registry.lock().unwrap().logger(
            "com.malinskiy.marathon.apple.ios.AppleSimulatorDevice",
            Some(jcompat::logging::Level::Warn),
        );
        let reads = Arc::new(AtomicUsize::new(0));
        let count = reads.clone();
        let boot = h.boot.clone();
        *h.worker.before.lock().unwrap() = Arc::new(move |command| {
            let reply = if command.first().is_some_and(|s| s == "cat") {
                // UNKNOWN starts cleanup; only the cleanup read has the wrong type.
                let state = if count.fetch_add(1, Ordering::SeqCst).is_multiple_of(2) {
                    "<integer>2</integer>"
                } else {
                    xml
                };
                Some(marathon_test_support::apple::fake_simulator_worker::ok([format!(
                    "<plist><dict><key>state</key>{state}</dict></plist>"
                )]))
            } else {
                boot.answer(&command)
            };
            Box::pin(async move { Ok(reply) })
        });
        assert!(matches!(h.prepare(&d).await, Err(VendorError::LeavePool { .. })));
        assert_eq!(reads.load(Ordering::SeqCst), 4);
        assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 0);
        let warnings: Vec<_> = h
            .logs
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, _, message, _)| message.contains("unable to shut it down after a failed boot"))
            .cloned()
            .collect();
        assert_eq!(warnings.len(), 2);
        for (_, level, _, error) in warnings {
            assert_eq!(level, jcompat::logging::Level::Warn);
            assert_eq!(error.unwrap().class.to_string_lossy(), "java.lang.ClassCastException");
        }
        h.finish(&[d]).await;
    }
    Ok(())
}
