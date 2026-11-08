//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/SimulatorProvisionerTest.kt
mod support;
use marathon_apple::cmd::{CommandExecutor, cancelled};
use marathon_ios::device::{provisioning_plan::ProvisioningPlan, simulator_provisioner::FailedSetup};
use marathon_test_support::apple::fake_simulator_worker::ok;
use std::sync::{Arc, atomic::Ordering};
use support::*;
use tokio::sync::Semaphore;

#[tokio::test(start_paused = true)]
async fn existing_simulators_overlap_and_register_exactly_once_after_setup() -> TestResult {
    let h = Harness::new();
    let run = h.existing(&["existing-1", "existing-2", "existing-3", "existing-4"]);
    tokio::pin!(run);
    assert!(futures_util::poll!(&mut run).is_pending());
    assert_eq!(h.creating.peak.load(Ordering::SeqCst), 2);
    assert_eq!(h.registered(), 0);
    run.await?;
    assert_eq!(h.clock.now(), 200);
    assert_eq!(h.registered(), 4);
    let registrations = h.registrations.lock().unwrap();
    assert_eq!(
        registrations
            .iter()
            .map(|(id, _, _)| id.clone())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );
    for (id, device, _) in registrations.iter() {
        assert_eq!(
            id.to_owned(),
            format!(
                "{}@{}",
                device.0.info().serial_number.to_owned(),
                worker("worker-1").id().to_owned()
            )
        );
        assert!(h.creating.devices.lock().unwrap().iter().any(|d| d.device() == *device));
    }
    assert_eq!(h.connections.released.load(Ordering::SeqCst), 0);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn new_profiles_overlap_and_are_bounded() -> TestResult {
    let h = Harness::new();
    assert!(h.profiles(4).await?.is_empty());
    assert_eq!(h.creating.peak.load(Ordering::SeqCst), 2);
    assert_eq!(h.clock.now(), 200);
    assert_eq!(h.registered(), 4);
    assert_eq!(h.deletes(), 0);
    assert_eq!(h.created.load(Ordering::SeqCst), 4);
    assert_eq!(
        h.registrations
            .lock()
            .unwrap()
            .iter()
            .map(|(id, _, _)| id.clone())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn existing_simulators_and_new_profiles_share_the_worker_limit() -> TestResult {
    let h = Harness::new();
    h.provisioner
        .provision(
            ProvisioningPlan {
                existing_simulators: ["a".into(), "b".into()].into_iter().collect(),
                needs_provisioning: vec![profile(); 2],
                ..Default::default()
            },
            worker("worker-1"),
            h.bin.clone(),
            h.cycle.clone(),
            h.context.clone(),
        )
        .await?;
    assert_eq!(h.creating.peak.load(Ordering::SeqCst), 2);
    assert_eq!(h.clock.now(), 200);
    assert_eq!(h.registered(), 4);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn concurrent_provisioning_calls_on_one_worker_share_the_limit() -> TestResult {
    let h = Harness::new();
    let (a, b) = tokio::join!(h.existing(&["a", "b"]), h.existing(&["c", "d"]));
    a?;
    b?;
    assert_eq!(h.creating.peak.load(Ordering::SeqCst), 2);
    assert_eq!(h.clock.now(), 200);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn different_transport_settings_for_the_same_host_share_the_worker_limit() -> TestResult {
    let h = Harness::new();
    let mut transport = worker("worker-1");
    if let marathon_config::devices::Transport::Ssh {
        check_reachability,
        authentication,
        ..
    } = &mut transport
    {
        *check_reachability = false;
        *authentication = Some(marathon_config::SshAuthentication::Password {
            username: "u".into(),
            password: "changed".into(),
        });
    }
    let (a, b) = tokio::join!(
        h.existing(&["a", "b"]),
        h.provisioner
            .existing(["c".into(), "d".into()], transport, h.context.clone())
    );
    a?;
    b?;
    assert_eq!(h.creating.peak.load(Ordering::SeqCst), 2);
    assert_eq!(h.clock.now(), 200);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn workers_retain_independent_concurrency_budgets() -> TestResult {
    let h = Harness::new();
    let (a, b) = tokio::join!(
        h.existing(&["a", "b"]),
        h.provisioner
            .existing(["a".into(), "b".into()], worker("worker-2"), h.context.clone())
    );
    a?;
    b?;
    assert_eq!(
        h.registrations
            .lock()
            .unwrap()
            .iter()
            .map(|(id, _, _)| id.clone())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );
    assert_eq!(h.creating.peak.load(Ordering::SeqCst), 4);
    assert_eq!(h.clock.now(), 100);
    assert_eq!(h.registered(), 4);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn one_failed_setup_releases_only_its_reference_and_healthy_siblings_finish() -> TestResult {
    let h = Harness::new();
    h.creating.fail_first.store(true, Ordering::SeqCst);
    h.existing(&["bad", "healthy", "other"]).await?;
    assert_eq!(
        h.registrations
            .lock()
            .unwrap()
            .iter()
            .map(|(id, _, _)| id.to_owned())
            .collect::<std::collections::HashSet<_>>(),
        ["healthy@worker-1:22".into(), "other@worker-1:22".into()]
            .into_iter()
            .collect()
    );
    assert_eq!(h.registered(), 2);
    assert_eq!(h.connections.released.load(Ordering::SeqCst), 1);
    assert!(h.worker.connected());
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn failed_new_simulator_is_deleted_while_healthy_profiles_finish() -> TestResult {
    let h = Harness::new();
    h.creating.fail_first.store(true, Ordering::SeqCst);
    let failed = h.profiles(3).await?;
    assert_eq!(
        failed,
        vec![FailedSetup::Created {
            profile: profile(),
            unreachable: false,
            refused: false,
        }]
    );
    assert_eq!(h.registered(), 2);
    assert_eq!(h.connections.released.load(Ordering::SeqCst), 1);
    assert_eq!(h.deletes(), 1);
    // Ids follow simctl create order, while the fake fails whichever setup starts first;
    // the three profiles run concurrently, so that need not be new-1.
    let failed = h
        .trace
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| e.strip_prefix("setup:").map(str::to_owned))
        .expect("a setup started");
    assert_eq!(h.worker.count(&["xcrun", "simctl", "delete", &failed]), 1);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn new_simulator_is_deleted_if_no_connection_can_be_acquired() -> TestResult {
    let h = Harness::new();
    h.connections.unavailable.store(true, Ordering::SeqCst);
    assert_eq!(
        h.profiles(1).await?,
        vec![FailedSetup::Created {
            profile: profile(),
            unreachable: true,
            refused: false,
        }]
    );
    assert_eq!(h.registered(), 0);
    assert_eq!(h.connections.released.load(Ordering::SeqCst), 0);
    assert_eq!(h.deletes(), 1);
    assert_eq!(h.worker.count(&["xcrun", "simctl", "delete", "new-1"]), 1);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn cancellation_cleans_active_profiles_and_does_not_start_queued_work() -> TestResult {
    let h = Harness::new();
    h.creating.block.store(true, Ordering::SeqCst);
    let run = h.profiles(4);
    tokio::pin!(run);
    assert!(futures_util::poll!(&mut run).is_pending());
    // Fake commands complete through owned tasks. Drive until both active setups are at their barrier.
    while h.creating.started.load(Ordering::SeqCst) < 2 {
        tokio::task::yield_now().await;
        assert!(futures_util::poll!(&mut run).is_pending());
    }
    assert_eq!(h.created.load(Ordering::SeqCst), 2);
    h.signal.cancel(cancelled("stop"));
    assert!(run.await.unwrap_err().is_cancelled());
    assert_eq!(h.creating.active.load(Ordering::SeqCst), 0);
    assert_eq!(h.creating.started.load(Ordering::SeqCst), 2);
    assert_eq!(h.registered(), 0);
    assert_eq!(h.created.load(Ordering::SeqCst), 2);
    assert_eq!(h.deletes(), 2);
    for id in ["new-1", "new-2"] {
        assert_eq!(h.worker.count(&["xcrun", "simctl", "delete", id]), 1);
    }
    assert_eq!(h.connections.released.load(Ordering::SeqCst), 2);
    h.creating.block.store(false, Ordering::SeqCst);
    h.provisioner
        .existing(["a".into(), "b".into()], worker("worker-1"), context())
        .await?;
    assert_eq!(h.registered(), 2);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn cancellation_during_simulator_creation_retains_the_udid_for_cleanup() -> TestResult {
    let h = Harness::new();
    h.commands.passthrough.store(true, Ordering::SeqCst);
    let gate = Arc::new(Semaphore::new(0));
    let create_started = Arc::new(Semaphore::new(0));
    let g = gate.clone();
    let started = create_started.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |args| {
        let g = g.clone();
        let started = started.clone();
        Box::pin(async move {
            if args.get(2).is_some_and(|s| s == "create") {
                started.add_permits(1);
                g.acquire().await.map_err(|e| error(&e.to_string()))?.forget();
                return Ok(Some(ok(["ABCDABCD-ABCD-ABCD-ABCD-ABCDABCDABCD"])));
            }
            Ok(None)
        })
    });
    let run = h.profiles(1);
    tokio::pin!(run);
    while create_started.available_permits() == 0 {
        assert!(futures_util::poll!(&mut run).is_pending());
        tokio::task::yield_now().await;
    }
    h.signal.cancel(cancelled("during create"));
    gate.add_permits(1);
    assert!(run.await.unwrap_err().is_cancelled());
    assert_eq!(h.connections.connected.load(Ordering::SeqCst), 0);
    assert_eq!(h.registered(), 0);
    assert_eq!(h.deletes(), 1);
    assert_eq!(
        h.worker
            .count(&["xcrun", "simctl", "delete", "ABCDABCD-ABCD-ABCD-ABCD-ABCDABCDABCD"]),
        1
    );
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn setup_cancellation_propagates_instead_of_being_treated_as_a_setup_failure() -> TestResult {
    let h = Harness::new();
    h.creating.cancel_setup.store(true, Ordering::SeqCst);
    assert!(h.existing(&["a", "b"]).await.unwrap_err().is_cancelled());
    assert_eq!(h.registered(), 0);
    assert_eq!(h.connections.released.load(Ordering::SeqCst), 2);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn cancellation_after_setup_releases_the_unregistered_device() -> TestResult {
    let h = Harness::new();
    h.creating.block.store(true, Ordering::SeqCst);
    h.creating.ignore_cancellation.store(true, Ordering::SeqCst);
    let run = h.existing(&["a"]);
    tokio::pin!(run);
    assert!(futures_util::poll!(&mut run).is_pending());
    h.signal.cancel(cancelled("after setup"));
    h.creating.gate.add_permits(1);
    assert!(run.await.unwrap_err().is_cancelled());
    assert_eq!(h.registered(), 0);
    assert_eq!(h.creating.devices.lock().unwrap()[0].disposed.load(Ordering::SeqCst), 1);
    assert_eq!(h.connections.released.load(Ordering::SeqCst), 1);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn fake_cold_startup_and_reprovisioning_timing_for_one_two_and_four_simulators() -> TestResult {
    for count in [1, 2, 4] {
        let h = Harness::new();
        h.profiles(count).await?;
        assert_eq!(h.registrations.lock().unwrap()[0].2, 100);
        assert_eq!(h.clock.now(), count.div_ceil(2) as i64 * 100);
        assert_eq!(h.creating.peak.load(Ordering::SeqCst), count.min(2));
        let start = h.clock.now();
        let ids = (1..=count).map(|n| format!("reconnected-{n}")).collect::<Vec<_>>();
        h.provisioner
            .existing(ids, worker("worker-1"), h.context.clone())
            .await?;
        assert_eq!(h.registrations.lock().unwrap()[count].2, start + 100);
        assert_eq!(h.clock.now() - start, count.div_ceil(2) as i64 * 100);
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn dropped_registration_disposes_then_releases_and_deletes_without_a_failure() -> TestResult {
    let h = Harness::new();
    let (logging, _) = logging();
    let p = marathon_ios::device::simulator_provisioner::SimulatorProvisioner::new(
        h.connections.clone(),
        h.creating.clone(),
        Arc::new(|_, _| Box::pin(async { Err(marathon_ios::device::error::SetupDropped("gone".into()).into()) })),
        2,
        logging,
        h.clock.as_ref(),
    )?;
    assert!(
        p.provision(
            ProvisioningPlan {
                needs_provisioning: vec![profile()],
                ..Default::default()
            },
            worker("worker-1"),
            h.bin.clone(),
            h.cycle.clone(),
            h.context.clone()
        )
        .await?
        .is_empty()
    );
    assert_eq!(h.creating.devices.lock().unwrap()[0].disposed.load(Ordering::SeqCst), 1);
    assert_eq!(h.connections.released.load(Ordering::SeqCst), 1);
    assert_eq!(h.deletes(), 1);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn unsupported_profiles_do_not_create_or_report_a_failure() -> TestResult {
    let h = Harness::new();
    let mut p = profile();
    p.device_type_id = "missing".into();
    assert!(
        h.provisioner
            .provision(
                ProvisioningPlan {
                    needs_provisioning: vec![p],
                    ..Default::default()
                },
                worker("worker-1"),
                h.bin.clone(),
                h.cycle.clone(),
                h.context.clone()
            )
            .await?
            .is_empty()
    );
    assert_eq!(h.connections.connected.load(Ordering::SeqCst), 0);
    assert_eq!(h.deletes(), 0);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn unreachable_and_permanent_connection_errors_keep_their_source_classification() -> TestResult {
    use marathon_apple::{
        cmd::remote::ssh::sshj::{DisconnectReason, SshFailure},
        device::connection_factory::ConnectionError,
        logparser::parser::{DeviceFailureException, DeviceFailureReason},
    };
    for (err, unreachable) in [
        (
            ConnectionError::Device(DeviceFailureException::new(
                DeviceFailureReason::UnreachableHost,
                None,
                None,
            )),
            true,
        ),
        (ConnectionError::Vendor(error("permanent")), false),
        // D35: both report TransportException[UNKNOWN], but only the network fault can heal.
        (
            ConnectionError::Device(DeviceFailureException::new(
                DeviceFailureReason::UnreachableHost,
                None,
                Some(SshFailure::transport(
                    DisconnectReason::Unknown,
                    "Broken transport; encountered EOF",
                )),
            )),
            true,
        ),
        (
            ConnectionError::Device(DeviceFailureException::new(
                DeviceFailureReason::UnreachableHost,
                None,
                Some(SshFailure::no_common_algorithm("Unable to reach a settlement")),
            )),
            false,
        ),
    ] {
        let h = Harness::new();
        *h.connections.connect_error.lock().unwrap() = Some(err);
        let plan = ProvisioningPlan {
            existing_simulators: ["a".into(), "b".into()].into_iter().collect(),
            ..Default::default()
        };
        let failed = h
            .provisioner
            .provision(
                plan,
                worker("worker"),
                h.bin.clone(),
                h.cycle.clone(),
                h.context.clone(),
            )
            .await?;
        assert_eq!(
            failed,
            vec![
                FailedSetup::Existing {
                    udid: "a".into(),
                    unreachable,
                    refused: !unreachable
                },
                FailedSetup::Existing {
                    udid: "b".into(),
                    unreachable,
                    refused: !unreachable
                }
            ]
        );
        assert_eq!(h.connections.released.load(Ordering::SeqCst), 0);
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn disconnected_setup_is_checked_before_release_and_delete_failure_preserves_the_failure() -> TestResult {
    let h = Harness::new();
    h.worker.connected.store(false, Ordering::SeqCst);
    h.creating.fail_first.store(true, Ordering::SeqCst);
    let failed = h
        .provisioner
        .provision(
            ProvisioningPlan {
                existing_simulators: ["bad".into()].into_iter().collect(),
                ..Default::default()
            },
            worker("worker"),
            h.bin.clone(),
            h.cycle.clone(),
            h.context.clone(),
        )
        .await?;
    assert_eq!(
        failed,
        vec![FailedSetup::Existing {
            udid: "bad".into(),
            unreachable: true,
            refused: false,
        }]
    );
    assert_eq!(h.connections.released.load(Ordering::SeqCst), 1);
    let h = Harness::new();
    h.creating.fail_first.store(true, Ordering::SeqCst);
    *h.worker.before.lock().unwrap() = Arc::new(|args| {
        Box::pin(async move {
            if args.get(2).is_some_and(|s| s == "delete") {
                Err(error("delete failed"))
            } else {
                Ok(None)
            }
        })
    });
    assert_eq!(
        h.profiles(1).await?,
        vec![FailedSetup::Created {
            profile: profile(),
            unreachable: false,
            refused: false,
        }]
    );
    // At default ERROR, WARN is suppressed; turning the registry on is covered separately by timing/logger tests.
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a4_empty_udid_is_non_null_and_is_released_and_deleted_on_failure() -> TestResult {
    let h = Harness::new();
    h.commands.passthrough.store(true, Ordering::SeqCst);
    h.connections.unavailable.store(true, Ordering::SeqCst);
    *h.worker.before.lock().unwrap() = Arc::new(|args| {
        Box::pin(async move {
            if args.get(2).is_some_and(|s| s == "create") {
                Ok(Some(ok([""])))
            } else {
                Ok(None)
            }
        })
    });
    assert_eq!(
        h.profiles(1).await?,
        vec![FailedSetup::Created {
            profile: profile(),
            unreachable: true,
            refused: false,
        }]
    );
    assert!(
        h.worker
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|a| a == &["xcrun", "simctl", "delete", ""])
    );
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn release_failure_replaces_disposal_and_registration_errors_in_the_finally() -> TestResult {
    let h = Harness::new();
    *h.connections.release_error.lock().unwrap() = Some(error("release"));
    let (logging, logs) = logging();
    let created = h.creating.clone();
    let p = marathon_ios::device::simulator_provisioner::SimulatorProvisioner::new(
        h.connections.clone(),
        h.creating.clone(),
        Arc::new(move |_, d| {
            let devices = created.devices.lock().unwrap();
            let device = devices.last().unwrap();
            assert_eq!(device.device(), d.device);
            assert!(Arc::ptr_eq(device, &d.handle));
            *device.dispose_error.lock().unwrap() = Some(error("dispose"));
            Box::pin(async { Err(error("register").into()) })
        }),
        2,
        logging,
        h.clock.as_ref(),
    )?;
    let failed = p
        .provision(
            ProvisioningPlan {
                existing_simulators: ["a".into()].into_iter().collect(),
                ..Default::default()
            },
            worker("worker"),
            h.bin.clone(),
            h.cycle.clone(),
            h.context.clone(),
        )
        .await?;
    assert_eq!(
        failed,
        vec![FailedSetup::Existing {
            udid: "a".into(),
            unreachable: false,
            refused: false,
        }]
    );
    assert!(logs.lock().unwrap().iter().any(|(_, _, _, e)| {
        e.as_ref()
            .is_some_and(|e| e.message.as_ref().is_some_and(|m| m.to_owned() == "release"))
    }));
    assert_eq!(h.creating.devices.lock().unwrap()[0].disposed.load(Ordering::SeqCst), 1);
    Ok(())
}
