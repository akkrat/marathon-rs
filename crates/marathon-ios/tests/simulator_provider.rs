//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProviderTest.kt
//! and StartupPhasesTest.kt, AppleSimulatorProviderArtifactTest.kt, WorkerProvisioningContextTest.kt, AppleDeviceProviderTest.kt.
mod provider_support;
mod simulator_support;
mod support;
use marathon_apple::apple_device::AppleDevice;
use marathon_config::devices::{AppleTarget, Transport};
use marathon_core::{
    executor::shared::SharedDeferred,
    runtime::VendorClock,
    vendor::{Device, DeviceProvider},
};
use marathon_ios::device::{
    simulator_factory::SimulatorCreating,
    simulator_provisioner::{ProvisioningConnections, SimulatorProvisioner},
    worker_provisioning_context::WorkerProvisioningContext,
};
use marathon_test_support::apple::fake_simulator_worker::{ENV, LIST, SYSTEM_PROFILER, UNAME};
use provider_support::*;
use simulator_support::TestResult;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use support::{profile, worker};

#[tokio::test(start_paused = true)]
async fn routine_termination_delivers_a_slow_pull_before_bounding_provider_joins_d95() -> TestResult {
    use std::time::Duration;
    let p = ProviderHarness::new(|v| {
        v.lifecycle_configuration.on_dispose = vec![marathon_config::vendor::LifecycleAction::Shutdown];
    })?;
    p.h.services.logging.registry.lock().unwrap().set_level(
        "marathon_ios::device::simulator_provider",
        marathon_logging::Level::WARN,
    );
    let bridge = Arc::new(PullsUntilStopped {
        finish_after: Some(Duration::from_secs(200)),
        ..Default::default()
    });
    *p.connections.bridge.lock().unwrap() = Some(bridge.clone());
    p.update(&["a"]).await.map_err(test_error)?;
    let device = p.connected(1).await.remove(0);
    let delivered = SharedDeferred::new();
    let undelivered = Arc::new(AtomicUsize::new(0));
    let output = tempfile::tempdir()?;
    p.h.services
        .artifacts
        .submit(
            "a@local".into(),
            "slow xcresult".into(),
            "local".into(),
            Some(Arc::new({
                let undelivered = undelivered.clone();
                move |_| {
                    undelivered.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            })),
            Arc::new({
                let (device, delivered) = (device.clone(), delivered.clone());
                let output = output.path().to_path_buf();
                move |context| {
                    let (device, delivered) = (device.clone(), delivered.clone());
                    let output = output.clone();
                    Box::pin(async move {
                        if !device.pull_folder("/artifact.xcresult", &output, &context).await? {
                            return Err(support::error("The xcresult pull failed"));
                        }
                        delivered.complete(Ok(tokio::time::Instant::now()));
                        Ok(())
                    })
                }
            }),
            p.h.services.context.clone(),
        )
        .await
        .map_err(test_error)?;
    bridge.started.wait().await.map_err(test_error)?;
    p.h.boot.state.store(3, Ordering::SeqCst);
    let entered = SharedDeferred::new();
    let b = p.worker(worker("second"), &["b"]);
    *b.before.lock().unwrap() = Arc::new({
        let entered = entered.clone();
        move |_| {
            let entered = entered.clone();
            Box::pin(async move {
                entered.complete(Ok(()));
                std::future::pending().await
            })
        }
    });
    let update = tokio::spawn({
        let (provider, context) = (p.provider.clone(), p.h.services.context.clone());
        async move {
            provider
                .process_update(
                    snapshot(vec![
                        (Transport::Local, targets(&["a"])),
                        (worker("second"), targets(&["b"])),
                    ]),
                    context,
                )
                .await
        }
    });
    entered.wait().await.map_err(test_error)?;
    let start = tokio::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(400), p.provider.terminate())
        .await?
        .map_err(test_error)?;
    let delivery = delivered
        .completed()
        .expect("the slow pull was not delivered")
        .map_err(test_error)?;
    assert_eq!(*delivery - start, Duration::from_secs(200));
    assert_eq!(start.elapsed(), Duration::from_secs(270));
    assert_eq!(undelivered.load(Ordering::SeqCst), 0);
    assert!(device.quarantined());
    assert_eq!(count(&p.h.worker, &["xcrun", "simctl", "shutdown", "a"]), 0);
    assert!(update.await?.is_err());
    assert_eq!(
        p.h.logs
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, level, line, _)| {
                *level == marathon_logging::Level::WARN && line.contains("iOS provider shutdown timed out")
            })
            .count(),
        1
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn retained_pulls_get_a_stop_deadline_only_after_the_run_stops_d95() -> TestResult {
    use std::time::Duration;
    let p = ProviderHarness::with_device_window(|_| {}, 70_000)?;
    let bridge = Arc::new(PullsUntilStopped::default());
    *p.connections.bridge.lock().unwrap() = Some(bridge.clone());
    p.update(&["a"]).await.map_err(test_error)?;
    let device = p.connected(1).await.remove(0);
    let pull = tokio::spawn({
        let (device, context) = (device.clone(), p.h.services.context.clone());
        async move {
            device
                .pull_folder("/artifact.xcresult", &std::env::temp_dir(), &context)
                .await
        }
    });
    bridge.started.wait().await.map_err(test_error)?;
    tokio::time::advance(Duration::from_secs(90)).await;
    assert!(bridge.ended_at.lock().unwrap().is_none());
    let start = tokio::time::Instant::now();
    Device::seal_for_stop(device.as_ref());
    let result = tokio::time::timeout(Duration::from_secs(90), pull).await??;
    assert!(matches!(result, Err(marathon_core::error::VendorError::Cancelled(_))));
    assert_eq!(
        bridge.ended_at.lock().unwrap().unwrap() - start,
        Duration::from_secs(30)
    );
    p.finish().await
}

