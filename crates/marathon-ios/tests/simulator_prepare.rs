//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/StartupPhasesTest.kt
mod simulator_support;
mod support;
use marathon_apple::cmd::{CommandResult, cancelled};
use marathon_core::{
    error::{LeavePoolReason, VendorError, VendorResult},
    vendor::{Device, VendorContext},
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
        // The readiness probe, whichever form the runtime gets (D46: `print` from 18, `dumpstate` on 16 and 17).
        let reply = if c.get(4).is_some_and(|s| s == "launchctl") {
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
async fn the_readiness_probe_of_a_runtime_18_simulator_is_launchctl_print_user_foreground_d46() -> TestResult {
    // The harness's worker reports runtime 18.6.
    let h = Harness::new(|_| {})?;
    let d = h.device().await?;
    probe_fails(&h, 0, Some(0));
    assert!(d.boot(&h.services.context).await.unwrap());
    let probes: Vec<Vec<String>> = h
        .worker
        .commands
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.iter().any(|arg| arg == "launchctl"))
        .cloned()
        .collect();
    assert_eq!(
        probes,
        [["xcrun", "simctl", "spawn", "a", "launchctl", "print", "user/foreground"]]
    );
    h.finish(&[d]).await;
    Ok(())
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
#[tokio::test(start_paused = true)]
async fn a_connection_lost_during_prepare_is_not_a_failed_boot_d36() -> TestResult {
    use marathon_apple::cmd::remote::ssh::sshj::error::CONNECTION_EXCEPTION;
    let h = Harness::new(|_| {})?;
    let d = h.device().await?;
    // Only what reaches the worker changes: the safe probes answer null, critical commands fail.
    h.worker.connected.store(false, Ordering::SeqCst);
    *h.worker.before.lock().unwrap() = Arc::new(|_| Box::pin(async { Ok(None) }));
    let e = h.prepare(&d).await.unwrap_err();
    assert!(!matches!(e, VendorError::LeavePool { .. }), "{e:?}");
    assert_eq!(e.failure().class.to_string_lossy(), CONNECTION_EXCEPTION);
    assert_eq!(h.boot.boots.load(Ordering::SeqCst), 0);
    assert!(
        !h.logs
            .lock()
            .unwrap()
            .iter()
            .any(|(_, _, message, _)| message.contains("failed to boot"))
    );
    h.worker.connected.store(true, Ordering::SeqCst);
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_null_readiness_probe_on_a_lost_connection_is_not_a_failed_boot_d36_d38() -> TestResult {
    use marathon_apple::cmd::{failure, remote::ssh::sshj::error::CONNECTION_EXCEPTION};
    let h = Harness::new(|_| {})?;
    h.boot.state.store(3, Ordering::SeqCst);
    h.boot.finish_after.store(usize::MAX, Ordering::SeqCst);
    let d = h.device().await?;
    // D38 reads the null of the readiness probe as "not running", so state 3 stays Booting. The connection is gone by
    // then, and only the next descriptor read can tell that from a simulator that is slow to boot.
    let plan = h.boot.clone();
    let worker = Arc::downgrade(&h.worker);
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let lost = || VendorError::Other(failure(CONNECTION_EXCEPTION, "Not connected"));
        let worker = worker.upgrade();
        let gone = worker.as_ref().is_some_and(|w| !w.connected.load(Ordering::SeqCst));
        if cmd.iter().any(|arg| arg == "launchctl") {
            if let Some(worker) = worker {
                worker.connected.store(false, Ordering::SeqCst);
            }
            return Box::pin(async move { Err(lost()) });
        }
        if gone && cmd.first().is_some_and(|c| c == "cat") {
            return Box::pin(async move { Err(lost()) });
        }
        let reply = plan.answer(&cmd);
        Box::pin(async move { Ok(reply) })
    });
    let e = h.prepare(&d).await.unwrap_err();
    assert!(!matches!(e, VendorError::LeavePool { .. }), "{e:?}");
    assert_eq!(e.failure().class.to_string_lossy(), CONNECTION_EXCEPTION);
    assert!(
        !h.logs
            .lock()
            .unwrap()
            .iter()
            .any(|(_, _, message, _)| message.contains("failed to boot"))
    );
    h.worker.connected.store(true, Ordering::SeqCst);
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
/// Every send ends in `outcome`, counted. Receives are not what these tests are about.
struct FailingSends {
    sends: AtomicUsize,
    outcome: fn() -> marathon_core::error::VendorResult<bool>,
}
impl marathon_apple::cmd::FileBridge for FailingSends {
    fn send<'a>(&'a self, _: &'a std::path::Path, _: &'a str) -> marathon_core::runtime::VendorFuture<'a, bool> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        let outcome = self.outcome;
        Box::pin(async move { outcome() })
    }
    fn receive<'a>(&'a self, _: &'a str, _: &'a std::path::Path) -> marathon_core::runtime::VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
}
async fn prepare_with_failing_sends(
    outcome: fn() -> marathon_core::error::VendorResult<bool>,
) -> TestResult<(VendorError, usize, bool)> {
    let h = Harness::new(|_| {})?;
    let bridge = Arc::new(FailingSends {
        sends: AtomicUsize::new(0),
        outcome,
    });
    let d = h
        .device_on_bridge(
            h.worker.clone(),
            "a",
            Arc::new(
                marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext::new(
                    marathon_config::devices::Transport::Local,
                    h.clock.clone(),
                ),
            ),
            bridge.clone(),
        )
        .await?;
    let error = h.prepare(&d).await.unwrap_err();
    Ok((error, bridge.sends.load(Ordering::SeqCst), d.retired()))
}
#[tokio::test(start_paused = true)]
async fn a_stalled_send_is_not_retried_and_takes_the_simulator_out_of_the_run_u163() -> TestResult {
    let (error, sends, retired) = prepare_with_failing_sends(|| {
        Err(VendorError::LeavePool {
            reason: LeavePoolReason::TransferStalled,
            detail: marathon_apple::cmd::failure("TransferStalled", "no progress"),
        })
    })
    .await?;
    assert!(
        matches!(
            error,
            VendorError::LeavePool {
                reason: LeavePoolReason::TransferStalled,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(sends, 1);
    assert!(retired);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_send_that_fails_any_other_way_keeps_the_installers_three_attempts_u163() -> TestResult {
    let (error, sends, retired) = prepare_with_failing_sends(|| Ok(false)).await?;
    assert!(matches!(error, VendorError::DeviceSetup(_)), "{error:?}");
    assert_eq!(sends, 3);
    assert!(!retired);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_send_that_errs_any_other_way_keeps_the_installers_three_attempts_u163() -> TestResult {
    let (error, sends, retired) = prepare_with_failing_sends(|| {
        Err(VendorError::Other(marathon_apple::cmd::failure(
            "java.io.IOException",
            "x",
        )))
    })
    .await?;
    assert!(matches!(error, VendorError::Other(_)), "{error:?}");
    assert_eq!(sends, 3);
    assert!(!retired);
    Ok(())
}
/// D54: transfers that run until they are stopped, and end as the rsync bridge ends them: a cancellation returns it, a lost
/// connection fails a send and leaves a receive `false`. The plain calls, which carry no stop, would never end.
#[derive(Default)]
struct HangingTransfers {
    started: tokio::sync::Notify,
}
impl HangingTransfers {
    async fn run(&self, mut stop: marathon_apple::cmd::TransferStop, receive: bool) -> VendorResult<bool> {
        use marathon_apple::cmd::TransferStopped;
        self.started.notify_one();
        match stop.stopped().await {
            TransferStopped::Cancelled(reason) => Err(VendorError::Cancelled(reason)),
            TransferStopped::Lost(_) if receive => Ok(false),
            TransferStopped::Lost(failure) => Err(VendorError::Other(failure)),
        }
    }
}
impl marathon_apple::cmd::FileBridge for HangingTransfers {
    fn send<'a>(&'a self, _: &'a std::path::Path, _: &'a str) -> marathon_core::runtime::VendorFuture<'a, bool> {
        self.started.notify_one();
        Box::pin(std::future::pending())
    }
    fn receive<'a>(&'a self, _: &'a str, _: &'a std::path::Path) -> marathon_core::runtime::VendorFuture<'a, bool> {
        self.started.notify_one();
        Box::pin(std::future::pending())
    }
    fn send_with<'a>(
        &'a self,
        _: &'a std::path::Path,
        _: &'a str,
        stop: marathon_apple::cmd::TransferStop,
    ) -> marathon_core::runtime::VendorFuture<'a, bool> {
        Box::pin(self.run(stop, false))
    }
    fn receive_with<'a>(
        &'a self,
        _: &'a str,
        _: &'a std::path::Path,
        stop: marathon_apple::cmd::TransferStop,
    ) -> marathon_core::runtime::VendorFuture<'a, bool> {
        Box::pin(self.run(stop, true))
    }
}
async fn on_hanging_transfers(
    h: &Harness,
) -> TestResult<(
    Arc<marathon_ios::apple_simulator_device::AppleSimulatorDevice>,
    Arc<HangingTransfers>,
)> {
    let bridge = Arc::new(HangingTransfers::default());
    let d = h
        .device_on_bridge(
            h.worker.clone(),
            "a",
            Arc::new(
                marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext::new(
                    marathon_config::devices::Transport::Local,
                    h.clock.clone(),
                ),
            ),
            bridge.clone(),
        )
        .await?;
    Ok((d, bridge))
}
#[tokio::test(start_paused = true)]
async fn a_lost_connection_fails_a_transfer_in_flight_as_an_ordinary_error_d54() -> TestResult {
    use marathon_apple::{apple_device::AppleDevice, cmd::remote::ssh::sshj::error::CONNECTION_EXCEPTION};
    let h = Harness::new(|_| {})?;
    let (d, bridge) = on_hanging_transfers(&h).await?;
    let src = h.dir.path().join("tests.xctest");
    let send = {
        let (d, src, context) = (d.clone(), src.clone(), h.services.context.clone());
        tokio::spawn(async move { d.push_folder(&src, "/tmp/marathon/shared/tests.xctest", &context).await })
    };
    bridge.started.notified().await;
    let keep_alive = "Did not receive any keep-alive response for 60 seconds";
    h.worker
        .lose(marathon_test_support::apple::fake_simulator_worker::connection_exception(keep_alive));
    let error = tokio::time::timeout(std::time::Duration::from_secs(600), send)
        .await
        .expect("the lost connection did not end the send")?
        .unwrap_err();
    // Not a cancellation: core's D53 counts the simulator as tried and borrows another.
    let VendorError::Other(failure) = error else {
        panic!("{error:?}");
    };
    assert_eq!(failure.class.to_string_lossy(), CONNECTION_EXCEPTION);
    assert_eq!(
        failure.message.map(|m| m.to_string_lossy()),
        Some("Lost the connection to local during a transfer".into())
    );
    assert_eq!(
        failure.cause.and_then(|c| c.message).map(|m| m.to_string_lossy()),
        Some(keep_alive.into())
    );
    // A receive on the lost connection is the `false` its listener deals with.
    let back = h.dir.path().join("back");
    assert_eq!(
        d.pull_file("/tmp/marathon/log", &back, &h.services.context).await,
        Ok(false)
    );
    h.worker.connected.store(true, Ordering::SeqCst);
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
/// D54 (A104): the limit on remote parsing ends a send to a worker that has stopped answering, at the limit.
#[tokio::test(start_paused = true)]
async fn the_parses_limit_ends_a_hung_send_at_the_limit_d54() -> TestResult {
    use marathon_apple::apple_device::AppleDevice;
    use marathon_core::runtime::VendorClock;
    let h = Harness::new(|_| {})?;
    let (d, _bridge) = on_hanging_transfers(&h).await?;
    let src = h.dir.path().join("tests.xctest");
    let context = h.services.context.clone();
    let started = tokio::time::Instant::now();
    let parse = h.clock.with_timeout_or_null(
        context.cancellation.clone(),
        180_000,
        Box::new(|scope| {
            let context = VendorContext {
                cancellation: scope,
                ..context.clone()
            };
            let (d, src) = (d.clone(), src.clone());
            Box::pin(async move {
                d.push_shared(&src, "/tmp/marathon/shared/tests.xctest", &context)
                    .await?;
                Ok(())
            })
        }),
    );
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(3_600), parse)
        .await
        .expect("the limit did not end the send");
    assert_eq!(outcome, Ok(None));
    assert_eq!(started.elapsed(), std::time::Duration::from_secs(180));
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
/// Whether each transfer's stop had already come when it was called.
#[derive(Default)]
struct StopsAtCall(std::sync::Mutex<Vec<bool>>);
impl marathon_apple::cmd::FileBridge for StopsAtCall {
    fn send<'a>(&'a self, _: &'a std::path::Path, _: &'a str) -> marathon_core::runtime::VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    fn receive<'a>(&'a self, _: &'a str, _: &'a std::path::Path) -> marathon_core::runtime::VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    fn send_with<'a>(
        &'a self,
        _: &'a std::path::Path,
        _: &'a str,
        mut stop: marathon_apple::cmd::TransferStop,
    ) -> marathon_core::runtime::VendorFuture<'a, bool> {
        self.0.lock().unwrap().push(stop.now().is_some());
        Box::pin(async { Ok(true) })
    }
    fn receive_with<'a>(
        &'a self,
        _: &'a str,
        _: &'a std::path::Path,
        mut stop: marathon_apple::cmd::TransferStop,
    ) -> marathon_core::runtime::VendorFuture<'a, bool> {
        self.0.lock().unwrap().push(stop.now().is_some());
        Box::pin(async { Ok(true) })
    }
}
/// D54: a receive never takes its caller's cancellation, only the loss of its connection. Every receive collects an artifact
/// that its caller is cancelled around on purpose: a screenshot's capture at the end of every test, the artifact queue 300 s
/// after the run. A send does take it.
#[tokio::test(start_paused = true)]
async fn a_receive_never_takes_its_callers_cancellation_but_a_send_does_d54() -> TestResult {
    use marathon_apple::apple_device::AppleDevice;
    let h = Harness::new(|_| {})?;
    let bridge = Arc::new(StopsAtCall::default());
    let d = h
        .device_on_bridge(
            h.worker.clone(),
            "a",
            Arc::new(
                marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext::new(
                    marathon_config::devices::Transport::Local,
                    h.clock.clone(),
                ),
            ),
            bridge.clone(),
        )
        .await?;
    *h.worker.before.lock().unwrap() = Arc::new(|_| {
        Box::pin(async {
            Ok(Some(marathon_test_support::apple::fake_simulator_worker::ok(Vec::<
                String,
            >::new(
            ))))
        })
    });
    let signal = marathon_core::executor::shared::Signal::new();
    signal.cancel(cancelled("Recording finished"));
    let cancelled = VendorContext {
        cancellation: signal,
        ..h.services.context.clone()
    };
    let frame = h.dir.path().join("frame.png");
    let timeout = jcompat::jtime::Duration::of_seconds(10);
    assert_eq!(d.get_screenshot(timeout, &frame, &cancelled).await, Ok(true));
    assert_eq!(d.pull_file("/tmp/marathon/log", &frame, &cancelled).await, Ok(true));
    assert_eq!(
        d.pull_folder("/tmp/marathon/a.xcresult", h.dir.path(), &cancelled)
            .await,
        Ok(true)
    );
    let src = h.dir.path().join("tests.xctest");
    assert_eq!(
        d.push_folder(&src, "/tmp/marathon/tests.xctest", &cancelled).await,
        Ok(true)
    );
    assert_eq!(*bridge.0.lock().unwrap(), [false, false, false, true]);
    *h.worker.before.lock().unwrap() = Arc::new(|_| Box::pin(async { Ok(None) }));
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
