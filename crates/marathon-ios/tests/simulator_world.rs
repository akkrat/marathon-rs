//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt
//! WP 5.2's in-process 0.4b setup/prepare/batch/dispose witness.
mod simulator_support;
mod support;
use fake_worker::{
    apple::{Scenario, World},
    fs::VirtualFs,
    router::Router,
    script::Script,
};
use marathon_apple::cmd::CommandExecutor;
use marathon_config::{devices::Transport, vendor::LifecycleAction};
use marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext;
use marathon_test_support::apple::WorldExecutor;
use simulator_support::*;
use std::sync::Arc;
#[tokio::test]
async fn setup_prepare_batch_and_dispose_share_the_in_process_apple_world() -> TestResult {
    let mut h = Harness::new(|v| {
        v.lifecycle_configuration.on_dispose = vec![LifecycleAction::Shutdown];
        v.xcresult.remote_clean = true;
    })?;
    let runner = h.dir.path().join("runner.app");
    std::fs::create_dir_all(runner.join("PlugIns"))?;
    std::fs::write(
        runner.join("Info.plist"),
        "<plist><dict><key>CFBundleIdentifier</key><string>com.example.Runner</string><key>CFBundleName</key><string>Runner</string><key>CFBundleExecutable</key><string>Runner</string><key>CFBundleSupportedPlatforms</key><array><string>iPhoneSimulator</string></array></dict></plist>",
    )?;
    std::fs::write(runner.join("Runner"), [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0])?;
    std::fs::rename(h.dir.path().join("tests.xctest"), runner.join("PlugIns/tests.xctest"))?;
    let bundle = Arc::make_mut(&mut h.services.vendor).bundle.as_mut().unwrap();
    bundle.test_app = Some(jcompat::file_manager::JavaPath::from_text(runner.to_str().unwrap()));
    bundle.xctest = Some(jcompat::file_manager::JavaPath::from_text(
        runner.join("PlugIns/tests.xctest").to_str().unwrap(),
    ));
    Arc::make_mut(&mut h.services.configuration).vendor_configuration =
        marathon_config::VendorConfiguration::Ios((*h.services.vendor).clone());
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fake-worker/apple/ios-smoke/script/world.json");
    let mut scenario: Scenario = serde_json::from_slice(&std::fs::read(fixture)?)?;
    scenario.expectations.clear();
    scenario.devices = vec![fake_worker::apple::UDID.into()];
    scenario.target = "Tests".into();
    scenario.tests = vec![
        fake_worker::apple::Test {
            class: "Suite".into(),
            method: "test1".into(),
            status: "passed".into(),
        },
        fake_worker::apple::Test {
            class: "Suite".into(),
            method: "test2".into(),
            status: "failed".into(),
        },
    ];
    let world = World::new(scenario).map_err(|e| e.to_string())?;
    let fs = VirtualFs::create(h.dir.path().join("worker-root"))
        .await
        .map_err(|e| e.to_string())?;
    let router = Router {
        script: Script::default(),
        rsync_remote: "/usr/bin/rsync".into(),
        rsync_binary: "/usr/bin/rsync".into(),
        extensions: vec![Arc::new(world.clone())],
    };
    let worker = Arc::new(WorldExecutor::new(
        h.services.runtime.clone(),
        &mut h.services.logging.registry.lock().unwrap(),
        router,
        fs.clone(),
        h.dir.path().to_owned(),
    ));
    let d = h
        .device_on_bridge(
            worker.clone(),
            fake_worker::apple::UDID,
            Arc::new(WorkerProvisioningContext::new(Transport::Local, h.clock.clone())),
            Arc::new(worker.file_bridge()),
        )
        .await?;
    let prep = h.prepare(&d).await;
    assert!(
        prep.is_ok(),
        "{}; commands={:?}; logs={:?}",
        prep.unwrap_err().failure().to_java_string().to_string_lossy(),
        worker.commands.lock().unwrap(),
        h.logs.lock().unwrap()
    );
    let (r, results) = support::bounded(20, run(&h, &d)).await;
    assert!(r.is_ok(), "{r:?}");
    let results = results.unwrap();
    assert_eq!(results.finished.len(), 1);
    assert_eq!(results.failed.len(), 1);
    h.finish(&[d]).await;
    worker.close().await.unwrap();
    let state = world.final_state(&fs).await.map_err(|e| e.to_string())?;
    assert_eq!(state["devices"][fake_worker::apple::UDID]["state"], "Shutdown");
    assert_eq!(state["devices"][fake_worker::apple::UDID]["keyboard"], false);
    assert_eq!(state["sessions"][0]["state"], "finished");
    assert!(
        !state["devices"][fake_worker::apple::UDID]["installed"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    world.validate().map_err(|e| e.to_string())?;
    Ok(())
}
