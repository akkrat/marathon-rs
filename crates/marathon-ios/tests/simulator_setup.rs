//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt
mod simulator_support;
mod support;
use marathon_apple::{apple_device::AppleDevice, cmd::CommandResult, model::Arch};
use marathon_core::{model::DeviceFeature, vendor::Device};
use simulator_support::*;
use std::sync::{Arc, atomic::Ordering};
#[tokio::test(start_paused = true)]
async fn setup_probe_order_fallbacks_and_fresh_info_are_preserved() -> TestResult {
    let h = Harness::new(|_| {})?;
    let plan = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let reply = if cmd == ["env"] {
            Some(CommandResult {
                stdout: vec!["USER=x".into(), "  NO_EQUAL  ".into(), " KEY = value = rest ".into()],
                stderr: vec![],
                exit_code: Some(0),
            })
        } else if cmd.last().is_some_and(|s| s == "SIMULATOR_HOST_HOME") {
            Some(marathon_test_support::apple::fake_simulator_worker::ok([
                "/Users/worker",
            ]))
        } else {
            plan.answer(&cmd)
        };
        Box::pin(async move { Ok(reply) })
    });
    let d = h.device().await?;
    assert_eq!(d.root_path(), "/Users/worker/Library/Developer/CoreSimulator/Devices/a");
    assert_eq!(d.arch(), Arch::arm64);
    assert_eq!(d.info().operating_system.version.to_owned(), "Unknown");
    assert_eq!(d.info().model.to_owned(), "model-a");
    assert_eq!(
        d.info().device_features,
        [DeviceFeature::Video, DeviceFeature::Screenshot]
    );
    let cmds = h.worker.commands.lock().unwrap().clone();
    assert_eq!(
        cmds[1..5].iter().map(|c| c.join(" ")).collect::<Vec<_>>(),
        [
            "env",
            "xcrun xcodebuild -version",
            "uname -m",
            "/usr/sbin/system_profiler -detailLevel mini -xml SPDisplaysDataType"
        ]
    );
    assert_eq!(h.worker.getenv("a", "SIMULATOR_HOST_HOME"), 1);
    let before = d.info();
    *h.worker.arch.lock().unwrap() = "x86_64".into();
    d.setup(&h.services.context).await.unwrap();
    assert_eq!(d.arch(), Arch::x86_64);
    assert_eq!(before.model, d.info().model);
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn blank_home_is_a_setup_failure_before_simulator_getenv() -> TestResult {
    let h = Harness::new(|_| {})?;
    let plan = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let reply = if cmd == ["env"] {
            Some(marathon_test_support::apple::fake_simulator_worker::ok(["HOME=  "]))
        } else {
            plan.answer(&cmd)
        };
        Box::pin(async move { Ok(reply) })
    });
    let error = h.device().await.err().unwrap().to_string();
    assert!(error.contains("invalid value  for environment variable HOME"));
    assert_eq!(h.worker.getenv("a", "HOME"), 0);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn nonzero_display_probe_still_detects_metal_and_is_shareable() -> TestResult {
    let h = Harness::new(|_| {})?;
    let plan = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let reply = if cmd.first().is_some_and(|s| s == "/usr/sbin/system_profiler") {
            Some(CommandResult {
                stdout: vec!["spdisplays_metalfamily".into()],
                stderr: vec![],
                exit_code: Some(1),
            })
        } else {
            plan.answer(&cmd)
        };
        Box::pin(async move { Ok(reply) })
    });
    let d = h.device().await?;
    assert!(d.info().device_features.contains(&DeviceFeature::Video));
    h.finish(&[d]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn shutdown_wait_ignores_configured_timeouts_and_uses_thirty_fixed_checks() -> TestResult {
    use std::sync::atomic::Ordering;
    let h = Harness::new(|v| v.timeout_configuration.shutdown = support::duration(1))?;
    let d = h.device().await?;
    h.boot.state.store(3, Ordering::SeqCst);
    let plan = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let answer = if cmd.get(2).is_some_and(|s| s == "shutdown") {
            Some(marathon_test_support::apple::fake_simulator_worker::ok(
                Vec::<String>::new(),
            ))
        } else {
            plan.answer(&cmd)
        };
        Box::pin(async move { Ok(answer) })
    });
    let start = h.clock.now();
    let reads = h.boot.reads.load(Ordering::SeqCst);
    assert!(!d.shutdown(&h.services.context).await.unwrap());
    assert_eq!(h.clock.now() - start, 30_000);
    assert_eq!(h.boot.reads.load(Ordering::SeqCst) - reads, 31);
    h.finish(&[d]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn construction_does_no_io_and_partial_setup_disposal_keeps_lateinit_failures() -> TestResult {
    use marathon_config::{
        devices::Transport,
        vendor::{IosPermission, LifecycleAction},
    };
    use marathon_ios::{
        apple_simulator_device::AppleSimulatorDevice,
        device::{simulator_factory::SimulatorConstruction, worker_provisioning_context::WorkerProvisioningContext},
    };
    let h = Harness::new(|v| v.lifecycle_configuration.on_dispose = vec![LifecycleAction::Shutdown])?;
    let bin = h.binary(h.worker.clone());
    let cycle = WorkerProvisioningContext::new(Transport::Local, h.clock.clone());
    let record = cycle.simulator("a", &bin, false, &h.services.context).await?;
    h.worker.commands.lock().unwrap().clear();
    let d = AppleSimulatorDevice::new(
        SimulatorConstruction {
            transport: Transport::Local,
            executor: h.worker.clone(),
            bridge: Arc::new(support::BridgeMock),
            bin,
            record,
            sdk: marathon_apple::model::Sdk::IphoneSimulator,
            shared_transfers: Arc::new(marathon_apple::cmd::shared_transfers::SharedTransfers::new(
                h.services.runtime.clone(),
                h.services.logging.clone(),
            )),
        },
        h.services.clone(),
    );
    assert!(h.worker.commands.lock().unwrap().is_empty());
    let error = d.simulator_state(&h.services.context).await.unwrap_err();
    assert_eq!(error.failure().kind.description().to_owned(), "uninitialized property");
    assert_eq!(
        error.failure().message.as_ref().unwrap().to_owned(),
        "lateinit property devicePlistPath has not been initialized"
    );
    let error = d
        .grant(IosPermission::UserTracking, "app", &h.services.context)
        .await
        .unwrap_err();
    assert_eq!(
        error.failure().message.as_ref().unwrap().to_owned(),
        "lateinit property env has not been initialized"
    );
    d.dispose().await.unwrap();
    assert!(d.disposed());
    assert!(h.worker.commands.lock().unwrap().is_empty());
    let evidence: Vec<serde_json::Value> = include_str!("../../../fixtures/oracle/simulator-device/events.jsonl")
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let fields: Vec<_> = evidence.iter().filter(|r| r["kind"] == "partialField").collect();
    assert_eq!(fields.len(), 2);
    for row in fields {
        assert_eq!(
            marathon_test_support::recorded_errors::kind(row["class"].as_str().unwrap()),
            marathon_core::error::FailureKind::UninitializedProperty
        );
    }
    let disposed = evidence.iter().find(|r| r["kind"] == "partialDisposal").unwrap();
    assert_eq!(disposed["commands"], 0);
    assert_eq!(disposed["disposed"], d.disposed());
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn setup_on_a_lost_connection_fails_instead_of_describing_the_simulator_with_defaults_d36() -> TestResult {
    use marathon_apple::cmd::remote::ssh::sshj::error::CONNECTION_EXCEPTION;
    let h = Harness::new(|_| {})?;
    let d = h.device().await?;
    // Lost after the home and the descriptor were read: safeExecute answers the remaining probes with null (A17),
    // and each of them has a default.
    h.worker.connected.store(false, Ordering::SeqCst);
    let plan = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let reply = if cmd.last().is_some_and(|s| s == "HOME") {
            Some(marathon_test_support::apple::fake_simulator_worker::ok([
                "/Users/worker/Library/Developer/CoreSimulator/Devices/a/data",
            ]))
        } else if cmd.iter().any(|arg| arg == "getenv") {
            None
        } else {
            plan.answer(&cmd)
        };
        Box::pin(async move { Ok(reply) })
    });
    let error = d.setup(&h.services.context).await.unwrap_err();
    assert_eq!(
        (
            error.failure().kind.description().to_owned(),
            error.failure().message.as_ref().unwrap().to_owned()
        ),
        (
            CONNECTION_EXCEPTION.description().to_owned(),
            "Not connected".to_owned()
        )
    );
    h.worker.connected.store(true, Ordering::SeqCst);
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
