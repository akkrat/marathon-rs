//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProviderTest.kt
//! and StartupPhasesTest.kt, AppleSimulatorProviderArtifactTest.kt, WorkerProvisioningContextTest.kt, AppleDeviceProviderTest.kt.
mod provider_support;
mod simulator_support;
mod support;
use marathon_apple::apple_device::AppleDevice;
use marathon_config::devices::{AppleTarget, Transport};
use marathon_core::{
    executor::shared::SharedDeferred,
    runtime::Deferred,
    vendor::{Device, DeviceProvider},
};
use marathon_ios::device::simulator_provisioner::SimulatorProvisioner;
use marathon_test_support::apple::fake_simulator_worker::{ENV, LIST, SYSTEM_PROFILER, UNAME};
use provider_support::*;
use simulator_support::TestResult;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use support::{profile, worker};

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
        .filter(|(logger, _, _, _)| logger == "Timings")
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
    let provider = p.provider.clone();
    let close = tokio::spawn(async move { provider.terminate().await });
    eventually(|| p.provider.subscribe().state().closed_for_send).await;
    assert!(!device.disposed());
    assert_eq!(p.connections.released.load(Ordering::SeqCst), 1);
    assert!(!close.is_finished());
    release.complete(Ok(()));
    close.await.map_err(test_error)?.map_err(test_error)?;
    assert!(device.disposed());
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
    p.h.services
        .logging
        .registry
        .lock()
        .unwrap()
        .logger("AppleSimulatorProvider", Some(jcompat::logging::Level::Info));
    let path = p.h.dir.path().join("Marathondevices");
    std::fs::write(path, "workers: []\n")?;
    assert!(p.provider.initialize(p.h.services.context.clone()).await.is_err());
    assert!(
        p.h.logs
            .lock()
            .unwrap()
            .iter()
            .any(|(name, level, s, _)| name == "AppleSimulatorProvider"
                && *level == jcompat::logging::Level::Info
                && *s
                    == marathon_apple::bin::xcrun::xcresulttool::describe_timeout(
                        &p.h.services.vendor.timeout_configuration
                    ))
    );
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
        p.h.services.context.hooks.as_ref(),
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
                marathonfile: jcompat::file_manager::JavaPath::from_text("/Marathonfile"),
                marathonfile_dir: jcompat::file_manager::JavaPath::from_text("/"),
                process_dir: jcompat::file_manager::JavaPath::from_text(p.h.dir.path().to_str().unwrap()),
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
