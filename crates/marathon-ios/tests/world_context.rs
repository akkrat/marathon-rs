//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/WorkerProvisioningContext.kt
//! Supplementary shared-world mutation/replacement witness; concrete setup belongs to 5.2.
mod support;
use fake_worker::{
    apple::{Scenario, World},
    fs::VirtualFs,
    router::Router,
    script::Script,
};
use marathon_apple::cmd::CommandExecutor;
use marathon_config::devices::Transport;
use marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext;
use marathon_test_support::apple::WorldExecutor;
use std::{
    path::Path,
    sync::{Arc, atomic::Ordering},
};
use support::*;
#[tokio::test]
async fn world_inventory_mutation_survives_replacement_connections_and_facts_are_reloaded() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fake-worker/apple/ios-smoke/script/world.json");
    let mut scenario: Scenario = serde_json::from_slice(&std::fs::read(path)?)?;
    scenario.expectations.clear();
    let world = World::new(scenario).map_err(|e| e.to_string())?;
    let fs = VirtualFs::create(dir.path().join("root"))
        .await
        .map_err(|e| e.to_string())?;
    let router = Router {
        script: Script::default(),
        rsync_remote: "/usr/bin/rsync".into(),
        rsync_binary: "/usr/bin/rsync".into(),
        extensions: vec![Arc::new(world)],
    };
    let (logging, _) = logging();
    let executor = Arc::new(WorldExecutor::new(
        Arc::new(Runtime),
        &mut logging.registry.lock().unwrap(),
        router,
        fs,
        dir.path().to_owned(),
    ));
    let replacement = Arc::new(executor.new_connection("replacement"));
    let first_bin = bin(executor.clone(), logging.clone());
    let second_bin = bin(replacement.clone(), logging);
    let caller = context();
    let cycle = WorkerProvisioningContext::new(Transport::Local, caller.hooks.clone());
    let old = cycle.inventory(&first_bin, &caller).await?;
    let udid = old.devices.devices[0].udid.clone();
    cycle
        .worker_facts(executor.clone(), &caller, || async { Ok(facts(true)) })
        .await?;
    first_bin
        .xcrun
        .simctl
        .simulator
        .boot(&udid, &[], &caller)
        .await
        .map_err(|e| e.failure().to_string().to_owned())?;
    assert_eq!(
        cycle.inventory(&first_bin, &caller).await?.devices.devices[0].udid,
        udid
    );
    assert_eq!(
        cycle.inventory(&first_bin, &caller).await?.devices.devices[0].state,
        marathon_apple::bin::xcrun::simctl::model::SimctlDeviceState::Shutdown
    );
    executor.connected.store(false, Ordering::SeqCst);
    assert_eq!(
        cycle.inventory(&second_bin, &caller).await?.devices.devices[0].state,
        marathon_apple::bin::xcrun::simctl::model::SimctlDeviceState::Booted
    );
    let changed = cycle
        .worker_facts(replacement.clone(), &caller, || async {
            let mut value = facts(true);
            value.arch = "replacement".into();
            Ok(value)
        })
        .await?;
    assert_eq!(changed.arch.to_owned(), "replacement");
    executor.close().await.map_err(|e| e.failure().to_string().to_owned())?;
    replacement
        .close()
        .await
        .map_err(|e| e.failure().to_string().to_owned())?;
    Ok(())
}