#[tokio::test(start_paused = true)]
async fn replaced_simulator_pulls_get_a_fresh_window_only_on_interruption_d95() -> TestResult {
    use std::time::Duration;
    for (interrupted, duration, expected) in [(false, 95, 5), (true, 150, 30)] {
        let p = ProviderHarness::new(|_| {})?;
        let bridge = Arc::new(PullsUntilStopped {
            finish_after: Some(Duration::from_secs(duration)),
            ..Default::default()
        });
        *p.connections.bridge.lock().unwrap() = Some(bridge.clone());
        p.update(&["a"]).await?;
        let predecessor = p.connected(1).await.remove(0);
        let pull = SharedDeferred::new();
        let output = tempfile::tempdir()?;
        p.h.services
            .artifacts
            .submit(
                "a@local".into(),
                "xcresult from the predecessor".into(),
                "local".into(),
                None,
                Arc::new({
                    let (predecessor, pull) = (predecessor.clone(), pull.clone());
                    let output = output.path().to_path_buf();
                    move |context| {
                        let (predecessor, pull) = (predecessor.clone(), pull.clone());
                        let output = output.clone();
                        Box::pin(async move {
                            let result = predecessor.pull_folder("/artifact.xcresult", &output, &context).await;
                            pull.complete(Ok(result.clone()));
                            result.map(|_| ())
                        })
                    }
                }),
                p.h.services.context.clone(),
            )
            .await
            .map_err(test_error)?;
        bridge.started.wait().await.map_err(test_error)?;
        p.connections
            .connect(Transport::Local, p.h.services.context.clone())
            .await
            .map_err(test_error)?;
        let replacement =
            p.h.factory()
                .create(
                    Transport::Local,
                    p.h.worker.clone(),
                    Arc::new(support::BridgeMock),
                    "a".into(),
                    Arc::new(WorkerProvisioningContext::new(Transport::Local, p.h.clock.clone())),
                    p.h.services.context.clone(),
                )
                .await?;
        p.provider.register("a@local".into(), replacement).await?;
        assert!(predecessor.disposed() && predecessor.quarantined());
        tokio::time::advance(Duration::from_secs(90)).await;
        assert!(bridge.ended_at.lock().unwrap().is_none());

        let started = tokio::time::Instant::now();
        let termination = if interrupted {
            p.provider.terminate_after_stop()
        } else {
            p.provider.terminate()
        };
        tokio::time::timeout(Duration::from_secs(160), termination)
            .await?
            .map_err(test_error)?;
        eventually(|| bridge.ended_at.lock().unwrap().is_some()).await;
        assert_eq!(
            bridge.ended_at.lock().unwrap().unwrap() - started,
            Duration::from_secs(expected)
        );
        if !interrupted {
            assert!(matches!(pull.completed().as_ref(), Some(Ok(result)) if matches!(**result, Ok(true))));
        }
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_worker_sets_up_as_many_simulators_at_a_time_as_provisioning_concurrency_allows() -> TestResult {
    for limit in [1, 2, 3, 6] {
        let p = ProviderHarness::new(|v| v.threading_configuration.provisioning_concurrency = limit)?;
        p.h.worker.add(["c", "d", "e", "f"]);
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let a = active.clone();
        let pk = peak.clone();
        let boot = p.h.boot.clone();
        *p.h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
            let a = a.clone();
            let pk = pk.clone();
            let result = boot.answer(&cmd);
            Box::pin(async move {
                if cmd.starts_with(&["xcrun".into(), "simctl".into(), "getenv".into()])
                    && cmd.last().is_some_and(|s| s == "HOME")
                {
                    pk.fetch_max(a.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    a.fetch_sub(1, Ordering::SeqCst);
                }
                Ok(result)
            })
        });
        p.update(&["a", "b", "c", "d", "e", "f"]).await.map_err(test_error)?;
        assert_eq!(p.connected(6).await.len(), 6);
        assert_eq!(peak.load(Ordering::SeqCst), limit as usize);
        p.finish().await.map_err(test_error)?;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn provisioning_and_preparation_log_their_timings_without_debug() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    let w = p.worker(worker("worker-1"), &["a", "b"]);
    p.provider
        .process_update(
            snapshot(vec![(worker("worker-1"), targets(&["a", "b"]))]),
            p.h.services.context.clone(),
        )
        .await
        .map_err(test_error)?;
    let devices = p.connected(2).await;
    p.h.prepare(devices.iter().find(|d| d.serial().starts_with("a@")).unwrap())
        .await
        .map_err(test_error)?;
    let logs = p.h.logs.lock().unwrap().clone();
    let lines: Vec<_> = logs
        .iter()
        .filter(|(logger, _, _, _)| logger == "marathon_core::timing")
        .map(|(_, _, s, _)| s.as_str())
        .collect();
    assert!(lines.iter().any(|s| s.starts_with("Worker worker-1:22: connected in ")
        && s.contains("planned 2 simulators in ")
        && s.contains(" (+")));
    for id in ["a", "b"] {
        assert!(
            lines
                .iter()
                .any(|s| s.starts_with(&format!("Simulator {id}@worker-1:22 set up in ")))
        );
    }
    assert!(
        lines
            .iter()
            .any(|s| s.starts_with("Simulator a@worker-1:22 prepared in ")
                && s.contains("cleanup ")
                && s.contains("installation ")
                && s.contains("boot "))
    );
    assert_eq!(
        w.commands
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.first().is_some_and(|s| s == "sh") && c.last().is_some_and(|s| s.starts_with("umask 077")))
            .count(),
        1
    );
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn provider_drains_artifacts_before_device_disposal_and_transport_release() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    p.update(&["a"]).await.map_err(test_error)?;
    let device = p.connected(1).await.remove(0);
    let release = SharedDeferred::new();
    let gate = release.clone();
    let started = SharedDeferred::new();
    let entered = started.clone();
    p.h.services
        .artifacts
        .submit(
            "a@local".into(),
            "bundle".into(),
            "local".into(),
            None,
            Arc::new(move |_| {
                let gate = gate.clone();
                let entered = entered.clone();
                Box::pin(async move {
                    entered.complete(Ok(()));
                    gate.wait().await?;
                    Ok(())
                })
            }),
            p.h.services.context.clone(),
        )
        .await
        .map_err(test_error)?;
    started.wait().await.map_err(test_error)?;
    let close = p.start_termination().await;
    assert!(!device.disposed());
    assert_eq!(p.connections.released.load(Ordering::SeqCst), 1);
    assert!(!close.is_finished());
    release.complete(Ok(()));
    close.await.map_err(test_error)?.map_err(test_error)?;
    assert!(device.disposed());
    assert!(p.provider.subscribe().state().closed_for_send);
    p.finish().await
}
/// x110: with debug off, the run's end says how long the artifact wait and the simulators' release took.
#[tokio::test(start_paused = true)]
async fn run_end_logs_artifact_finalization_and_device_release_timings_without_debug() -> TestResult {
    use std::time::Duration;
    let p = ProviderHarness::with_device_window(
        |v| {
            v.lifecycle_configuration.on_dispose = vec![marathon_config::vendor::LifecycleAction::Shutdown];
        },
        70_000,
    )?;
    // Once the run ends, the first simulator shut down takes 2 s for it, and is the slowest to dispose of. The fake's boot
    // state is shared, so the other one finds its simulator shut down by then, or shuts it down at once.
    let ending = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let slow = Arc::new(std::sync::Mutex::new(None::<String>));
    let boot = p.h.boot.clone();
    *p.h.worker.before.lock().unwrap() = Arc::new({
        let (ending, slow) = (ending.clone(), slow.clone());
        move |command| {
            let answer = boot.answer(&command);
            let first = ending.load(Ordering::SeqCst)
                && command.len() == 4
                && command[..3]
                    .iter()
                    .map(String::as_str)
                    .eq(["xcrun", "simctl", "shutdown"])
                && {
                    let mut slow = slow.lock().unwrap();
                    slow.is_none() && slow.replace(command[3].clone()).is_none()
                };
            Box::pin(async move {
                if first {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Ok(answer)
            })
        }
    });
    p.update(&["a", "b"]).await.map_err(test_error)?;
    p.connected(2).await;
    let release = SharedDeferred::new();
    let gate = release.clone();
    let started = SharedDeferred::new();
    let entered = started.clone();
    p.h.services
        .artifacts
        .submit(
            "a@local".into(),
            "bundle".into(),
            "local".into(),
            None,
            Arc::new(move |_| {
                let gate = gate.clone();
                let entered = entered.clone();
                Box::pin(async move {
                    entered.complete(Ok(()));
                    gate.wait().await?;
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    Ok(())
                })
            }),
            p.h.services.context.clone(),
        )
        .await
        .map_err(test_error)?;
    started.wait().await.map_err(test_error)?;
    ending.store(true, Ordering::SeqCst);
    p.h.boot.state.store(3, Ordering::SeqCst);
    let close = p.start_termination().await;
    release.complete(Ok(()));
    close.await.map_err(test_error)?.map_err(test_error)?;
    let lines: Vec<_> =
        p.h.logs
            .lock()
            .unwrap()
            .iter()
            .filter(|(logger, _, line, _)| logger == "marathon_core::timing" && line.contains(" at run end "))
            .map(|(_, level, line, _)| (*level, line.clone()))
            .collect();
    // The harness's registry is Apple's with debug off, and its sink only takes enabled records.
    assert_eq!(
        lines,
        [
            (
                marathon_logging::Level::INFO,
                "Artifact finalization at run end took 1.5 s (1 item queued or running)".to_owned()
            ),
            (
                marathon_logging::Level::INFO,
                format!(
                    "Device release at run end took 2.0 s (iOS, 2 devices; slowest {}, 2.0 s)",
                    slow.lock().unwrap().clone().expect("no simulator was shut down")
                )
            ),
        ]
    );
    assert!(
        marathon_logging::filter(marathon_logging::Vendor::Apple { compact_output: false }, false)
            .would_enable("marathon_core::timing", &marathon_logging::Level::INFO)
    );
    p.finish().await
}
/// A pull that owns a task independently of its caller, like the bridge's
/// rsync transfers. It records when it ended, including after a stop.
#[derive(Default)]
struct PullsUntilStopped {
    started: Arc<SharedDeferred<()>>,
    ended_at: Arc<std::sync::Mutex<Option<tokio::time::Instant>>>,
    finish_after: Option<std::time::Duration>,
}
impl marathon_apple::cmd::FileBridge for PullsUntilStopped {
    fn send<'a>(&'a self, _: &'a std::path::Path, _: &'a str) -> marathon_core::runtime::VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    fn receive<'a>(&'a self, _: &'a str, _: &'a std::path::Path) -> marathon_core::runtime::VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    fn receive_with<'a>(
        &'a self,
        _: &'a str,
        _: &'a std::path::Path,
        mut stop: marathon_apple::cmd::TransferStop,
    ) -> marathon_core::runtime::VendorFuture<'a, bool> {
        let (started, ended_at) = (self.started.clone(), self.ended_at.clone());
        let finish_after = self.finish_after;
        let transfer = tokio::spawn(async move {
            started.complete(Ok(()));
            let finish = async {
                match finish_after {
                    Some(duration) => tokio::time::sleep(duration).await,
                    None => std::future::pending().await,
                }
                Ok(true)
            };
            let result = tokio::select! {
                biased;
                stopped = stop.stopped() => Err(stopped.into_error()),
                result = finish => result,
            };
            *ended_at.lock().unwrap() = Some(tokio::time::Instant::now());
            result
        });
        Box::pin(async move { transfer.await.unwrap_or(Ok(false)) })
    }
}

