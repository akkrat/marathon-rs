//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/video/ScreenRecordingListener.kt
mod listener_support;
mod simulator_support;
mod support;
use fake_worker::{
    apple::{Scenario, UDID, World},
    fs::VirtualFs,
    router::Router,
    script::Script,
};
use marathon_apple::{apple_device::AppleDevice, cmd::CommandExecutor, logparser::parser::DeviceFailureReason};
use marathon_config::{devices::Transport, schema::ScreenRecordingPolicy, vendor::*};
use marathon_core::{
    artifacts::AttachmentCollector,
    model::{DevicePoolId, Test, TestBatch},
};
use marathon_ios::{
    apple_simulator_device::SimulatorListenerFactory, device::worker_provisioning_context::WorkerProvisioningContext,
    listener::*,
};
use marathon_test_support::apple::WorldExecutor;
use simulator_support::TestResult;
use std::sync::Arc;

async fn replay(
    screenshot: bool,
    batch_failure: bool,
    codec: Codec,
    outcome: u8,
    policy: ScreenRecordingPolicy,
) -> TestResult {
    let mut h = listener_support::harness(|v| {
        v.device_log = true;
        v.screen_record_configuration.preferable_recorder_type = Some(if screenshot {
            IosRecorderType::Screenshot
        } else {
            IosRecorderType::Video
        });
        let video = &mut v.screen_record_configuration.video_configuration;
        video.enabled = true;
        video.codec = codec;
        video.transcoding.enabled = true;
        video.transcoding.binary = "/opt/homebrew/bin/ffmpeg".into();
        video.transcoding.size = TranscodingSize::Hd480;
        let shot = &mut v.screen_record_configuration.screenshot_configuration;
        shot.screenshot_type = ScreenshotType::Png;
        shot.enabled = true;
        shot.width = 32;
        shot.height = 48;
        shot.delay = support::duration(20);
    })?;
    Arc::make_mut(&mut h.services.configuration).screen_recording_policy = policy;
    let name = if screenshot { "ios-screenshot" } else { "ios-media" };
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../fixtures/fake-worker/apple/{name}/script/world.json"));
    let mut scenario: Scenario = serde_json::from_slice(&std::fs::read(fixture)?)?;
    // Whole-run Kotlin expectations include runner/result-bundle callbacks owned by 7.1.
    scenario.expectations.clear();
    let world = World::new(scenario).map_err(|e| e.to_string())?;
    let fs = VirtualFs::create(h.dir.path().join("worker"))
        .await
        .map_err(|e| e.to_string())?;
    let worker = Arc::new(WorldExecutor::new(
        h.services.runtime.clone(),
        &mut h.services.logging.registry.lock().unwrap(),
        Router {
            script: Script::default(),
            rsync_remote: "/usr/bin/rsync".into(),
            rsync_binary: "/usr/bin/rsync".into(),
            extensions: vec![Arc::new(world.clone())],
        },
        fs.clone(),
        h.dir.path().to_owned(),
    ));
    if batch_failure {
        *worker.before.lock().unwrap() = Arc::new(|cmd| {
            Box::pin(async move {
                Ok((cmd == ["echo", "quickhealthcheck"])
                    .then(|| marathon_test_support::apple::fake_simulator_worker::ok(["quickhealthcheck"])))
            })
        });
    }
    let d = h
        .device_on_bridge(
            worker.clone(),
            UDID,
            Arc::new(WorkerProvisioningContext::new(Transport::Local, h.clock.clone())),
            Arc::new(worker.file_bridge()),
        )
        .await?;
    assert!(d.supports_transcoding());
    d.execute_worker_command(
        vec!["xcrun".into(), "simctl".into(), "boot".into(), UDID.into()],
        &h.services.context,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    d.remote_file_manager()
        .create_remote_directory(None, &h.services.context)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let test = Test {
        pkg: "sample_appUITests".into(),
        clazz: "FailingTests".into(),
        method: "testAlwaysFailing".into(),
        meta_properties: vec![],
    };
    let c = ListenerContext {
        device: d.clone(),
        pool: DevicePoolId { name: "pool".into() },
        batch: Arc::new(TestBatch {
            tests: vec![test.clone()],
            id: "batch".into(),
        }),
        context: h.services.context.clone(),
    };
    let listeners = ProductionSimulatorListenerFactory
        .create(d.clone(), c.pool.clone(), c.batch.clone(), c.context.clone())
        .map_err(|e| format!("{e:?}"))?;
    let collector = AttachmentCollector::new(&listeners.recorder_attachments).map_err(|e| format!("{e:?}"))?;
    listeners
        .device_log
        .before_test_run()
        .await
        .map_err(|e| format!("{e:?}"))?;
    listeners
        .recorder
        .before_test_run()
        .await
        .map_err(|e| format!("{e:?}"))?;
    listeners
        .recorder
        .test_started(&test)
        .await
        .map_err(|e| format!("{e:?}"))?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let ready = if screenshot {
                worker
                    .commands
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|c| c.iter().any(|a| a == "screenshot"))
                    .count()
                    >= 2
            } else {
                h.logs
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|r| r.2.starts_with("started video recording"))
            };
            if ready {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;

    if batch_failure {
        listeners
            .recorder
            .test_run_failed(&"failure".into(), DeviceFailureReason::Unknown)
            .await
            .map_err(|e| format!("{e:?}"))?;
    } else if outcome == 2 {
        listeners
            .recorder
            .test_ignored(&test, 0, 0)
            .await
            .map_err(|e| format!("{e:?}"))?;
    } else if outcome == 0 {
        listeners
            .recorder
            .test_passed(&test, 0, 0)
            .await
            .map_err(|e| format!("{e:?}"))?;
    } else {
        listeners
            .recorder
            .test_failed(&test, 0, 0, None)
            .await
            .map_err(|e| format!("{e:?}"))?;
    }
    listeners
        .recorder
        .after_test_run()
        .await
        .map_err(|e| format!("{e:?}"))?;
    listeners
        .device_log
        .after_test_run()
        .await
        .map_err(|e| format!("{e:?}"))?;
    let attached = collector.get(&test);
    let should_attach = !batch_failure && (policy == ScreenRecordingPolicy::OnAny || outcome == 1);
    assert_eq!(
        attached.is_some(),
        should_attach,
        "{name} batch={batch_failure} logs={:?}",
        h.logs.lock().unwrap()
    );
    if let Some(attachments) = attached {
        let bytes = std::fs::read(attachments[0].file.to_file_path_buf()?)?;
        if screenshot {
            assert!(bytes.starts_with(b"GIF89a"));
            assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), 32);
        } else {
            assert_eq!(
                bytes,
                include_bytes!("../../../fixtures/fake-worker/apple/media/transcoded.mp4")
            );
        }
    }
    if screenshot && batch_failure {
        let target = |test| marathon_core::output::FileTarget {
            kind: marathon_core::output::FileType::Screenshot,
            pool: c.pool.clone(),
            device: marathon_core::vendor::Device::info(d.as_ref()),
            test,
            test_batch_id: Some(c.batch.id.clone()),
            id: None,
            chunk: None,
        };
        let src = h
            .services
            .output
            .create_file(&target(Some(test.clone())))
            .await
            .map_err(|e| format!("{e:?}"))?
            .to_file_path_buf()?;
        let dst = h
            .services
            .output
            .create_file(&target(None))
            .await
            .map_err(|e| format!("{e:?}"))?
            .to_file_path_buf()?;
        assert!(!src.exists());
        assert!(std::fs::read(&dst)?.starts_with(b"GIF89a"));
        // A97: lastTestIdentifier survives the failure; repeated callbacks rename again.
        std::fs::write(&src, b"second")?;
        listeners
            .recorder
            .test_run_failed(&"failure".into(), DeviceFailureReason::Unknown)
            .await
            .map_err(|e| format!("{e:?}"))?;
        assert_eq!(std::fs::read(&dst)?, b"second");
        std::fs::remove_file(&dst)?;
        std::fs::create_dir(&dst)?;
        std::fs::write(&src, b"kept")?;
        listeners
            .recorder
            .test_run_failed(&"failure".into(), DeviceFailureReason::Unknown)
            .await
            .map_err(|e| format!("{e:?}"))?;
        assert_eq!(std::fs::read(&src)?, b"kept", "rename failure Boolean is ignored");
    }
    if !screenshot {
        let remote = d
            .remote_file_manager()
            .remote_video_for_test(&test, &c.batch.id)
            .to_string_lossy();
        if batch_failure || should_attach {
            let temp = if batch_failure {
                format!("{remote}.tmp")
            } else {
                d.remote_file_manager()
                    .remote_temp_video_for_test(&test, &c.batch.id)
                    .to_string_lossy()
            };
            assert!(
                !fs.read(&temp).await.map_err(|e| e.to_string())?.is_empty(),
                "A19 original temp remains"
            );
            let commands = worker.commands.lock().unwrap().clone();
            let optional = if codec == Codec::H264 {
                "-movflags +faststart"
            } else {
                ""
            };
            assert!(commands.contains(&vec!["sh".into(), "-c".into(), format!("/opt/homebrew/bin/ffmpeg -i {temp} -vf \"scale=852:852:force_original_aspect_ratio=decrease:force_divisible_by=2\" -preset ultrafast {optional} {remote}")]));
        }
        assert!(fs.stat(&remote).await.is_err());
    }
    let state = world.final_state(&fs).await.map_err(|e| e.to_string())?;
    assert_eq!(state["deferred"], serde_json::json!([]));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !worker.processes().active().await.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    h.finish(&[d]).await;
    worker.close().await.map_err(|e| format!("{e:?}"))?;

    Ok(())
}
#[tokio::test]
async fn ios_media_replay_keeps_a19_temp_and_exact_transcode() -> TestResult {
    replay(false, false, Codec::H264, 1, ScreenRecordingPolicy::OnAny).await
}
#[tokio::test]
async fn batch_failure_transcodes_to_dot_tmp_without_policy_gate() -> TestResult {
    replay(false, true, Codec::H264, 0, ScreenRecordingPolicy::OnFailure).await
}
#[tokio::test]
async fn ios_screenshot_replay_policy_and_batch_rename() -> TestResult {
    for policy in [ScreenRecordingPolicy::OnAny, ScreenRecordingPolicy::OnFailure] {
        for outcome in 0..3 {
            replay(true, false, Codec::H264, outcome, policy).await?;
        }
    }
    replay(true, true, Codec::H264, 0, ScreenRecordingPolicy::OnFailure).await
}
