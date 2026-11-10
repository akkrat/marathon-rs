//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDeviceArtifactTest.kt
mod simulator_support;
mod support;
use marathon_core::{executor::shared::SharedDeferred, vendor::Device};
use simulator_support::*;
use std::sync::Arc;
#[tokio::test(start_paused = true)]
async fn replacement_device_prepare_cannot_clear_a_pending_bundle_directory() -> TestResult {
    let h = Harness::new(|_| {})?;
    let original = h.device().await?;
    let replacement = h.device().await?;
    let source = "/tmp/marathon/a/old.xcresult";
    h.services
        .artifacts
        .protect("a@local".into(), source.into(), Arc::new(|_| Ok(())))
        .unwrap();
    let gate = SharedDeferred::new();
    let release = gate.clone();
    h.services
        .artifacts
        .submit(
            "a@local".into(),
            "old".into(),
            "local".into(),
            None,
            Arc::new(move |_| {
                let gate = gate.clone();
                Box::pin(async move {
                    gate.wait().await?;
                    Ok(())
                })
            }),
            h.services.context.clone(),
        )
        .await
        .unwrap();
    original.dispose().await.unwrap();
    let before = h.worker.commands.lock().unwrap().len();
    let prep = h.prepare(&replacement);
    tokio::pin!(prep);
    assert!(futures_util::poll!(prep.as_mut()).is_pending());
    assert_eq!(h.worker.commands.lock().unwrap().len(), before);
    release.complete(Ok(()));
    prep.await.unwrap();
    assert!(
        h.worker.commands.lock().unwrap().iter().any(
            |c| c.first().is_some_and(|s| s == "find") && c.windows(3).any(|s| s == ["!", "-name", "old.xcresult"])
        )
    );
    h.finish(&[original, replacement.clone()]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn replacement_device_prepare_keeps_a_bundle_whose_delivery_failed() -> TestResult {
    let h = Harness::new(|_| {})?;
    let d = h.device().await?;
    let source = "/tmp/marathon/a/failed.xcresult";
    h.services
        .artifacts
        .protect("a@local".into(), source.into(), Arc::new(|_| Ok(())))
        .unwrap();
    h.services
        .artifacts
        .submit(
            "a@local".into(),
            "old".into(),
            "local".into(),
            None,
            Arc::new(|_| Box::pin(async { Err(support::error("delivery failed")) })),
            h.services.context.clone(),
        )
        .await
        .unwrap();
    h.services
        .artifacts
        .await_owner("a@local", &h.services.context)
        .await
        .unwrap();
    h.prepare(&d).await.unwrap();
    assert!(
        h.worker
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.first().is_some_and(|s| s == "find") && c.iter().any(|s| s == "failed.xcresult"))
    );
    h.finish(&[d]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn real_device_dispose_leaves_active_artifact_work_alive_and_rejects_subsequent_batches() -> TestResult {
    use simulator_support::artifacts::*;
    let h = Harness::new(|v| v.xcresult.pulling_policy = marathon_config::vendor::PullingPolicy::Always)?;
    let bridge = Arc::new(BundleBridge::default());
    let gate = SharedDeferred::new();
    *bridge.gate.lock().unwrap() = Some(gate.clone());
    let (d, s) = device(&h, bridge.clone()).await?;
    assert!(run(&h, &d).await.0.is_ok());
    bridge.entered.wait().await.unwrap();
    d.dispose().await.unwrap();
    assert!(d.disposed());
    assert!(run(&h, &d).await.0.is_err());
    assert_eq!(s.count.load(std::sync::atomic::Ordering::SeqCst), 1);
    let owner = "a@local";
    let barrier = h.services.artifacts.await_owner(owner, &h.services.context);
    tokio::pin!(barrier);
    assert!(futures_util::poll!(barrier.as_mut()).is_pending());
    gate.complete(Ok(()));
    barrier.await.unwrap();
    assert_eq!(result_folders(h.dir.path()).len(), 1);
    h.finish(&[d]).await;
    Ok(())
}