#[tokio::test(start_paused = true)]
async fn signal_tightens_an_already_running_artifact_drain_and_leaves_time_to_dispose_d97() -> TestResult {
    use marathon_core::signal_finish::SignalFinish;
    let p = ProviderHarness::with_device_window(
        |v| {
            v.lifecycle_configuration.on_dispose = vec![marathon_config::vendor::LifecycleAction::Shutdown];
        },
        70_000,
    )?;
    let bridge = Arc::new(PullsUntilStopped::default());
    *p.connections.bridge.lock().unwrap() = Some(bridge.clone());
    p.update(&["a"]).await.map_err(test_error)?;
    let device = p.connected(1).await.remove(0);
    let context = p.h.services.context.clone();
    let release = SharedDeferred::new();
    let returned = SharedDeferred::new();
    let callbacks = Arc::new(AtomicUsize::new(0));
    let pull = device.clone();
    let (gate, done, calls) = (release.clone(), returned.clone(), callbacks.clone());
    p.h.services
        .artifacts
        .submit(
            "a@local".into(),
            "held bundle".into(),
            "local".into(),
            Some(Arc::new(move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
            Arc::new(move |_| {
                let (device, gate, done, context) = (pull.clone(), gate.clone(), done.clone(), context.clone());
                Box::pin(async move {
                    let result = device
                        .pull_folder("/artifact.xcresult", &std::env::temp_dir(), &context)
                        .await;
                    gate.wait().await?;
                    done.complete(Ok(()));
                    result.map(|_| ())
                })
            }),
            p.h.services.context.clone(),
        )
        .await
        .map_err(test_error)?;
    bridge.started.wait().await.map_err(test_error)?;
    let closing = p.start_termination().await;
    assert_eq!(
        p.recovery.stopped.load(Ordering::SeqCst),
        1,
        "recovery stops before a routine drain"
    );
    assert_eq!(p.recovery.joins.load(Ordering::SeqCst), 0);
    assert!(!device.disposed());
    p.h.boot.state.store(3, Ordering::SeqCst);
    let at = tokio::time::Instant::now();
    let finish = SignalFinish::from_start(p.h.clock.now_millis());
    p.provider.seal_for_signal(finish);
    closing.await?.map_err(test_error)?;
    assert_eq!(
        bridge.ended_at.lock().unwrap().unwrap() - at,
        std::time::Duration::from_secs(12)
    );
    assert_eq!(at.elapsed(), std::time::Duration::from_secs(18));
    assert_eq!(callbacks.load(Ordering::SeqCst), 1);
    assert_eq!(p.recovery.joins.load(Ordering::SeqCst), 1);
    assert!(device.disposed());
    assert!(!device.quarantined());
    assert_eq!(count(&p.h.worker, &["xcrun", "simctl", "shutdown", "a"]), 1);
    release.complete(Ok(()));
    returned.wait().await.map_err(test_error)?;
    p.finish().await
}

#[tokio::test(start_paused = true)]
async fn signal_with_no_artifacts_disposes_and_releases_without_waiting_for_a_cutoff_d97() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    p.update(&["a"]).await.map_err(test_error)?;
    let device = p.connected(1).await.remove(0);
    let start = p.h.clock.now_millis();
    p.provider
        .seal_for_signal(marathon_core::signal_finish::SignalFinish::from_start(start));
    p.finish().await?;
    assert_eq!(p.h.clock.now_millis(), start);
    assert!(device.disposed());
    Ok(())
}

/// An artifact job's pull still runs when the routine 360 s artifact window ends. The simulator's disposal finds it
/// among its pulls: no on-dispose lifecycle runs beside it, the simulator's lease is quarantined, and the pull ends within
/// D91's 40 s bound for a stopped transfer. Before, pulls were not the device's to know of: the lifecycle (here `simctl
/// shutdown`) ran and the lease was released while the pull went on.
#[tokio::test(start_paused = true)]
async fn a_pull_left_running_by_the_artifact_drain_keeps_its_simulator_from_the_lifecycle_d93() -> TestResult {
    let p = ProviderHarness::new(|v| {
        v.lifecycle_configuration.on_dispose = vec![marathon_config::vendor::LifecycleAction::Shutdown];
    })?;
    let bridge = Arc::new(PullsUntilStopped::default());
    *p.connections.bridge.lock().unwrap() = Some(bridge.clone());
    p.update(&["a"]).await.map_err(test_error)?;
    let device = p.connected(1).await.remove(0);
    let pulling = device.clone();
    let context = p.h.services.context.clone();
    p.h.services
        .artifacts
        .submit(
            "a@local".into(),
            "bundle".into(),
            "local".into(),
            None,
            Arc::new(move |_| {
                let (device, context) = (pulling.clone(), context.clone());
                Box::pin(async move {
                    let target = std::env::temp_dir();
                    device
                        .pull_folder("/tmp/marathon/a.xcresult", &target, &context)
                        .await?;
                    Ok(())
                })
            }),
            p.h.services.context.clone(),
        )
        .await
        .map_err(test_error)?;
    bridge.started.wait().await.map_err(test_error)?;
    // Booting, so that the on-dispose lifecycle, if it runs, shuts it down.
    p.h.boot.state.store(3, Ordering::SeqCst);
    let started = tokio::time::Instant::now();
    p.provider.terminate().await.map_err(test_error)?;
    let terminated = tokio::time::Instant::now();
    assert!(device.disposed());
    let shutdowns = count(&p.h.worker, &["xcrun", "simctl", "shutdown", "a"]);
    assert_eq!(shutdowns, 0, "the on-dispose lifecycle ran beside the pull");
    assert_eq!(
        p.connections.quarantined.load(Ordering::SeqCst),
        1,
        "the lease was not quarantined"
    );
    assert!(terminated - started >= std::time::Duration::from_secs(360));
    eventually(|| bridge.ended_at.lock().unwrap().is_some()).await;
    let ended = bridge.ended_at.lock().unwrap().unwrap();
    assert!(
        ended - started <= std::time::Duration::from_secs(360 + 40),
        "{:?}",
        ended - started
    );
    assert_eq!(
        p.connections.acquired.load(Ordering::SeqCst),
        p.connections.released.load(Ordering::SeqCst) + p.connections.quarantined.load(Ordering::SeqCst)
    );
    Ok(())
}
/// A pull that starts unless its stop refuses it at once (its device's pull admission), counting those that started.
#[derive(Default)]
struct CountsStartedPulls {
    started: Arc<AtomicUsize>,
}
impl marathon_apple::cmd::FileBridge for CountsStartedPulls {
    fn send<'a>(&'a self, _: &'a std::path::Path, _: &'a str) -> marathon_core::runtime::VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    fn receive<'a>(&'a self, _: &'a str, _: &'a std::path::Path) -> marathon_core::runtime::VendorFuture<'a, bool> {
        self.started.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(true) })
    }
    fn receive_with<'a>(
        &'a self,
        src: &'a str,
        dst: &'a std::path::Path,
        mut stop: marathon_apple::cmd::TransferStop,
    ) -> marathon_core::runtime::VendorFuture<'a, bool> {
        match stop.now() {
            Some(stopped) => Box::pin(async move { Err(stopped.into_error()) }),
            None => self.receive(src, dst),
        }
    }
}

