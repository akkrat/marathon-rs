//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt
//! Supplemental provider snapshot witness over WP 0.4b's shared Apple world.
mod provider_support;
mod simulator_support;
mod support;
use fake_worker::{
    apple::{Scenario, UDID, World},
    fs::VirtualFs,
    router::Router,
    script::Script,
};
use marathon_config::devices::Transport;
use marathon_core::vendor::DeviceProvider;
use marathon_test_support::apple::WorldExecutor;
use provider_support::*;
use simulator_support::TestResult;
use std::sync::Arc;
#[tokio::test]
async fn snapshot_add_remove_and_readd_share_the_existing_world_executor() -> TestResult {
    let p = ProviderHarness::new(|_| {})?;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fake-worker/apple/ios-smoke/script/world.json");
    let mut scenario: Scenario = serde_json::from_slice(&std::fs::read(path)?)?;
    scenario.expectations.clear();
    scenario.devices = vec![UDID.into()];
    let world = World::new(scenario).map_err(|e| e.to_string())?;
    let fs = VirtualFs::create(p.h.dir.path().join("root"))
        .await
        .map_err(|e| e.to_string())?;
    let executor = Arc::new(WorldExecutor::new(
        p.h.services.runtime.clone(),
        &mut p.h.services.logging.registry.lock().unwrap(),
        Router {
            script: Script::default(),
            rsync_remote: "/usr/bin/rsync".into(),
            rsync_binary: "/usr/bin/rsync".into(),
            extensions: vec![Arc::new(world)],
        },
        fs,
        p.h.dir.path().to_owned(),
    ));
    *p.connections.workers.lock().unwrap() = vec![(Transport::Local, executor.clone())];
    p.update(&[]).await?;
    p.update(&[UDID]).await?;
    let original = p.connected(1).await.remove(0);
    p.update(&[]).await?;
    assert!(original.disposed());
    assert_eq!(p.provider.devices().len(), 1);
    p.provider.subscribe().receive().await.map_err(test_error)?;
    p.update(&[UDID]).await?;
    let replacement = p.connected(1).await.remove(0);
    assert!(!Arc::ptr_eq(&original, &replacement));
    p.finish().await
}
