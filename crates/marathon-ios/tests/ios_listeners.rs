//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/video/ScreenRecordingListener.kt
use marathon_error::FailureKind;
mod listener_support;
mod simulator_support;
mod support;
use listener_support::{Bridge, context, device, harness};
use marathon_apple::{cmd::CommandResult, listener::AppleTestRunListener, logparser::parser::DeviceFailureReason};
use marathon_config::{schema::ScreenRecordingPolicy, vendor::*};
use marathon_core::{artifacts::AttachmentCollector, error::VendorError, model::DeviceFeature};
use marathon_ios::listener::{data_container_clear::*, device_log::*, screenshot::*, video::*};
use marathon_test_support::apple::fake_simulator_worker::ok;
use simulator_support::TestResult;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
fn absent() -> CommandResult {
    CommandResult {
        stdout: vec![],
        stderr: vec![],
        exit_code: Some(1),
    }
}

#[test]
fn selector_truth_table() -> TestResult {
    let h = harness(|_| {})?;
    let mut config = h.services.vendor.screen_record_configuration.clone();
    for preference in [None, Some(IosRecorderType::Video), Some(IosRecorderType::Screenshot)] {
        for supported in 0..4 {
            for enabled in 0..4 {
                config.preferable_recorder_type = preference;
                config.video_configuration.enabled = enabled & 1 != 0;
                config.screenshot_configuration.enabled = enabled & 2 != 0;
                let features: Vec<_> = [(1, DeviceFeature::Video), (2, DeviceFeature::Screenshot)]
                    .into_iter()
                    .filter(|(bit, _)| supported & bit != 0)
                    .map(|(_, f)| f)
                    .collect();
                let available = supported & enabled;
                let expected = match (preference, available) {
                    (Some(IosRecorderType::Screenshot), 2 | 3) => Some(DeviceFeature::Screenshot),
                    (_, 1 | 3) => Some(DeviceFeature::Video),
                    (_, 2) => Some(DeviceFeature::Screenshot),
                    _ => None,
                };
                assert_eq!(
                    marathon_ios::recorder_type_selector::select_recorder_type(&features, &config),
                    expected,
                    "{preference:?} {supported} {enabled}"
                );
            }
        }
    }
    Ok(())
}
#[test]
fn transcode_and_log_command_strings_match_kotlin() -> TestResult {
    let h = harness(|_| {})?;
    let mut config = h
        .services
        .vendor
        .screen_record_configuration
        .video_configuration
        .clone();
    config.transcoding.binary = "/usr/local/bin/ffmpeg".into();
    config.transcoding.size = TranscodingSize::Hd480;
    config.codec = Codec::H264;
    assert_eq!(
        transcode_command(&config, "/src.mp4", "/src-temp.mp4"),
        [
            "sh",
            "-c",
            "/usr/local/bin/ffmpeg -i /src-temp.mp4 -vf \"scale=852:852:force_original_aspect_ratio=decrease:force_divisible_by=2\" -preset ultrafast -movflags +faststart /src.mp4"
        ]
    );
    config.codec = Codec::Hevc;
    assert_eq!(
        transcode_command(&config, "/src.mp4", "/src.mp4.tmp"),
        [
            "sh",
            "-c",
            "/usr/local/bin/ffmpeg -i /src.mp4.tmp -vf \"scale=852:852:force_original_aspect_ratio=decrease:force_divisible_by=2\" -preset ultrafast  /src.mp4"
        ]
    );
    assert_eq!(
        log_stream_command("a", "/log"),
        [
            "sh",
            "-c",
            "xcrun simctl spawn a log stream --type log --color none --style compact 2>/dev/null > /log & echo $!"
        ]
    );
    assert_eq!(parse_pid(true, "\u{a0}+0012\n"), Some("+0012".into()));
    for s in ["", "abc", "2147483648", "１２"] {
        assert_eq!(parse_pid(true, s), None);
    }
    assert_eq!(parse_pid(false, "123"), None);
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn video_policy_matrix_and_ignored_pull_boolean_emit_attachment() -> TestResult {
    for policy in [ScreenRecordingPolicy::OnAny, ScreenRecordingPolicy::OnFailure] {
        for outcome in 0..3 {
            let mut h = harness(|_| {})?;
            Arc::make_mut(&mut h.services.configuration).screen_recording_policy = policy;
            let bridge = Arc::new(Bridge::default());
            bridge.mode.store(1, Ordering::SeqCst);
            let d = device(&h, bridge.clone()).await.map_err(|e| format!("{e:?}"))?;
            let c = context(&h, d.clone());
            let test = c.batch.tests[0].clone();
            let l = ScreenRecordingListener::new(c);
            let collector = AttachmentCollector::new(&[
                l.attachments.clone() as Arc<dyn marathon_core::listener::AttachmentProvider>
            ])
            .map_err(|e| format!("{e:?}"))?;
            *h.worker.before.lock().unwrap() = Arc::new(|cmd| {
                Box::pin(async move {
                    Ok(Some(if cmd.first().is_some_and(|s| s == "cat") {
                        absent()
                    } else {
                        ok(Vec::<String>::new())
                    }))
                })
            });
            h.worker.commands.lock().unwrap().clear();
            match outcome {
                0 => l.test_passed(&test, 0, 0).await.map_err(|e| format!("{e:?}"))?,
                1 => l.test_failed(&test, 0, 0, None).await.map_err(|e| format!("{e:?}"))?,
                _ => l.test_ignored(&test, 0, 0).await.map_err(|e| format!("{e:?}"))?,
            }
            // D80: the pull runs after the callback, off the event path.
            l.settle().await.map_err(|e| format!("{e:?}"))?;
            let attach = policy == ScreenRecordingPolicy::OnAny || outcome == 1;
            assert_eq!(collector.get(&test).is_some(), attach);
            assert_eq!(bridge.pulls.lock().unwrap().len(), usize::from(attach));
            assert_eq!(
                h.worker.commands.lock().unwrap().last().unwrap(),
                &["rm", "-rf", "/tmp/marathon/a/Tests-Suite-test1-batch.mp4"]
            );
            h.finish(&[d]).await;
        }
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn video_transfer_only_catch_and_missing_file_skip_removal() -> TestResult {
    for mode in [2, 3, 4] {
        let mut h = harness(|_| {})?;
        Arc::make_mut(&mut h.services.configuration).screen_recording_policy = ScreenRecordingPolicy::OnAny;
        let bridge = Arc::new(Bridge::default());
        bridge.mode.store(mode, Ordering::SeqCst);
        let d = device(&h, bridge).await.map_err(|e| format!("{e:?}"))?;
        let c = context(&h, d.clone());
        let test = c.batch.tests[0].clone();
        let l = ScreenRecordingListener::new(c);
        *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
            Box::pin(async move {
                Ok(Some(if cmd[0] == "cat" || (mode == 4 && cmd[0] == "test") {
                    absent()
                } else {
                    ok(Vec::<String>::new())
                }))
            })
        });
        h.worker.commands.lock().unwrap().clear();
        l.test_failed(&test, 0, 0, None).await.map_err(|e| format!("{e:?}"))?;
        // D80: a non-transfer failure of the pull surfaces when the batch's videos are settled.
        let result = l.settle().await;
        assert_eq!(result.is_err(), mode == 3);
        assert!(!h.worker.commands.lock().unwrap().iter().any(|c| c[0] == "rm"));
        if mode == 2 {
            assert!(h.logs.lock().unwrap().iter().any(|r| r.2 == "Can't pull video"));
        }
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn log_disabled_no_pid_and_ignored_kill_and_pull_results() -> TestResult {
    for pid in [None, Some("nonsense"), Some("  +0012  ")] {
        let h = harness(|v| v.device_log = pid.is_some())?;
        let bridge = Arc::new(Bridge::default());
        bridge.mode.store(1, Ordering::SeqCst);
        let d = device(&h, bridge.clone()).await.map_err(|e| format!("{e:?}"))?;
        let l = DeviceLogListener::new(context(&h, d.clone()));
        let text = pid.unwrap_or("").to_owned();
        *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
            let text = text.clone();
            Box::pin(async move {
                Ok(Some(if cmd.last().is_some_and(|s| s.starts_with("kill")) {
                    absent()
                } else {
                    ok([text])
                }))
            })
        });
        h.worker.commands.lock().unwrap().clear();
        l.before_test_run().await.map_err(|e| format!("{e:?}"))?;
        l.after_test_run().await.map_err(|e| format!("{e:?}"))?;
        let numeric = pid == Some("  +0012  ");
        assert_eq!(bridge.pulls.lock().unwrap().len(), usize::from(numeric));
        let commands = h.worker.commands.lock().unwrap().clone();
        assert_eq!(commands.len(), usize::from(pid.is_some()) + usize::from(numeric));
        if numeric {
            assert_eq!(commands[1], ["sh", "-c", "kill -s INT +0012"]);
            assert!(
                h.logs
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|r| r.2.starts_with("Pulling finished in ") && r.2.ends_with(" "))
            );
        }
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn data_clear_disabled_avoids_identification_and_unlocated_test_fails() -> TestResult {
    for enabled in [false, true] {
        let h = harness(|v| v.data_container_clear = enabled)?;
        let d = h.device().await.map_err(|e| format!("{e:?}"))?;
        let l = DataContainerClearListener(context(&h, d.clone()));
        h.worker.commands.lock().unwrap().clear();
        assert_eq!(l.before_test_run().await.is_err(), enabled);
        assert!(h.worker.commands.lock().unwrap().is_empty());
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn unhealthy_batch_and_disposal_join_the_same_recording_stop_d96() -> TestResult {
    use marathon_core::executor::shared::SharedDeferred;
    use marathon_ios::apple_simulator_device::recording::RecordingFinalization;
    let h = harness(|_| {})?;
    let d = h.device().await?;
    let mut c = context(&h, d.clone());
    let batch = marathon_core::executor::shared::Signal::new();
    c.context.cancellation = batch.clone();
    let test = c.batch.tests[0].clone();
    let l = ScreenRecordingListener::new(c);
    let opened = SharedDeferred::new();
    let signals = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (open, seen) = (opened.clone(), signals.clone());
    *h.worker.session.lock().unwrap() = Arc::new(move |cmd| {
        if !cmd.iter().any(|arg| arg == "recordVideo") {
            return Ok(None);
        }
        let seen = seen.clone();
        let session = simulator_support::recorder_session(move |signal| {
            seen.lock().unwrap().push(signal);
            Box::pin(async move { Ok((signal == "INT").then_some(0)) })
        });
        open.complete(Ok(()));
        Ok(Some(session))
    });
    *h.worker.before.lock().unwrap() = Arc::new(|_| Box::pin(async { Ok(Some(absent())) }));
    l.test_started(&test).await.map_err(|e| format!("{e:?}"))?;
    opened.wait().await.map_err(|e| format!("{e:?}"))?;
    batch.cancel(marathon_apple::cmd::cancelled("Stop the batch"));
    let (failure, disposed) = tokio::join!(
        l.test_run_failed("failure", DeviceFailureReason::Unknown),
        d.dispose_and_join(),
    );
    failure.map_err(|e| format!("{e:?}"))?;
    disposed.map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        l.stop().await.map_err(|e| format!("{e:?}"))?,
        RecordingFinalization::Finalized
    );
    assert_eq!(*signals.lock().unwrap(), ["INT"]);
    assert!(
        !h.worker
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|cmd| cmd.iter().any(|arg| arg.ends_with(".pid")))
    );
    h.finish(&[d]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn an_unresponsive_recording_escalates_and_requires_an_exit_report_d96() -> TestResult {
    use marathon_core::executor::shared::SharedDeferred;
    use marathon_ios::apple_simulator_device::recording::RecordingFinalization;
    for acknowledged in [true, false] {
        let h = harness(|_| {})?;
        let d = h.device().await?;
        let c = context(&h, d.clone());
        let test = c.batch.tests[0].clone();
        let l = ScreenRecordingListener::new(c);
        let opened = SharedDeferred::new();
        let signals = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (open, seen) = (opened.clone(), signals.clone());
        *h.worker.session.lock().unwrap() = Arc::new(move |cmd| {
            if !cmd.iter().any(|arg| arg == "recordVideo") {
                return Ok(None);
            }
            let seen = seen.clone();
            let session = simulator_support::recorder_session(move |signal| {
                seen.lock().unwrap().push(signal);
                Box::pin(async move { Ok((acknowledged && signal == "KILL").then_some(137)) })
            });
            open.complete(Ok(()));
            Ok(Some(session))
        });
        l.test_started(&test).await.map_err(|e| format!("{e:?}"))?;
        opened.wait().await.map_err(|e| format!("{e:?}"))?;
        let (left, right) = tokio::join!(l.stop(), l.stop());
        let expected = if acknowledged {
            RecordingFinalization::Unfinalized
        } else {
            RecordingFinalization::Unconfirmed
        };
        assert_eq!(left.map_err(|e| format!("{e:?}"))?, expected);
        assert_eq!(right.map_err(|e| format!("{e:?}"))?, expected);
        d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
        assert_eq!(d.quarantined(), !acknowledged);
        assert_eq!(*signals.lock().unwrap(), ["INT", "TERM", "KILL"]);
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn batch_failure_health_swallow_and_no_inflight_test() -> TestResult {
    for mode in 0..3 {
        let h = harness(|_| {})?;
        let d = h.device().await?;
        let l = ScreenRecordingListener::new(context(&h, d.clone()));
        *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
            Box::pin(async move {
                if cmd == ["echo", "quickhealthcheck"] {
                    match mode {
                        0 => Ok(Some(ok(["quickhealthcheck"]))),
                        1 => Ok(Some(ok(["different"]))),
                        _ => Err(VendorError::Cancelled(marathon_apple::cmd::cancelled(
                            "worker cancelled",
                        ))),
                    }
                } else {
                    Ok(Some(absent()))
                }
            })
        });
        h.worker.commands.lock().unwrap().clear();
        l.test_run_failed("failure", DeviceFailureReason::Unknown)
            .await
            .map_err(|e| format!("{e:?}"))?;
        assert_eq!(h.worker.commands.lock().unwrap().len(), 1);
        assert_eq!(
            h.logs
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.2 == "Device is unhealthy, can't pull batch fail video")
                .count(),
            usize::from(mode != 0)
        );
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn data_clear_deduplicates_app_ids_in_first_seen_order() -> TestResult {
    let h = harness(|v| v.data_container_clear = true)?;
    let d = h.device().await?;
    let mut c = context(&h, d.clone());
    let mut tests = simulator_support::tests();
    tests.push(tests[0].clone());
    let app = h.dir.path().join("app.app");
    let plist = app.join("Info.plist");
    let mut bundles = vec![];
    for id in ["second", "first"] {
        std::fs::write(
            &plist,
            format!(
                "<plist><dict><key>CFBundleIdentifier</key><string>{id}</string><key>CFBundleExecutable</key><string>App</string></dict></plist>"
            ),
        )?;
        bundles.push(Arc::new(
            marathon_apple::model::AppleTestBundle::new(
                Some(app.clone()),
                None,
                h.dir.path().join("tests.xctest"),
                marathon_apple::model::Sdk::IphoneSimulator,
                h.services.runtime.clone(),
                h.services.logging.clone(),
            )
            .await
            .map_err(|e| format!("{e:?}"))?,
        ));
    }
    for (test, bundle) in tests.iter().zip(bundles.iter()) {
        h.services.identifier.put(test.clone(), bundle.clone());
    }
    c.batch = Arc::new(marathon_core::model::TestBatch {
        tests,
        id: "batch".into(),
    });
    *h.worker.before.lock().unwrap() = Arc::new(|cmd| {
        Box::pin(async move {
            Ok(Some(if cmd.iter().any(|s| s == "get_app_container") {
                ok(["/container"])
            } else {
                ok(Vec::<String>::new())
            }))
        })
    });
    h.worker.commands.lock().unwrap().clear();
    DataContainerClearListener(c)
        .before_test_run()
        .await
        .map_err(|e| format!("{e:?}"))?;
    let commands = h.worker.commands.lock().unwrap().clone();
    assert_eq!(
        commands,
        vec![
            vec!["xcrun", "simctl", "terminate", "a", "second"],
            vec!["xcrun", "simctl", "get_app_container", "a", "second", "data"],
            vec!["rm", "-rf", "/container"],
            vec!["xcrun", "simctl", "terminate", "a", "first"],
            vec!["xcrun", "simctl", "get_app_container", "a", "first", "data"],
            vec!["rm", "-rf", "/container"]
        ]
    );
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn screenshot_zero_captures_deletes_and_no_inflight_batch_failure_is_inert() -> TestResult {
    for outcome in 0..3 {
        let h = harness(|_| {})?;
        let d = h.device().await?;
        let c = context(&h, d.clone());
        let test = c.batch.tests[0].clone();
        let target = marathon_core::output::FileTarget {
            kind: marathon_core::output::FileType::Screenshot,
            pool: c.pool.clone(),
            device: marathon_core::vendor::Device::info(d.as_ref()),
            test: Some(test.clone()),
            test_batch_id: Some(c.batch.id.clone()),
            id: None,
            chunk: None,
        };
        let path = h
            .services
            .output
            .create_file(&target)
            .await
            .map_err(|e| format!("{e:?}"))?;
        std::fs::write(&path, b"existing")?;
        let l = ScreenCapturerTestRunListener::new(c);
        let collector =
            AttachmentCollector::new(&[l.attachments.clone() as Arc<dyn marathon_core::listener::AttachmentProvider>])
                .map_err(|e| format!("{e:?}"))?;
        l.test_run_failed("failure", DeviceFailureReason::Unknown)
            .await
            .map_err(|e| format!("{e:?}"))?;
        match outcome {
            0 => l.test_passed(&test, 0, 0).await,
            1 => l.test_failed(&test, 0, 0, None).await,
            _ => l.test_ignored(&test, 0, 0).await,
        }
        .map_err(|e| format!("{e:?}"))?;
        assert!(collector.get(&test).is_none());
        assert!(!path.exists());
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn unsupported_sdk_warns_twice_and_disabled_is_silent() -> TestResult {
    for enabled in [false, true] {
        let h = harness(|v| v.device_log = enabled)?;
        let bin = h.binary(h.worker.clone());
        let cycle = marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext::new(
            marathon_config::devices::Transport::Local,
            h.clock.clone(),
        );
        let record = cycle.simulator("a", &bin, false, &h.services.context).await?;
        let d = marathon_ios::apple_simulator_device::AppleSimulatorDevice::new(
            marathon_ios::device::simulator_factory::SimulatorConstruction {
                transport: marathon_config::devices::Transport::Local,
                executor: h.worker.clone(),
                commands: marathon_apple::cmd::DeviceCommands::new(h.worker.clone()),
                bridge: Arc::new(Bridge::default()),
                bin,
                record,
                sdk: marathon_apple::model::Sdk::Iphoneos,
                shared_transfers: Arc::new(marathon_apple::cmd::shared_transfers::SharedTransfers::new(
                    h.services.runtime.clone(),
                    h.services.logging.clone(),
                )),
            },
            h.services.clone(),
        );
        let l = DeviceLogListener::new(context(&h, d));
        h.worker.commands.lock().unwrap().clear();
        l.before_test_run().await.map_err(|e| format!("{e:?}"))?;
        l.after_test_run().await.map_err(|e| format!("{e:?}"))?;
        assert!(h.worker.commands.lock().unwrap().is_empty());
        assert_eq!(
            h.logs
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.2 == "Device a@local does not support capturing device logs")
                .count(),
            if enabled { 2 } else { 0 }
        );
        h.finish(&[]).await;
    }
    Ok(())
}
/// Kotlin pulls inside the test-ended callback, so a non-transfer failure of the pull stopped the composite there.
/// D80 pulls off the event path: the next listeners still see the test end, and the failure fails the run when the
/// recorder's `after_test_run` settles its videos.
#[tokio::test(start_paused = true)]
async fn non_transfer_failure_reaches_the_next_listener_and_fails_after_test_run_d80() -> TestResult {
    struct Next(Arc<AtomicUsize>);
    impl AppleTestRunListener for Next {
        fn test_failed<'a>(
            &'a self,
            _: &'a marathon_core::model::Test,
            _: i64,
            _: i64,
            _: Option<&'a str>,
        ) -> marathon_core::runtime::VendorFuture<'a, ()> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }
    let mut h = harness(|_| {})?;
    Arc::make_mut(&mut h.services.configuration).screen_recording_policy = ScreenRecordingPolicy::OnAny;
    let bridge = Arc::new(Bridge::default());
    bridge.mode.store(3, Ordering::SeqCst);
    let d = device(&h, bridge).await?;
    let c = context(&h, d.clone());
    let test = c.batch.tests[0].clone();
    let next = Arc::new(AtomicUsize::new(0));
    let recorder = Arc::new(ScreenRecordingListener::new(c));
    let composite = marathon_apple::listener::CompositeTestRunListener {
        listeners: vec![recorder.clone(), Arc::new(Next(next.clone()))],
    };
    *h.worker.before.lock().unwrap() = Arc::new(|cmd| {
        Box::pin(async move {
            Ok(Some(if cmd[0] == "cat" {
                absent()
            } else {
                ok(Vec::<String>::new())
            }))
        })
    });
    composite
        .test_failed(&test, 0, 0, None)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(next.load(Ordering::SeqCst), 1);
    assert!(recorder.after_test_run().await.is_err());
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn failed_or_null_transcode_still_pulls_and_only_failed_result_logs_error() -> TestResult {
    for null in [false, true] {
        let mut h = harness(|v| v.screen_record_configuration.video_configuration.transcoding.enabled = true)?;
        Arc::make_mut(&mut h.services.configuration).screen_recording_policy = ScreenRecordingPolicy::OnAny;
        let bridge = Arc::new(Bridge::default());
        bridge.mode.store(1, Ordering::SeqCst);
        let d = device(&h, bridge.clone()).await?;
        assert!(d.supports_transcoding());
        let c = context(&h, d.clone());
        let test = c.batch.tests[0].clone();
        let l = ScreenRecordingListener::new(c);
        let collector =
            AttachmentCollector::new(&[l.attachments.clone() as Arc<dyn marathon_core::listener::AttachmentProvider>])
                .map_err(|e| format!("{e:?}"))?;
        *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
            Box::pin(async move {
                if cmd.last().is_some_and(|s| s.contains(" -vf ")) {
                    if null {
                        return Err(VendorError::Other(
                            marathon_test_support::apple::fake_simulator_worker::connection_exception("closed"),
                        ));
                    }
                    return Ok(Some(CommandResult {
                        stdout: vec!["out".into()],
                        stderr: vec!["err".into()],
                        exit_code: Some(1),
                    }));
                }
                Ok(Some(if cmd[0] == "cat" {
                    absent()
                } else {
                    ok(Vec::<String>::new())
                }))
            })
        });
        l.test_failed(&test, 0, 0, None).await.map_err(|e| format!("{e:?}"))?;
        l.settle().await.map_err(|e| format!("{e:?}"))?;
        assert_eq!(bridge.pulls.lock().unwrap().len(), 1);
        assert!(collector.get(&test).is_some());
        assert_eq!(
            h.logs
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.2 == "Transcoding failed for /tmp/marathon/a/Tests-Suite-test1-batch.mp4: out, err")
                .count(),
            usize::from(!null)
        );
        h.finish(&[d]).await;
    }
    Ok(())
}
/// D80: every join of the recording start drops its failure, as Kotlin's cancelAndJoin does, so a start that panicked
/// on the production executor used to leave `after_test_run` successful. The panic now fails the batch.
#[test]
fn a_recording_start_that_panics_fails_after_test_run_d80() -> TestResult {
    let mut h = harness(|_| {})?;
    let outcome = marathon_core::executor::CoreExecutor::run(move |core| {
        Box::pin(async move {
            h.services.runtime = core.vendor();
            let d = device(&h, Arc::new(Bridge::default()))
                .await
                .map_err(|e| marathon_apple::cmd::failure(FailureKind::IllegalState, e.to_string()))?;
            let c = context(&h, d.clone());
            let test = c.batch.tests[0].clone();
            let l = ScreenRecordingListener::new(c);
            let reached = marathon_core::executor::shared::SharedDeferred::new();
            let starting = reached.clone();
            *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
                let starting = starting.clone();
                Box::pin(async move {
                    if cmd.iter().any(|a| a == "recordVideo") {
                        starting.complete(Ok(()));
                        panic!("recording start panicked");
                    }
                    Ok(Some(if cmd[0] == "cat" {
                        absent()
                    } else {
                        ok(Vec::<String>::new())
                    }))
                })
            });
            l.test_started(&test).await.map_err(|e| e.failure().clone())?;
            // The start has reached the command that panics; the stop below cancels and joins it.
            reached.wait().await.map_err(|e| e.failure().clone())?;
            let after = l.after_test_run().await;
            h.finish(&[d]).await;
            Ok(format!("{after:?}"))
        })
    })
    .map_err(|e| format!("{e:?}"))?;
    assert!(
        outcome.starts_with("Err") && outcome.contains("recording start panicked"),
        "{outcome}"
    );
    Ok(())
}