/// D93 (round 10, finding 2): an artifact job stalls before it asks for its pull (as one does creating its output folder
/// on a filesystem that no longer answers), past the provider's artifact drain at the run's end. The simulator's
/// disposal takes its look at the simulator's pulls (none) and runs its on-dispose lifecycle; the job resumes during that
/// lifecycle (`simctl shutdown`) and asks for its pull. The look closed the simulator's pulls, whatever the seal's cause:
/// the pull is refused as it starts, an undelivered artifact with a WARN, and nothing is pulled beside the lifecycle or
/// the release of the lease. Before, the termination's seal left pulls open and the pull started then.
#[tokio::test(start_paused = true)]
async fn a_pull_asked_for_after_the_disposals_look_at_the_pulls_is_refused_d93() -> TestResult {
    let p = ProviderHarness::new(|v| {
        v.lifecycle_configuration.on_dispose = vec![marathon_config::vendor::LifecycleAction::Shutdown];
    })?;
    p.h.services
        .logging
        .registry
        .lock()
        .unwrap()
        .set_level("marathon_ios::apple_simulator_device", marathon_logging::Level::WARN);
    let bridge = Arc::new(CountsStartedPulls::default());
    *p.connections.bridge.lock().unwrap() = Some(bridge.clone());
    p.update(&["a"]).await.map_err(test_error)?;
    let device = p.connected(1).await.remove(0);
    let (stalled, resume, pulled) = (SharedDeferred::new(), SharedDeferred::new(), SharedDeferred::new());
    {
        let (stalled, resume, pulled) = (stalled.clone(), resume.clone(), pulled.clone());
        let (device, context) = (device.clone(), p.h.services.context.clone());
        p.h.services
            .artifacts
            .submit(
                "a@local".into(),
                "bundle".into(),
                "local".into(),
                None,
                Arc::new(move |_| {
                    let (stalled, resume, pulled) = (stalled.clone(), resume.clone(), pulled.clone());
                    let (device, context) = (device.clone(), context.clone());
                    // The stall is not the job's to cancel: it goes on after the drain has given the job up.
                    let job = tokio::spawn(async move {
                        stalled.complete(Ok(()));
                        let _ = resume.wait().await;
                        let outcome = device
                            .pull_folder("/tmp/marathon/a.xcresult", &std::env::temp_dir(), &context)
                            .await;
                        pulled.complete(Ok(outcome));
                    });
                    Box::pin(async move {
                        let _ = job.await;
                        Ok(())
                    })
                }),
                p.h.services.context.clone(),
            )
            .await
            .map_err(test_error)?;
    }
    stalled.wait().await.map_err(test_error)?;
    // The job resumes as the lifecycle shuts the simulator down, which waits for its pull's outcome.
    let boot = p.h.boot.clone();
    {
        let (resume, pulled) = (resume.clone(), pulled.clone());
        *p.h.worker.before.lock().unwrap() = Arc::new(move |c| {
            let lifecycle = c == ["xcrun", "simctl", "shutdown", "a"];
            let answer = boot.answer(&c);
            let (resume, pulled) = (resume.clone(), pulled.clone());
            Box::pin(async move {
                if lifecycle {
                    resume.complete(Ok(()));
                    let _ = pulled.wait().await;
                }
                Ok(answer)
            })
        });
    }
    p.h.boot.state.store(3, Ordering::SeqCst);
    let started = tokio::time::Instant::now();
    p.provider.terminate().await.map_err(test_error)?;
    assert_eq!(started.elapsed(), std::time::Duration::from_secs(360));
    assert_eq!(count(&p.h.worker, &["xcrun", "simctl", "shutdown", "a"]), 1);
    assert!(!device.quarantined());
    let outcome = pulled.wait().await.map_err(test_error)?;
    assert_eq!(
        bridge.started.load(Ordering::SeqCst),
        0,
        "the pull started beside the lifecycle"
    );
    assert!(matches!(*outcome, Ok(false)), "{outcome:?}");
    assert!(
        p.h.logs
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.1 == marathon_logging::Level::WARN && r.2.starts_with("Not receiving ")),
        "no WARN for the refused pull"
    );
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn a_stopped_run_disposes_simulators_within_one_artifact_window_d94() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    p.h.services
        .logging
        .registry
        .lock()
        .unwrap()
        .component_with_level("marathon_apple::artifact_processor", marathon_logging::Level::WARN);
    p.update(&["a"]).await.map_err(test_error)?;
    let device = p.connected(1).await.remove(0);
    let started = SharedDeferred::new();
    let cancel_at = SharedDeferred::new();
    let release = SharedDeferred::new();
    let returned = SharedDeferred::new();
    let (entered, cancelled, gate, done) = (started.clone(), cancel_at.clone(), release.clone(), returned.clone());
    p.h.services
        .artifacts
        .submit(
            "a@local".into(),
            "stalled bundle".into(),
            "local".into(),
            None,
            Arc::new(move |context| {
                let (entered, cancelled, gate, done) = (entered.clone(), cancelled.clone(), gate.clone(), done.clone());
                Box::pin(async move {
                    entered.complete(Ok(()));
                    let _ = context.cancellation.cancelled().await;
                    cancelled.complete(Ok(context.clock.now_millis()));
                    gate.wait().await?;
                    done.complete(Ok(()));
                    Ok(())
                })
            }),
            p.h.services.context.clone(),
        )
        .await
        .map_err(test_error)?;
    started.wait().await.map_err(test_error)?;
    let start = p.h.clock.now_millis();
    p.provider.terminate_after_stop().await.map_err(test_error)?;
    assert_eq!(*cancel_at.wait().await.map_err(test_error)? - start, 30_000);
    assert_eq!(p.h.clock.now_millis() - start, 70_000);
    assert!(device.disposed());
    assert!(p.h.logs.lock().unwrap().iter().any(|(_, _, line, _)| {
        line.contains("Artifact work did not stop within 40000 ms of cancellation") && line.contains("stalled bundle")
    }));
    assert!(p.h.logs.lock().unwrap().iter().any(|(_, _, line, _)| {
        line.contains("their remote sources are kept on the workers") && line.contains("stalled bundle")
    }));
    release.complete(Ok(()));
    returned.wait().await.map_err(test_error)?;
    p.finish().await
}

