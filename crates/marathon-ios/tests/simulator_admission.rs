//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/StartupPhasesTest.kt
//! Supplemental default retry, recovery and admission boundaries.
mod provider_support;
mod recovery_support;
mod simulator_support;
mod support;
use marathon_core::error::{LeavePoolReason, VendorError};
use provider_support::*;
use recovery_support::*;
use simulator_support::*;
use std::sync::{Arc, atomic::Ordering};

#[tokio::test(start_paused = true)]
async fn a_late_simulator_prepared_after_the_late_join_cut_off_leaves_the_run_before_its_first_batch() -> TestResult {
    let p = RecoveryHarness::fast()?;
    p.fail_initial();
    p.update(&["a", "b"]).await?;
    let mut ds = p.connected(2).await;
    ds.sort_by(|a, b| a.serial().cmp(b.serial()));
    p.h().prepare(&ds[0]).await.map_err(test_error)?;
    assert!(!ds[0].retired());
    p.time.wall_offset.store(600_001, Ordering::SeqCst);
    let script = Arc::new(BatchScript::default());
    script.attach(&p.h().worker);
    let error = p.h().prepare(&ds[1]).await.unwrap_err();
    assert!(matches!(
        error,
        VendorError::LeavePool {
            reason: LeavePoolReason::NotAdmitted,
            ..
        }
    ));
    assert!(ds[1].retired());
    assert_eq!(script.count.load(Ordering::SeqCst), 0);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn a_simulator_that_fails_to_boot_is_set_up_again_by_its_provider_as_often_as_a_lost_one() -> TestResult {
    let p = RecoveryHarness::settings(|_| {}, 2, 50)?;
    p.start(&["a"]).await?;
    let mut device = p.connected(1).await.remove(0);
    for _ in 0..2 {
        p.lose(&device).await;
        device = p.connected(1).await.remove(0);
    }
    p.lose(&device).await;
    eventually(|| p.log("has been lost 3 times")).await;
    assert_eq!(home(&p.h().worker, "a"), 3);
    assert!(device.disposed());
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn omitted_worker_connection_defaults_retry_at_0_10_30_90_150_seconds_with_exact_logs() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    let config = &p.h().services.vendor.worker_connection;
    assert_eq!(config.attempts, 3);
    assert_eq!(config.backoff, support::duration(10_000));
    assert_eq!(config.retry_interval, support::duration(60_000));
    assert!(config.late_join);
    assert_eq!(config.late_join_cutoff, support::duration(600_000));
    p.answer(|_, _, _| Box::pin(async { Err(unreachable()) }));
    p.update(&["a"]).await?;
    for pause in [10_000, 20_000, 60_000, 60_000] {
        advance(pause).await;
    }
    settle().await;
    let times: Vec<_> = p
        .connections
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(_, _, time)| *time)
        .collect();
    let expected: serde_json::Value = include_str!("../../../fixtures/oracle/simulator-recovery/probes.jsonl")
        .lines()
        .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap())
        .find(|v| v["case"] == "omitted-worker-defaults")
        .unwrap()["value"]
        .clone();
    assert_eq!(serde_json::json!(times), expected["offsets"]);
    let logs = p.h().logs.lock().unwrap().clone();
    let messages: Vec<_> = logs
        .iter()
        .filter(|(name, _, _, _)| name == "AppleSimulatorProvider")
        .map(|(_, _, m, _)| m.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "Processing updates from com.malinskiy.marathon.apple.configuration.Transport$Local@abc:\n- Connected(target=Simulator(udid=a))",
            "Failed to connect to worker local (No route to host), retrying",
            "Providing 0 devices",
            "Connecting to worker local, attempt 2",
            "Failed to connect to worker local, attempt 2: No route to host",
            "Connecting to worker local, attempt 3",
            "Failed to connect to worker local, attempt 3: No route to host",
            "Failed to connect to worker local after 3 attempts (No route to host), retrying every 60 s while the run is active",
            "Connecting to worker local, attempt 4",
            "Failed to connect to worker local, attempt 4: No route to host",
            "Connecting to worker local, attempt 5",
            "Failed to connect to worker local, attempt 5: No route to host"
        ]
    );
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn cutoff_is_strict_at_600_seconds_and_stays_closed_after_wall_time_moves_backward() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.time.wall_offset.store(600_000, Ordering::SeqCst);
    assert!(p.recovery.late_join_cutoff().map_err(test_error)?.is_none());
    p.time.wall_offset.store(600_001, Ordering::SeqCst);
    let reason = p.recovery.late_join_cutoff().map_err(test_error)?.unwrap();
    assert_eq!(reason, "the run started more than 600 s ago");
    p.time.wall_offset.store(-600_000, Ordering::SeqCst);
    assert_eq!(p.recovery.late_join_cutoff().map_err(test_error)?, Some(reason));
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn zero_cutoff_disables_wall_limit_but_idle_threshold_is_strict_and_global() -> TestResult {
    let p = RecoveryHarness::new(|v| v.worker_connection.late_join_cutoff = support::duration(0))?;
    p.update(&["a"]).await?;
    let d = p.connected(1).await.remove(0);
    p.h().prepare(&d).await.map_err(test_error)?;
    let script = Arc::new(BatchScript::default());
    script.output(passed(1));
    script.attach(&p.h().worker);
    let _ = run(p.h(), &d).await;
    let since = d.waiting_since().unwrap();
    p.time.wall_offset.store(i64::MAX / 2, Ordering::SeqCst);
    p.time.nano_offset.store(
        since
            .wrapping_add(30_000_000_000)
            .wrapping_sub(p.h().clock.now() * 1_000_000),
        Ordering::SeqCst,
    );
    assert!(p.recovery.late_join_cutoff().map_err(test_error)?.is_none());
    p.time.nano_offset.fetch_add(1, Ordering::SeqCst);
    assert_eq!(
        p.recovery.late_join_cutoff().map_err(test_error)?,
        Some("a@local has been waiting for a batch for more than 30 s".into())
    );
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn default_recovery_pauses_0_30_60_seconds_then_fourth_loss_exhausts_the_id() -> TestResult {
    let p = RecoveryHarness::new(|_| {})?;
    p.update(&["a"]).await?;
    p.connected(1).await;
    for pause in [0, 30_000, 60_000] {
        let old = p.current("a");
        p.lose(&old).await;
        let at = p.h().clock.now();
        p.scan().await;
        advance(pause as u64).await;
        let new = p.connected(1).await.remove(0);
        assert_eq!(p.h().clock.now() - at, pause);
        assert!(!Arc::ptr_eq(&old, &new));
    }
    p.lose(&p.current("a")).await;
    p.scan().await;
    eventually(|| p.log("has been lost 4 times")).await;
    assert_eq!(home(&p.h().worker, "a"), 4);
    p.update(&[]).await?;
    p.update(&["a"]).await?;
    assert_eq!(home(&p.h().worker, "a"), 4);
    p.balanced().await
}
#[tokio::test(start_paused = true)]
async fn recovery_waits_70_seconds_for_an_open_parsing_session_then_retires_only_that_device() -> TestResult {
    use marathon_apple::{apple_device::AppleDevice, test::TestRequest, xcodebuild_session::XcodebuildSession};
    use marathon_config::devices::Transport;
    use marathon_core::{executor::shared::SharedDeferred, runtime::Deferred};
    let p = RecoveryHarness::fast()?;
    p.update(&["a", "b"]).await?;
    p.connected(2).await;
    let old = p.current("a");
    p.h().prepare(&old).await.map_err(test_error)?;
    let script = Arc::new(BatchScript::default());
    let gate = SharedDeferred::new();
    *script.plan.lock().unwrap() = OutputPlan {
        hang: true,
        close_gate: Some(gate.clone()),
        ..Default::default()
    };
    script.attach(&p.h().worker);
    let session = XcodebuildSession::new(p.h().services.runtime.clone(), p.h().services.logging.clone());
    let _events = old
        .execute_test_request(
            TestRequest::new("/tmp/marathon", "/tmp/marathon/parse.xctestrun", false),
            session.clone(),
            &p.h().services.context,
        )
        .await
        .map_err(test_error)?;
    script.opened.wait().await.map_err(test_error)?;
    assert!(!old.retired());
    let fresh = p.base.worker(Transport::Local, &["a", "b"]);
    *p.base.connections.workers.lock().unwrap() = vec![(Transport::Local, fresh.clone())];
    p.h().worker.connected.store(false, Ordering::SeqCst);
    p.scan().await;
    settle().await;
    assert!(old.disposed());
    advance(69_999).await;
    assert!(!old.retired());
    assert_eq!(home(&fresh, "a"), 0);
    advance(1).await;
    assert!(old.retired());
    assert_eq!(p.connected(1).await[0].udid(), "b");
    assert_eq!(home(&fresh, "a"), 0);
    gate.complete(Ok(()));
    let _ = session.stop(support::duration(100), &p.h().services.context).await;
    session.join_producer().await.map_err(test_error)?;
    p.finish().await
}
#[tokio::test(start_paused = true)]
async fn production_vendor_services_keep_default_retry_and_run_a_healthy_peer_before_late_admission() -> TestResult {
    use marathon_core::{executor::shared::SharedDeferred, runtime::Deferred, vendor::DeviceEvent};
    use marathon_ios::{device::simulator_provider::recovery::SimulatorRecovery, ios_vendor::IosVendor};
    let p = RecoveryHarness::new(|_| {})?;
    let late_worker = p.base.worker(support::worker("late"), &["a"]);
    let gate = SharedDeferred::new();
    let g = gate.clone();
    p.answer(move |t, w, n| {
        let g = g.clone();
        Box::pin(async move {
            if t == support::worker("late") && w {
                if n == 1 {
                    return Err(unreachable());
                }
                g.wait().await?;
            }
            Ok(None)
        })
    });
    let services = ProviderHarness::services(p.h(), p.connections.clone(), SimulatorRecovery::new());
    let vendor = IosVendor {
        simulator: p.h().services.clone(),
        binding: services.binding,
        connections: p.connections.clone(),
        recovery: services.recovery,
        shared_transfers: Arc::new(marathon_apple::cmd::shared_transfers::SharedTransfers::new(
            p.h().services.runtime.clone(),
            p.h().services.logging.clone(),
        )),
        registry: services.registry,
        parser_libraries: Default::default(),
    }
    .construct()?;
    std::fs::write(
        p.h().dir.path().join("Marathondevices"),
        "workers:\n- transport: {type: local}\n  devices: [{type: simulator, udid: b}]\n- transport: {type: ssh, addr: late}\n  devices: [{type: simulator, udid: a}]\n",
    )?;
    vendor
        .provider
        .initialize(p.h().services.context.clone())
        .await
        .map_err(test_error)?;
    let Some(DeviceEvent::Connected(healthy)) = support::watchdog(vendor.provider.subscribe().receive())
        .await
        .map_err(test_error)?
    else {
        panic!("healthy peer missing")
    };
    assert!(healthy.0.info().serial_number.to_string_lossy().starts_with("b@"));
    healthy
        .0
        .prepare(p.h().services.configuration.clone(), p.h().services.context.clone())
        .await
        .map_err(test_error)?;
    let script = Arc::new(BatchScript::default());
    script.output(passed(1));
    script.attach(&p.h().worker);
    let results = SharedDeferred::new();
    let batch = Arc::new(marathon_core::model::TestBatch {
        tests: tests(),
        id: "healthy".into(),
    });
    healthy
        .0
        .execute(
            p.h().services.configuration.clone(),
            marathon_core::model::DevicePoolId { name: "pool".into() },
            batch,
            results.clone(),
            p.h().services.context.clone(),
        )
        .await
        .map_err(test_error)?;
    assert!(results.completed().is_some());
    gate.complete(Ok(()));
    let Some(DeviceEvent::Connected(late)) = support::watchdog(vendor.provider.subscribe().receive())
        .await
        .map_err(test_error)?
    else {
        panic!("late peer missing")
    };
    late.0
        .prepare(p.h().services.configuration.clone(), p.h().services.context.clone())
        .await
        .map_err(test_error)?;
    let script = Arc::new(BatchScript::default());
    script.output(passed(1));
    script.attach(&late_worker);
    let results = SharedDeferred::new();
    let batch = Arc::new(marathon_core::model::TestBatch {
        tests: tests(),
        id: "late".into(),
    });
    late.0
        .execute(
            p.h().services.configuration.clone(),
            marathon_core::model::DevicePoolId { name: "pool".into() },
            batch,
            results.clone(),
            p.h().services.context.clone(),
        )
        .await
        .map_err(test_error)?;
    assert!(results.completed().is_some());
    assert_eq!(p.connections.attempts(&support::worker("late"), true), 2);
    vendor.provider.terminate().await.map_err(test_error)?;
    Ok(())
}