#[tokio::test(start_paused = true)]
async fn a_worker_planning_failure_releases_its_connection_and_does_not_cancel_healthy_workers() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    let failed = p.worker(worker("failed"), &["bad"]);
    let healthy = p.worker(worker("healthy"), &["a", "b"]);
    *failed.before.lock().unwrap() = Arc::new(|_| Box::pin(async { Err(support::error("simctl list failed")) }));
    p.provider
        .process_update(
            snapshot(vec![
                (worker("failed"), targets(&["bad"])),
                (worker("healthy"), targets(&["a", "b"])),
            ]),
            p.h.services.context.clone(),
        )
        .await
        .map_err(test_error)?;
    assert_eq!(p.connected(2).await.len(), 2);
    assert_eq!(healthy.lists(), 1);
    assert_eq!(p.connections.released.load(Ordering::SeqCst), 2);
    assert!(
        p.h.logs
            .lock()
            .unwrap()
            .iter()
            .any(|(_, _, s, e)| s.contains("Failed to initialize worker") && e.is_some())
    );
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn initialize_logs_the_effective_xcresulttool_timeout() -> TestResult {
    let p = ProviderHarness::new(|v| {
        v.timeout_configuration.shell = support::duration(300_000);
        v.timeout_configuration.xcresulttool = support::duration(300_000);
    })?;
    p.h.services.logging.registry.lock().unwrap().component_with_level(
        "marathon_ios::device::simulator_provider",
        marathon_logging::Level::INFO,
    );
    let path = p.h.dir.path().join("Marathondevices");
    std::fs::write(path, "workers: []\n")?;
    assert!(p.provider.initialize(p.h.services.context.clone()).await.is_err());
    assert!(p.h.logs.lock().unwrap().iter().any(|(name, level, s, _)| name
        == "marathon_ios::device::simulator_provider"
        && *level == marathon_logging::Level::INFO
        && *s
            == "xcresulttool timeout is 300 s (timeoutConfiguration.xcresulttool; when not set, the larger of shell 300 s and 120 s)"));
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn four_unchanged_simulators_share_one_inventory_and_one_set_of_worker_facts() -> TestResult {
    for transport in [Transport::Local, worker("worker-1")] {
        let p = ProviderHarness::new(|_| {})?;
        let w = if transport == Transport::Local {
            p.h.worker.add(["c", "d"]);
            p.h.worker.clone()
        } else {
            p.worker(transport.clone(), &["a", "b", "c", "d"])
        };
        p.provider
            .process_update(
                snapshot(vec![(transport.clone(), targets(&["a", "b", "c", "d"]))]),
                p.h.services.context.clone(),
            )
            .await
            .map_err(test_error)?;
        let devices = p.connected(4).await;
        assert_eq!(w.lists(), 1);
        for c in [ENV, UNAME, &["xcrun", "xcodebuild", "-version"][..]] {
            assert_eq!(w.count(c), 1);
        }
        assert_eq!(
            w.commands
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.first().is_some_and(|s| s == SYSTEM_PROFILER))
                .count(),
            1
        );
        for d in devices {
            assert_eq!(home(&w, d.udid()), 1);
            assert_eq!(d.abi(), "arm64".to_owned());
            assert_eq!(d.serial(), format!("{}@{}", d.udid(), transport.id().to_owned()));
        }
        p.finish().await.map_err(test_error)?;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn harness_command_counts_for_existing_simulators_and_new_profiles_of_a_worker() -> TestResult {
    for n in [1, 4, 8, 16] {
        let p = ProviderHarness::new(|_| {})?;
        let ids: Vec<_> = (0..n).map(|n| format!("sim-{n}")).collect();
        let refs: Vec<_> = ids.iter().map(String::as_str).collect();
        let existing = p.worker(worker("existing"), &refs);
        let profiles = p.worker(worker("profiles"), &[]);
        p.provider
            .process_update(
                snapshot(vec![
                    (worker("existing"), targets(&refs)),
                    (worker("profiles"), vec![AppleTarget::SimulatorProfile(profile()); n]),
                ]),
                p.h.services.context.clone(),
            )
            .await
            .map_err(test_error)?;
        assert_eq!(p.connected(2 * n).await.len(), 2 * n);
        assert_eq!(existing.lists(), 1);
        assert_eq!(profiles.lists(), 1 + n);
        assert_eq!(existing.worker_fact_commands(), 4);
        assert_eq!(profiles.worker_fact_commands(), 4);
        assert_eq!(profiles.simulators.lock().unwrap().len(), n);
        p.finish().await.map_err(test_error)?;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn new_profiles_are_planned_from_the_shared_inventory_and_set_up_from_a_lookup_after_creation() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    let w = p.worker(worker("worker-1"), &["existing"]);
    let mut list = targets(&["existing"]);
    list.extend(vec![AppleTarget::SimulatorProfile(profile()); 3]);
    let mut unknown = profile();
    unknown.device_type_id = "iPhone 99".into();
    list.push(AppleTarget::SimulatorProfile(unknown));
    p.provider
        .process_update(snapshot(vec![(worker("worker-1"), list)]), p.h.services.context.clone())
        .await
        .map_err(test_error)?;
    assert_eq!(p.connected(4).await.len(), 4);
    assert_eq!(p.provider.subscribe().state().queued, 0);
    assert_eq!(w.lists(), 4);
    assert_eq!(w.worker_fact_commands(), 4);
    assert_eq!(w.simulators.lock().unwrap().len(), 4);
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_deleted_after_the_inventory_was_taken_is_not_set_up() -> TestResult {
    let p = ProviderHarness::new(|v| v.threading_configuration.provisioning_concurrency = 1)?;
    p.h.worker.add(["c", "d"]);
    let sims = p.h.worker.simulators.clone();
    let boot = p.h.boot.clone();
    *p.h.worker.before.lock().unwrap() = Arc::new(move |c| {
        if c == ENV {
            sims.lock().unwrap().remove("b");
        }
        let result = boot.answer(&c);
        Box::pin(async move { Ok(result) })
    });
    p.update(&["a", "b", "c", "d"]).await.map_err(test_error)?;
    let devices = p.connected(3).await;
    assert!(!devices.iter().any(|d| d.serial().starts_with("b@")));
    assert_eq!(p.h.worker.lists(), 2);
    assert_eq!(p.provider.subscribe().state().queued, 0);
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn unavailable_simulators_are_neither_planned_nor_re_provisioned() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    p.h.worker.simulators.lock().unwrap()["b"]
        .available
        .store(false, Ordering::SeqCst);
    p.update(&["a", "b"]).await.map_err(test_error)?;
    assert_eq!(p.connected(1).await[0].udid(), "a");
    assert_eq!(p.provider.subscribe().state().queued, 0);
    let registered = Arc::new(std::sync::Mutex::new(Vec::new()));
    let reg = registered.clone();
    let provisioner = SimulatorProvisioner::new(
        p.connections.clone(),
        p.h.factory(),
        Arc::new(move |id, d| {
            reg.lock().unwrap().push((id, d));
            Box::pin(async { Ok(()) })
        }),
        2,
        p.h.services.logging.clone(),
    )?;
    provisioner
        .existing(["a".into(), "b".into()], Transport::Local, p.h.services.context.clone())
        .await
        .map_err(test_error)?;
    let devices = std::mem::take(&mut *registered.lock().unwrap());
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].0, "a@local".to_owned());
    for (_, d) in devices {
        d.device.0.dispose().await.map_err(test_error)?;
        marathon_ios::device::simulator_provisioner::ProvisioningConnections::release(
            p.connections.as_ref(),
            d.handle.executor(),
        )
        .await
        .map_err(test_error)?;
    }
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn a_slow_inventory_of_one_worker_does_not_hold_up_other_workers() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    let slow = p.worker(worker("slow"), &["a", "b"]);
    p.worker(worker("fast"), &["c", "d"]);
    let listing = SharedDeferred::new();
    let release = SharedDeferred::new();
    let entered = listing.clone();
    let gate = release.clone();
    let boot = p.h.boot.clone();
    *slow.before.lock().unwrap() = Arc::new(move |c| {
        let entered = entered.clone();
        let gate = gate.clone();
        let result = boot.answer(&c);
        Box::pin(async move {
            if c == LIST {
                entered.complete(Ok(()));
                gate.wait().await?;
            }
            Ok(result)
        })
    });
    let provider = p.provider.clone();
    let caller = p.h.services.context.clone();
    let update = tokio::spawn(async move {
        provider
            .process_update(
                snapshot(vec![
                    (worker("slow"), targets(&["a", "b"])),
                    (worker("fast"), targets(&["c", "d"])),
                ]),
                caller,
            )
            .await
    });
    listing.wait().await.map_err(test_error)?;
    let fast = p.connected(2).await;
    assert!(fast.iter().all(|d| ["c", "d"].contains(&d.udid())));
    assert!(!update.is_finished());
    release.complete(Ok(()));
    update.await.map_err(test_error)?.map_err(test_error)?;
    assert_eq!(p.connected(2).await.len(), 2);
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn should_close_the_channel() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    assert!(Arc::ptr_eq(&p.provider.subscribe(), &p.provider.subscribe()));
    p.provider.terminate().await.map_err(test_error)?;
    p.provider.terminate().await.map_err(test_error)?;
    assert!(p.provider.subscribe().receive().await.map_err(test_error)?.is_none());
    let p = ProviderHarness::new(|_| {})?;
    p.update(&["a"]).await.map_err(test_error)?;
    eventually(|| p.provider.subscribe().state().queued == 1).await;
    p.provider.terminate().await.map_err(test_error)?;
    assert!(p.provider.subscribe().receive().await.map_err(test_error)?.is_some());
    assert!(p.provider.subscribe().receive().await.map_err(test_error)?.is_none());
    p.finish().await
}
#[tokio::test]
async fn explicit_vendor_construction_shares_identity_and_performs_no_initialization() -> TestResult {
    for parser in [
        marathon_config::vendor::AppleTestParserConfiguration::Nm {
            test_class_regexes: vec![],
        },
        marathon_config::vendor::AppleTestParserConfiguration::Xctest,
    ] {
        let p = ProviderHarness::new(|v| v.test_parser_configuration = parser)?;
        let registry = Arc::new(marathon_apple::apple_device_registry::AppleDeviceRegistry::default());
        let vendor = marathon_ios::ios_vendor::IosVendor {
            simulator: p.h.services.clone(),
            binding: Arc::new(marathon_config::binding::BindingContext {
                profile: marathon_config::binding::Profile::Marathonfile,
                marathonfile: std::path::PathBuf::from("/Marathonfile"),
                marathonfile_dir: std::path::PathBuf::from("/"),
                process_dir: std::path::PathBuf::from(p.h.dir.path().to_str().unwrap()),
                environment: vec![],
                now: jcompat::jtime::instant_of(0, 0).unwrap(),
                analytics_tracking: false,
                bugsnag_reporting: false,
            }),
            connections: p.connections.clone(),
            recovery: p.recovery.clone(),
            shared_transfers: Arc::new(marathon_apple::cmd::shared_transfers::SharedTransfers::new(
                p.h.services.runtime.clone(),
                p.h.services.logging.clone(),
            )),
            registry: registry.clone(),
            parser_libraries: Default::default(),
        }
        .construct()?;
        assert_eq!(p.connections.acquired.load(Ordering::SeqCst), 0);
        assert!(p.h.worker.commands.lock().unwrap().is_empty());
        let expected: Arc<dyn marathon_core::vendor::TestBundleIdentifier> = p.h.services.identifier.clone();
        assert!(Arc::ptr_eq(vendor.bundle_identifier.as_ref().unwrap(), &expected));
        assert!(matches!(
            vendor.parser,
            Some(marathon_core::vendor::TestParser::Remote(_))
        ));
        assert!(vendor.log_configurator.is_some());
        std::fs::write(
            p.h.dir.path().join("Marathondevices"),
            "workers:\n- transport: {type: local}\n  devices: [{type: simulator, udid: a}]\n",
        )?;
        vendor
            .provider
            .initialize(p.h.services.context.clone())
            .await
            .map_err(test_error)?;
        let Some(marathon_core::vendor::DeviceEvent::Connected(identity)) =
            support::watchdog(vendor.provider.subscribe().receive())
                .await
                .map_err(test_error)?
        else {
            panic!("connect")
        };
        let apple = registry.lookup(&identity).map_err(test_error)?;
        let device: Arc<dyn Device> = apple;
        assert!(Arc::ptr_eq(&device, &identity.0));
        vendor.provider.terminate().await.map_err(test_error)?;
        assert!(registry.lookup(&identity).is_err());
        p.finish().await?;
    }
    Ok(())
}
