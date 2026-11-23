//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/video/ScreenRecordingListener.kt
use marathon_error::FailureKind;
mod listener_support;
mod provider_support;
mod simulator_support;
mod support;
use listener_support::{Bridge, context, device, harness};
use marathon_apple::{
    apple_device::AppleDevice, cmd::CommandResult, listener::AppleTestRunListener,
    logparser::parser::DeviceFailureReason,
};
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

async fn open_video(
    h: &simulator_support::Harness,
    listener: &ScreenRecordingListener,
    test: &marathon_core::model::Test,
    exit: Option<i32>,
) -> TestResult {
    let opened = marathon_core::executor::shared::SharedDeferred::new();
    let ready = opened.clone();
    *h.worker.session.lock().unwrap() = Arc::new(move |argv| {
        if !argv.iter().any(|part| part == "recordVideo") {
            return Ok(None);
        }
        ready.complete(Ok(()));
        Ok(Some(simulator_support::recorder_session(move |_| {
            Box::pin(async move { Ok(exit) })
        })))
    });
    listener.test_started(test).await.map_err(|e| format!("{e:?}"))?;
    opened.wait().await.map_err(|e| format!("{e:?}"))?;
    Ok(())
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
async fn video_policy_gates_pulls_and_false_never_attaches_or_deletes_d97() -> TestResult {
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
            open_video(&h, &l, &test, Some(0)).await?;
            match outcome {
                0 => l.test_passed(&test, 0, 0).await.map_err(|e| format!("{e:?}"))?,
                1 => l.test_failed(&test, 0, 0, None).await.map_err(|e| format!("{e:?}"))?,
                _ => l.test_ignored(&test, 0, 0).await.map_err(|e| format!("{e:?}"))?,
            }
            // D80: the pull runs after the callback, off the event path.
            l.settle().await.map_err(|e| format!("{e:?}"))?;
            let requested = policy == ScreenRecordingPolicy::OnAny || outcome == 1;
            assert!(collector.get(&test).is_none());
            assert_eq!(bridge.pulls.lock().unwrap().len(), usize::from(requested));
            assert_eq!(
                h.worker
                    .commands
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|command| command[0] == "rm"),
                !requested
            );
            assert_eq!(
                h.logs
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|line| line.2.contains("was not delivered"))
                    .count(),
                usize::from(requested)
            );
            h.finish(&[d]).await;
        }
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_refused_legacy_launch_or_stop_costs_only_one_movie_d96_d97() -> TestResult {
    use marathon_core::executor::shared::SharedDeferred;
    use marathon_error::SshKind;
    use std::sync::atomic::AtomicBool;

    for refuse_launch in [true, false] {
        let mut h = harness(|_| {})?;
        Arc::make_mut(&mut h.services.configuration).screen_recording_policy = ScreenRecordingPolicy::OnAny;
        let bridge = Arc::new(Bridge::default());
        let d = device(&h, bridge.clone()).await?;
        h.worker.legacy_recording.store(true, Ordering::SeqCst);
        let c = context(&h, d.clone());
        let tests = c.batch.tests.clone();
        let l = ScreenRecordingListener::new(c);
        let collector =
            AttachmentCollector::new(&[l.attachments.clone() as Arc<dyn marathon_core::listener::AttachmentProvider>])
                .map_err(|e| format!("{e:?}"))?;
        let launched = [SharedDeferred::new(), SharedDeferred::new()];
        let ready = launched.clone();
        let attempts = Arc::new(AtomicUsize::new(0));
        let pid = Arc::new(AtomicBool::new(false));
        let pid_on_worker = pid.clone();
        *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
            let (pid, attempts, ready) = (pid_on_worker.clone(), attempts.clone(), ready.clone());
            Box::pin(async move {
                let wire = cmd.last().map_or("", String::as_str);
                if wire.starts_with("nohup ") {
                    let index = attempts.fetch_add(1, Ordering::SeqCst);
                    ready[index].complete(Ok(()));
                    if index == 0 && refuse_launch {
                        return Err(VendorError::Other(marathon_apple::cmd::failure(
                            FailureKind::Ssh(SshKind::OpenChannel),
                            "fixture refused recorder channel",
                        )));
                    }
                    pid.store(true, Ordering::SeqCst);
                } else if cmd[0] == "cat" {
                    return Ok(Some(if pid.load(Ordering::SeqCst) {
                        ok(["123"])
                    } else {
                        absent()
                    }));
                } else if wire.starts_with("kill -s INT") && attempts.load(Ordering::SeqCst) == 1 {
                    return Ok(Some(absent()));
                } else if wire.starts_with("ps -p") {
                    return Ok(Some(ok(["123"])));
                } else if cmd[0] == "rm" && wire.ends_with(".pid") {
                    pid.store(false, Ordering::SeqCst);
                }
                Ok(Some(ok(Vec::<String>::new())))
            })
        });
        for (index, test) in tests.iter().enumerate() {
            l.test_started(test).await.map_err(|e| format!("{e:?}"))?;
            tokio::time::timeout(std::time::Duration::from_secs(60), launched[index].wait())
                .await
                .expect("the next legacy recording did not launch")
                .map_err(|e| format!("{e:?}"))?;
            l.test_failed(test, 0, 0, None).await.map_err(|e| format!("{e:?}"))?;
            l.settle().await.map_err(|e| format!("{e:?}"))?;
            if index == 0 {
                assert!(bridge.pulls.lock().unwrap().is_empty());
                // A worker can finish a legacy recording after the failed stop.
                pid.store(false, Ordering::SeqCst);
            }
        }
        assert!(collector.get(&tests[0]).is_none());
        assert!(collector.get(&tests[1]).is_some());
        assert_eq!(bridge.pulls.lock().unwrap().len(), 1);
        assert_eq!(
            h.logs
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.2.contains("was not delivered"))
                .count(),
            1
        );
        d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
        assert!(!d.quarantined());
        h.finish(&[d]).await;
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn unfinished_movies_are_reported_once_and_never_pulled_d96_d97() -> TestResult {
    for (exit, reason) in [
        (None, "the recording did not start"),
        (Some(Some(137)), "the recording did not finalize"),
        (Some(None), "recorder cleanup could not be confirmed"),
    ] {
        let mut h = harness(|_| {})?;
        Arc::make_mut(&mut h.services.configuration).screen_recording_policy = ScreenRecordingPolicy::OnAny;
        let bridge = Arc::new(Bridge::default());
        let d = device(&h, bridge.clone()).await?;
        let c = context(&h, d.clone());
        let test = c.batch.tests[0].clone();
        let listener = ScreenRecordingListener::new(c);
        if let Some(exit) = exit {
            open_video(&h, &listener, &test, exit).await?;
        }
        listener
            .test_failed(&test, 0, 0, None)
            .await
            .map_err(|e| format!("{e:?}"))?;
        listener.after_test_run().await.map_err(|e| format!("{e:?}"))?;
        assert!(bridge.pulls.lock().unwrap().is_empty());
        let reports: Vec<_> = h
            .logs
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.2.contains("was not delivered"))
            .map(|r| r.2.clone())
            .collect();
        assert_eq!(reports.len(), 1, "{reports:?}");
        assert!(reports[0].ends_with(reason));
        h.finish(&[d]).await;
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
        open_video(&h, &l, &test, Some(0)).await?;
        l.test_failed(&test, 0, 0, None).await.map_err(|e| format!("{e:?}"))?;
        // D80: a non-transfer failure of the pull surfaces when the batch's videos are settled.
        let result = l.settle().await;
        assert_eq!(result.is_err(), mode == 3);
        assert!(!h.worker.commands.lock().unwrap().iter().any(|c| c[0] == "rm"));
        assert_eq!(
            h.logs
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.2.contains("was not delivered"))
                .count(),
            1
        );
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn log_disabled_no_pid_and_failed_pull_reports_undelivered_d97() -> TestResult {
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
                !h.logs
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|r| r.2.starts_with("Pulling finished in ") && r.2.ends_with(" "))
            );
        }
        assert_eq!(
            h.logs
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.2.contains("was not delivered"))
                .count(),
            usize::from(numeric)
        );
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
        RecordingFinalization::NotStarted
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
        assert_eq!(right.map_err(|e| format!("{e:?}"))?, RecordingFinalization::NotStarted);
        d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
        assert_eq!(d.quarantined(), !acknowledged);
        assert_eq!(*signals.lock().unwrap(), ["INT", "TERM", "KILL"]);
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn healthy_recording_gets_the_configured_shell_time_to_finalize_d96() -> TestResult {
    use marathon_core::executor::shared::SharedDeferred;
    use marathon_ios::apple_simulator_device::recording::RecordingFinalization;
    let h = harness(|vendor| vendor.timeout_configuration.shell = support::duration(30_000))?;
    let d = h.device().await?;
    let c = context(&h, d.clone());
    let test = c.batch.tests[0].clone();
    let listener = ScreenRecordingListener::new(c);
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
            Box::pin(async move {
                if signal == "INT" {
                    tokio::time::sleep(std::time::Duration::from_secs(24)).await;
                    Ok(Some(0))
                } else {
                    Ok(Some(137))
                }
            })
        });
        open.complete(Ok(()));
        Ok(Some(session))
    });
    listener.test_started(&test).await.map_err(|e| format!("{e:?}"))?;
    opened.wait().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        listener.stop().await.map_err(|e| format!("{e:?}"))?,
        RecordingFinalization::Finalized
    );
    assert_eq!(*signals.lock().unwrap(), ["INT"]);
    d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
    // The listener chain can still see an event after disposal has sealed admission.
    listener.test_started(&test).await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        listener.stop().await.map_err(|e| format!("{e:?}"))?,
        RecordingFinalization::NotStarted
    );
    h.finish(&[d]).await;
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
    open_video(&h, &recorder, &test, Some(0)).await?;
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
async fn failed_or_null_transcode_requests_the_original_movie_d97() -> TestResult {
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
        open_video(&h, &l, &test, Some(0)).await?;
        l.test_failed(&test, 0, 0, None).await.map_err(|e| format!("{e:?}"))?;
        l.settle().await.map_err(|e| format!("{e:?}"))?;
        assert_eq!(bridge.pulls.lock().unwrap().len(), 1);
        assert!(collector.get(&test).is_none());
        assert_eq!(
            bridge.pulls.lock().unwrap().as_slice(),
            &[d.remote_file_manager().remote_temp_video_for_test(&test, "batch")]
        );
        let src = d.remote_file_manager().remote_video_for_test(&test, "batch");
        {
            let commands = h.worker.commands.lock().unwrap();
            let transcode = commands
                .iter()
                .position(|cmd| cmd.last().is_some_and(|s| s.contains(" -vf ")))
                .unwrap();
            assert!(commands[transcode + 1..].iter().any(|cmd| cmd == &["rm", "-rf", &src]));
        }
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
/// U267: where a movie can reach the report.
#[derive(Clone, Copy, Debug, PartialEq)]
enum MoviePath {
    /// Pulled as recorded, transcoding off.
    Original,
    /// The transcode failed, so the original is pulled from its staged name.
    TranscodeFailed,
    /// The transcoded movie is pulled.
    Transcoded,
    /// The batch failed with the recording open: its movie is pulled without a test.
    BatchFailure,
}
/// The pulled movie, the "was not delivered" lines, the test's video attachment, and every file under the report root
/// apart from the harness's own bundles.
type MovieOutcome = (String, Vec<String>, Option<Vec<u8>>, Vec<String>);

async fn deliver_one_movie(path: MoviePath, received: &[u8]) -> TestResult<MovieOutcome> {
    let transcoding = matches!(path, MoviePath::TranscodeFailed | MoviePath::Transcoded);
    let mut h = harness(|v| v.screen_record_configuration.video_configuration.transcoding.enabled = transcoding)?;
    Arc::make_mut(&mut h.services.configuration).screen_recording_policy = ScreenRecordingPolicy::OnAny;
    let bridge = Arc::new(Bridge::default());
    let d = device(&h, bridge.clone()).await?;
    assert_eq!(d.supports_transcoding(), transcoding);
    let c = context(&h, d.clone());
    let test = c.batch.tests[0].clone();
    let src = d.remote_file_manager().remote_video_for_test(&test, "batch").to_owned();
    let pulled = match path {
        MoviePath::TranscodeFailed => d.remote_file_manager().remote_temp_video_for_test(&test, "batch"),
        _ => src.clone(),
    };
    bridge
        .contents
        .lock()
        .unwrap()
        .insert(pulled.clone(), received.to_vec());
    let l = ScreenRecordingListener::new(c);
    let collector =
        AttachmentCollector::new(&[l.attachments.clone() as Arc<dyn marathon_core::listener::AttachmentProvider>])
            .map_err(|e| format!("{e:?}"))?;
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        Box::pin(async move {
            Ok(Some(if cmd.last().is_some_and(|s| s.contains(" -vf ")) {
                CommandResult {
                    stdout: vec![],
                    stderr: vec!["moov atom not found".into()],
                    exit_code: Some(if path == MoviePath::TranscodeFailed { 1 } else { 0 }),
                }
            } else if cmd[0] == "cat" {
                absent()
            } else if cmd[0] == "echo" {
                ok(["quickhealthcheck"])
            } else {
                ok(Vec::<String>::new())
            }))
        })
    });
    open_video(&h, &l, &test, Some(0)).await?;
    if path == MoviePath::BatchFailure {
        l.test_run_failed("failure", DeviceFailureReason::Unknown)
            .await
            .map_err(|e| format!("{e:?}"))?;
    } else {
        l.test_failed(&test, 0, 0, None).await.map_err(|e| format!("{e:?}"))?;
    }
    l.settle().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(*bridge.pulls.lock().unwrap(), std::slice::from_ref(&pulled));
    let removed = h
        .worker
        .commands
        .lock()
        .unwrap()
        .iter()
        .any(|cmd| cmd == &["rm", "-rf", &pulled]);
    let undelivered: Vec<String> = h
        .logs
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.2.contains("was not delivered"))
        .map(|r| format!("{} {}", r.1, r.2))
        .collect();
    // D97: the worker's copy is removed once published, and kept for inspection when it is not.
    assert_eq!(removed, undelivered.is_empty(), "{undelivered:?}");
    let attachment = match collector.get(&test).unwrap_or_default().as_slice() {
        [] => None,
        [video] => {
            assert_eq!(video.kind, marathon_core::model::AttachmentType::Video);
            Some(std::fs::read(&video.file)?)
        }
        more => return Err(format!("{more:?}").into()),
    };
    let mut files = Vec::new();
    let mut dirs = vec![h.dir.path().to_owned()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if ["app.app", "tests.xctest"].contains(&name.as_str()) {
                continue;
            }
            if entry.file_type()?.is_dir() && !name.starts_with('.') {
                dirs.push(entry.path());
            } else {
                files.push(name);
            }
        }
    }
    h.finish(&[d]).await;
    Ok((src, undelivered, attachment, files))
}

#[tokio::test(start_paused = true)]
async fn a_failed_transcode_publishes_the_original_only_when_it_is_complete_u267() -> TestResult {
    let unfinished = &listener_support::MOVIE[..listener_support::MOVIE.len() - 1];
    for (received, reason) in [
        (&b""[..], Some("the recording is empty")),
        (
            unfinished,
            Some("the recording is not a complete movie (it is cut short)"),
        ),
        (listener_support::MOVIE, None),
    ] {
        let (src, undelivered, attachment, files) = deliver_one_movie(MoviePath::TranscodeFailed, received).await?;
        match reason {
            Some(reason) => {
                assert_eq!(undelivered, [format!("ERROR Video {src} was not delivered: {reason}")]);
                assert_eq!(attachment, None);
                assert!(files.is_empty(), "{files:?}");
            }
            None => {
                assert!(undelivered.is_empty(), "{undelivered:?}");
                assert_eq!(attachment.as_deref(), Some(listener_support::MOVIE));
                assert_eq!(files, ["Tests.Suite#test1-batch.mp4"]);
            }
        }
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn every_pull_of_a_movie_publishes_only_a_complete_one_u267() -> TestResult {
    for path in [MoviePath::Original, MoviePath::Transcoded, MoviePath::BatchFailure] {
        let (src, undelivered, attachment, files) = deliver_one_movie(path, b"").await?;
        assert_eq!(
            undelivered,
            [format!("ERROR Video {src} was not delivered: the recording is empty")],
            "{path:?}"
        );
        assert_eq!(attachment, None, "{path:?}");
        assert!(files.is_empty(), "{path:?} {files:?}");

        let (_, undelivered, attachment, files) = deliver_one_movie(path, listener_support::MOVIE).await?;
        assert!(undelivered.is_empty(), "{path:?} {undelivered:?}");
        // The batch's own video has no test to attach to.
        let attached = (path != MoviePath::BatchFailure).then_some(listener_support::MOVIE);
        assert_eq!(attachment.as_deref(), attached, "{path:?}");
        let published = if path == MoviePath::BatchFailure {
            "batch.batch"
        } else {
            "Tests.Suite#test1-batch.mp4"
        };
        assert_eq!(files, [published], "{path:?}");
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

type Signals = Arc<std::sync::Mutex<Vec<(&'static str, tokio::time::Instant)>>>;

/// D98: installs a recorder that has not printed simctl's start marker; returns its stderr and the signals it gets.
fn unconfirmed_recorder(
    h: &simulator_support::Harness,
    exit: impl Fn(&'static str) -> Option<i32> + Send + Sync + Copy + 'static,
) -> (
    Arc<marathon_core::executor::shared::SharedDeferred<marathon_apple::cmd::channel::LineChannel>>,
    Signals,
) {
    let printed = marathon_core::executor::shared::SharedDeferred::new();
    let signals: Signals = Arc::default();
    let (stderr, seen) = (printed.clone(), signals.clone());
    *h.worker.session.lock().unwrap() = Arc::new(move |cmd| {
        if !cmd.iter().any(|arg| arg == "recordVideo") {
            return Ok(None);
        }
        let seen = seen.clone();
        let (session, channel) = simulator_support::unconfirmed_recorder_session(move |signal| {
            seen.lock().unwrap().push((signal, tokio::time::Instant::now()));
            Box::pin(async move { Ok(exit(signal)) })
        });
        stderr.complete(Ok(channel));
        Ok(Some(session))
    });
    (printed, signals)
}

#[tokio::test(start_paused = true)]
async fn a_stop_before_the_start_marker_holds_int_until_the_marker_d98() -> TestResult {
    use marathon_ios::apple_simulator_device::recording::RecordingFinalization;
    let h = harness(|_| {})?;
    let d = h.device().await?;
    let c = context(&h, d.clone());
    let test = c.batch.tests[0].clone();
    let l = ScreenRecordingListener::new(c);
    let (stderr, signals) = unconfirmed_recorder(&h, |signal| (signal == "INT").then_some(0));
    l.test_started(&test).await.map_err(|e| format!("{e:?}"))?;
    let stderr = stderr.wait().await.map_err(|e| format!("{e:?}"))?;
    let requested = tokio::time::Instant::now();
    let script = async {
        // A slow simulator: the marker comes 5 s after the stop, inside the INT window.
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        assert!(signals.lock().unwrap().is_empty(), "INT before the start marker");
        stderr.send("Note: No display specified.".into()).await?;
        stderr.send("Recording started".into()).await?;
        Ok::<_, VendorError>(tokio::time::Instant::now())
    };
    let (stopped, marked) = tokio::join!(l.stop(), script);
    assert_eq!(stopped.map_err(|e| format!("{e:?}"))?, RecordingFinalization::Finalized);
    let marked = marked.map_err(|e| format!("{e:?}"))?;
    assert_eq!(marked - requested, std::time::Duration::from_secs(5));
    assert_eq!(
        *signals.lock().unwrap(),
        [("INT", marked)],
        "INT follows the marker at once"
    );
    assert_eq!(
        h.logs
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.2.starts_with("started video recording"))
            .count(),
        1
    );
    d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
    h.finish(&[d]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_recorder_without_a_start_marker_gets_term_and_kill_at_the_existing_cutoffs_d98() -> TestResult {
    use marathon_ios::apple_simulator_device::recording::RecordingFinalization;
    for ignore_term in [false, true] {
        let h = harness(|vendor| vendor.timeout_configuration.shell = support::duration(30_000))?;
        let d = h.device().await?;
        let c = context(&h, d.clone());
        let test = c.batch.tests[0].clone();
        let l = ScreenRecordingListener::new(c);
        let (stderr, signals) = if ignore_term {
            unconfirmed_recorder(&h, |signal| (signal == "KILL").then_some(137))
        } else {
            unconfirmed_recorder(&h, |signal| (signal == "TERM").then_some(143))
        };
        l.test_started(&test).await.map_err(|e| format!("{e:?}"))?;
        // The recorder has written only its display note; it never confirms its start.
        let stderr = stderr.wait().await.map_err(|e| format!("{e:?}"))?;
        stderr
            .send("Note: No display specified.".into())
            .await
            .map_err(|e| format!("{e:?}"))?;
        let requested = tokio::time::Instant::now();
        assert_eq!(
            l.stop().await.map_err(|e| format!("{e:?}"))?,
            RecordingFinalization::Unfinalized
        );
        // A healthy stop: INT window max(shell, 15 s) = 30 s, then 5 s for TERM and KILL, KILL at its midpoint.
        let seen: Vec<_> = signals
            .lock()
            .unwrap()
            .iter()
            .map(|(signal, at)| (*signal, *at - requested))
            .collect();
        let seconds = std::time::Duration::from_millis;
        if ignore_term {
            assert_eq!(seen, [("TERM", seconds(30_000)), ("KILL", seconds(32_500))]);
        } else {
            assert_eq!(seen, [("TERM", seconds(30_000))]);
        }
        d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
        assert!(!d.quarantined());
        assert!(
            !h.logs
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.2.starts_with("started video recording"))
        );
        h.finish(&[d]).await;
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_confirmed_recorder_gets_int_as_soon_as_its_stop_is_requested_d98() -> TestResult {
    use marathon_ios::apple_simulator_device::recording::RecordingFinalization;
    let h = harness(|_| {})?;
    let d = h.device().await?;
    let c = context(&h, d.clone());
    let test = c.batch.tests[0].clone();
    let l = ScreenRecordingListener::new(c);
    let (stderr, signals) = unconfirmed_recorder(&h, |signal| (signal == "INT").then_some(0));
    l.test_started(&test).await.map_err(|e| format!("{e:?}"))?;
    let stderr = stderr.wait().await.map_err(|e| format!("{e:?}"))?;
    stderr
        .send("Recording started".into())
        .await
        .map_err(|e| format!("{e:?}"))?;
    // The test runs for a while after the recorder confirmed its start.
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let requested = tokio::time::Instant::now();
    assert_eq!(
        l.stop().await.map_err(|e| format!("{e:?}"))?,
        RecordingFinalization::Finalized
    );
    assert_eq!(*signals.lock().unwrap(), [("INT", requested)]);
    d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
    h.finish(&[d]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_busy_video_service_turns_video_off_for_the_simulator_once_d98() -> TestResult {
    use marathon_ios::apple_simulator_device::recording::RecordingFinalization;
    // simctl on a simulator whose video service is still held by an earlier, unstoppable recording (before Xcode 26.6
    // and, U262, as Xcode 26.6 words it), and an exit status 16 for some other reason, which leaves video on.
    // U264: on a local simulator and on a supervised worker (stderr from the exit receipt) alike.
    for (busy, reason, supervised) in [
        (true, "A recording is already in progress", false),
        (true, "Host recording is already in progress", false),
        (false, "", false),
        (true, "Host recording is already in progress", true),
        (false, "", true),
    ] {
        let mut h = harness(|_| {})?;
        Arc::make_mut(&mut h.services.configuration).screen_recording_policy = ScreenRecordingPolicy::OnAny;
        let d = h.device().await?;
        let c = context(&h, d.clone());
        let tests = c.batch.tests.clone();
        let l = ScreenRecordingListener::new(c);
        let launched = marathon_core::executor::shared::SharedDeferred::new();
        let launch = launched.clone();
        *h.worker.session.lock().unwrap() = Arc::new(move |cmd| {
            if !cmd.iter().any(|arg| arg == "recordVideo") {
                return Ok(None);
            }
            launch.complete(Ok(()));
            let lines: Vec<String> = if busy {
                vec![
                    format!(
                        "Error starting video recorder: Error Domain=NSPOSIXErrorDomain Code=16 \"Resource busy\" \
                         UserInfo={{NSLocalizedFailureReason={reason}}}."
                    ),
                    "An error was encountered processing the command (domain=NSPOSIXErrorDomain, code=16):".into(),
                    reason.into(),
                ]
            } else {
                vec!["An error was encountered processing the command (domain=NSPOSIXErrorDomain, code=16):".into()]
            };
            let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
            Ok(Some(exiting_recorder(supervised, 16, &lines)))
        });
        let recordings = |h: &simulator_support::Harness| {
            h.worker
                .commands
                .lock()
                .unwrap()
                .iter()
                .filter(|cmd| cmd.iter().any(|arg| arg == "recordVideo"))
                .count()
        };
        for (index, test) in tests.iter().enumerate() {
            l.test_started(test).await.map_err(|e| format!("{e:?}"))?;
            if index == 0 {
                launched.wait().await.map_err(|e| format!("{e:?}"))?;
            } else {
                // Paused time: every runnable task, a launch included, runs before this returns.
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            l.test_failed(test, 0, 0, None).await.map_err(|e| format!("{e:?}"))?;
        }
        l.settle().await.map_err(|e| format!("{e:?}"))?;
        let logs = h.logs.lock().unwrap().clone();
        let warnings: Vec<_> = logs
            .iter()
            .filter(|r| r.1 == marathon_logging::Level::WARN && r.2.contains("video service is busy"))
            .inspect(|r| assert_eq!(r.0, BUSY_VIDEO_TARGET, "{r:?}"))
            .map(|r| r.2.clone())
            .collect();
        let undelivered: Vec<_> = logs
            .iter()
            .filter(|r| r.2.contains("was not delivered"))
            .map(|r| r.2.clone())
            .collect();
        assert!(!logs.iter().any(|r| r.2 == "Video recording did not start"));
        // U264: one line per refused recorder, and the busy warning is the only one.
        let lines = recorder_lines(&h);
        if busy {
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert_eq!(lines[0].0, marathon_logging::Level::WARN);
        } else {
            assert_eq!(lines.len(), 2, "{lines:?}");
            for (level, message) in &lines {
                assert_eq!(*level, marathon_logging::Level::WARN, "{message}");
                assert!(message.contains("failed (status 16). stderr: "), "{message}");
                assert!(!message.contains("restart"), "{message}");
            }
        }
        if busy {
            assert_eq!(recordings(&h), 1, "no recorder is launched after the busy refusal");
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert!(warnings[0].contains(d.udid()) && warnings[0].contains("Restart the simulator"));
            // The failed start still reports its own movie; the skipped one reports nothing per test.
            assert_eq!(undelivered.len(), 1, "{undelivered:?}");
            assert!(undelivered[0].ends_with("the recording did not finalize"));
            l.test_started(&tests[0]).await.map_err(|e| format!("{e:?}"))?;
            assert_eq!(
                l.stop().await.map_err(|e| format!("{e:?}"))?,
                RecordingFinalization::Skipped
            );
        } else {
            assert_eq!(recordings(&h), 2, "a bare status 16 leaves video on");
            assert!(warnings.is_empty(), "{warnings:?}");
            assert_eq!(undelivered.len(), 2, "{undelivered:?}");
        }
        d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
        h.finish(&[d]).await;
    }
    Ok(())
}

/// U264: a recorder that exits at once with `status` and `stderr`. A local recorder's stderr comes on its stream; a
/// supervised worker's comes with the exit receipt, set before the exit status, as the supervisor publishes it.
fn exiting_recorder(supervised: bool, status: i32, stderr: &[&str]) -> marathon_apple::cmd::CommandSession {
    exiting_recorder_after(supervised, status, stderr, async {})
}

/// `exiting_recorder` whose exit comes only once `after` has completed.
fn exiting_recorder_after(
    supervised: bool,
    status: i32,
    stderr: &[&str],
    after: impl std::future::Future<Output = ()> + Send + 'static,
) -> marathon_apple::cmd::CommandSession {
    let (session, channel) = simulator_support::unconfirmed_recorder_session(|_| Box::pin(async { Ok(None) }));
    let receipt = Arc::new(std::sync::OnceLock::new());
    let (started, report) = tokio::sync::watch::channel(false);
    let session = if supervised {
        session.with_worker_report(marathon_apple::cmd::base_command::WorkerReport {
            started: report,
            stderr: receipt.clone(),
            token: format!("{:032x}", 0x5eed_u128),
        })
    } else {
        session
    };
    let lines: Vec<String> = stderr.iter().map(|line| (*line).to_owned()).collect();
    let (exit, stdout) = (session.base.exit_code.clone(), session.base.stdout.clone());
    tokio::spawn(async move {
        after.await;
        if supervised {
            let _ = receipt.set(lines.iter().map(|line| format!("{line}\n")).collect::<String>());
        } else {
            for line in lines {
                channel.send(line).await.unwrap();
            }
        }
        stdout.close(None);
        channel.close(None);
        exit.complete(Ok(Some(status)));
        drop(started);
    });
    session
}

const HOST_BUSY: [&str; 3] = [
    "Error starting video recorder: Error Domain=NSPOSIXErrorDomain Code=16 \"Resource busy\" \
     UserInfo={NSLocalizedFailureReason=Host recording is already in progress}.",
    "An error was encountered processing the command (domain=NSPOSIXErrorDomain, code=16):",
    "Host recording is already in progress",
];

/// Every line on D98's target (U260), which shows with `debug: false`: (level, message).
fn recorder_lines(h: &simulator_support::Harness) -> Vec<(marathon_logging::Level, String)> {
    h.logs
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.0 == BUSY_VIDEO_TARGET)
        .map(|r| (r.1, r.2.clone()))
        .collect()
}

/// U265, U266: a simulator video service as simctl runs it: a recorder launched while another one there is live is
/// refused with status 16 and the busy text (at once, or only once the live one has ended: `late_refusal`). The first
/// recorder stays starting (no start marker yet) until the test releases it; it then exits 0 on INT. Every later
/// recorder that finds the service free starts and exits 0 on INT.
struct VideoService {
    launches: AtomicUsize,
    live: AtomicUsize,
    /// The launch indices of the recorders that finalized (exit 0 on INT).
    finalized: std::sync::Mutex<Vec<usize>>,
    first: Arc<marathon_core::executor::shared::SharedDeferred<marathon_apple::cmd::channel::LineChannel>>,
    started: tokio::sync::watch::Sender<bool>,
    release: Arc<marathon_core::executor::shared::SharedDeferred<()>>,
    first_ended: Arc<marathon_core::executor::shared::SharedDeferred<()>>,
}

impl VideoService {
    fn install(h: &simulator_support::Harness, supervised: bool, late_refusal: bool) -> Arc<Self> {
        let service = Arc::new(Self {
            launches: AtomicUsize::new(0),
            live: AtomicUsize::new(0),
            finalized: std::sync::Mutex::default(),
            first: marathon_core::executor::shared::SharedDeferred::new(),
            started: tokio::sync::watch::channel(false).0,
            release: marathon_core::executor::shared::SharedDeferred::new(),
            first_ended: marathon_core::executor::shared::SharedDeferred::new(),
        });
        let installed = service.clone();
        *h.worker.session.lock().unwrap() = Arc::new(move |cmd| {
            if !cmd.iter().any(|arg| arg == "recordVideo") {
                return Ok(None);
            }
            let service = installed.clone();
            let index = service.launches.fetch_add(1, Ordering::SeqCst);
            if service.live.load(Ordering::SeqCst) > 0 {
                let ended = service.first_ended.clone();
                return Ok(Some(if late_refusal {
                    exiting_recorder_after(supervised, 16, &HOST_BUSY, async move {
                        let _ = ended.wait().await;
                    })
                } else {
                    exiting_recorder(supervised, 16, &HOST_BUSY)
                }));
            }
            service.live.fetch_add(1, Ordering::SeqCst);
            let interrupted = service.clone();
            let (session, stderr) = simulator_support::unconfirmed_recorder_session(move |signal| {
                let service = interrupted.clone();
                Box::pin(async move {
                    if signal != "INT" {
                        return Ok(None);
                    }
                    // A supervised worker holds INT until the start marker; the first recorder starts when released.
                    let _ = service.release.wait().await;
                    service.live.fetch_sub(1, Ordering::SeqCst);
                    service.finalized.lock().unwrap().push(index);
                    if index == 0 {
                        service.first_ended.complete(Ok(()));
                    }
                    Ok(Some(0))
                })
            });
            if index == 0 {
                service.first.complete(Ok(stderr.clone()));
            } else {
                use futures_util::FutureExt;
                assert!(stderr.send("Recording started".into()).now_or_never().is_some());
            }
            Ok(Some(if supervised {
                let started = if index == 0 {
                    service.started.subscribe()
                } else {
                    tokio::sync::watch::channel(true).1
                };
                session.with_worker_report(marathon_apple::cmd::base_command::WorkerReport {
                    started,
                    stderr: Arc::default(),
                    token: format!("{index:032x}"),
                })
            } else {
                session
            }))
        });
        service
    }

    /// The first recorder prints its start marker (or its worker reports it) and may now finalize.
    async fn start_first(&self) -> TestResult {
        let stderr = self.first.wait().await.map_err(|e| format!("{e:?}"))?;
        self.release.complete(Ok(()));
        self.started.send_replace(true);
        stderr
            .send("Recording started".into())
            .await
            .map_err(|e| format!("{e:?}"))?;
        Ok(())
    }
}

/// U265, U266: one listener, as in production; the first recorder is still starting while three very short tests
/// end, and then a normal-length test runs.
async fn short_tests_behind_a_starting_recorder(supervised: bool, late_refusal: bool) -> TestResult {
    use marathon_ios::apple_simulator_device::recording::RecordingFinalization;
    let h = harness(|_| {})?;
    set_debug(&h, false);
    let d = h.device().await?;
    let c = context(&h, d.clone());
    let tests: Vec<_> = (1..=5)
        .map(|n| marathon_core::model::Test {
            pkg: "Tests".into(),
            clazz: "Suite".into(),
            method: format!("short{n}"),
            meta_properties: vec![],
        })
        .collect();
    let l = ScreenRecordingListener::new(c);
    let service = VideoService::install(&h, supervised, late_refusal);
    l.test_started(&tests[0]).await.map_err(|e| format!("{e:?}"))?;
    service.first.wait().await.map_err(|e| format!("{e:?}"))?;
    l.test_ignored(&tests[0], 0, 0).await.map_err(|e| format!("{e:?}"))?;
    for test in &tests[1..4] {
        l.test_started(test).await.map_err(|e| format!("{e:?}"))?;
        // Paused time: every runnable task, a launch included, runs before this returns.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        l.test_ignored(test, 0, 0).await.map_err(|e| format!("{e:?}"))?;
    }
    // A normal-length test: its recorder waits for the first one, which then starts, gets INT and finalizes.
    l.test_started(&tests[4]).await.map_err(|e| format!("{e:?}"))?;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let beside = service.launches.load(Ordering::SeqCst) - 1;
    service.start_first().await?;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let stopped = l.stop().await.map_err(|e| format!("{e:?}"))?;
    l.settle().await.map_err(|e| format!("{e:?}"))?;
    // U266 first, then U265: what each costs a user; then their common cause.
    let busy = busy_warnings(&h);
    assert!(busy.is_empty(), "D98 turned video off: {busy:?}");
    assert_eq!(
        stopped,
        RecordingFinalization::Finalized,
        "the normal-length test has its video"
    );
    let lines = recorder_lines(&h);
    assert!(lines.is_empty(), "{lines:?}");
    assert_eq!(beside, 0, "no recorder beside the first one");
    assert_eq!(service.launches.load(Ordering::SeqCst), 2);
    assert_eq!(*service.finalized.lock().unwrap(), [0, 1]);
    // Video stays on for the next test.
    l.test_started(&tests[0]).await.map_err(|e| format!("{e:?}"))?;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(service.launches.load(Ordering::SeqCst), 3, "video stays on");
    assert_eq!(
        l.stop().await.map_err(|e| format!("{e:?}"))?,
        RecordingFinalization::Finalized
    );
    assert!(recorder_lines(&h).is_empty(), "{:?}", recorder_lines(&h));
    d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
    h.finish(&[d]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn short_tests_never_launch_a_recorder_beside_marathon_s_starting_one_u265() -> TestResult {
    for supervised in [false, true] {
        short_tests_behind_a_starting_recorder(supervised, false).await?;
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_refusal_seen_after_marathon_s_own_recorder_ended_cannot_turn_video_off_u266() -> TestResult {
    for supervised in [false, true] {
        short_tests_behind_a_starting_recorder(supervised, true).await?;
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_recorder_marathon_stops_waiting_for_is_one_warning_u264() -> TestResult {
    use marathon_ios::apple_simulator_device::recording::RecordingFinalization;
    // A recorder whose launch never confirms (local), and a started one that never exits, whatever it is sent (local
    // and supervised): a supervised worker's receipt, if one came now, would be logged only at DEBUG.
    for (launch_hangs, supervised) in [(true, false), (false, false), (false, true)] {
        let h = harness(|_| {})?;
        set_debug(&h, false);
        let d = h.device().await?;
        let c = context(&h, d.clone());
        let test = c.batch.tests[0].clone();
        let l = ScreenRecordingListener::new(c);
        if launch_hangs {
            *h.worker.before.lock().unwrap() = Arc::new(|cmd| {
                Box::pin(async move {
                    if cmd.iter().any(|arg| arg == "recordVideo") {
                        std::future::pending::<()>().await;
                    }
                    Ok(Some(ok(Vec::<String>::new())))
                })
            });
        } else {
            *h.worker.session.lock().unwrap() = Arc::new(move |cmd| {
                if !cmd.iter().any(|arg| arg == "recordVideo") {
                    return Ok(None);
                }
                let session = simulator_support::recorder_session(|_| Box::pin(async { Ok(None) }));
                Ok(Some(if supervised {
                    session.with_worker_report(marathon_apple::cmd::base_command::WorkerReport {
                        started: tokio::sync::watch::channel(true).1,
                        stderr: Arc::default(),
                        token: format!("{:032x}", 0xfeed_u128),
                    })
                } else {
                    session
                }))
            });
        }
        l.test_started(&test).await.map_err(|e| format!("{e:?}"))?;
        tokio::time::sleep(std::time::Duration::from_secs(if launch_hangs { 301 } else { 1 })).await;
        assert_eq!(
            l.stop().await.map_err(|e| format!("{e:?}"))?,
            RecordingFinalization::Unconfirmed
        );
        let lines = recorder_lines(&h);
        assert_eq!(lines.len(), 1, "{lines:?}");
        let (level, message) = &lines[0];
        assert_eq!(*level, marathon_logging::Level::WARN, "{message}");
        assert!(message.contains(d.serial()), "{message}");
        if launch_hangs {
            assert!(
                message.contains("did not confirm its launch in time; marathon stopped waiting for it."),
                "{message}"
            );
        } else {
            assert!(
                message.contains("did not report how it ended; marathon stopped waiting for it."),
                "{message}"
            );
            assert_eq!(message.contains("(worker recorder 00000000)"), supervised, "{message}");
        }
        d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
        h.finish(&[d]).await;
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn any_other_recorder_failure_is_one_warning_with_its_stderr_u264() -> TestResult {
    // Another refusal; and the busy text with a status other than 16, the only way the restart advice remains.
    for (status, stderr, advice) in [(1, &["Invalid device: fixture"][..], false), (1, &HOST_BUSY[..], true)] {
        for supervised in [false, true] {
            let h = harness(|_| {})?;
            set_debug(&h, false);
            let d = h.device().await?;
            let c = context(&h, d.clone());
            let tests = c.batch.tests.clone();
            let l = ScreenRecordingListener::new(c);
            let launches = Arc::new(AtomicUsize::new(0));
            let counted = launches.clone();
            let lines: Vec<&'static str> = stderr.to_vec();
            *h.worker.session.lock().unwrap() = Arc::new(move |cmd| {
                if !cmd.iter().any(|arg| arg == "recordVideo") {
                    return Ok(None);
                }
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(Some(exiting_recorder(supervised, status, &lines)))
            });
            for test in &tests {
                l.test_started(test).await.map_err(|e| format!("{e:?}"))?;
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                l.test_failed(test, 0, 0, None).await.map_err(|e| format!("{e:?}"))?;
            }
            l.settle().await.map_err(|e| format!("{e:?}"))?;
            assert_eq!(launches.load(Ordering::SeqCst), tests.len(), "video stays on");
            let warnings = recorder_lines(&h);
            assert_eq!(warnings.len(), tests.len(), "one line per recorder: {warnings:?}");
            for (level, message) in &warnings {
                assert_eq!(*level, marathon_logging::Level::WARN, "{message}");
                assert!(message.contains(d.serial()), "{message}");
                assert!(message.contains("failed (status 1). stderr: "), "{message}");
                assert!(message.contains(stderr[stderr.len() - 1]), "{message}");
                assert_eq!(message.contains("restart the simulator"), advice, "{message}");
            }
            d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
            h.finish(&[d]).await;
        }
    }
    Ok(())
}

/// D98: a booted simulator whose first recording met a busy video service.
async fn simulator_with_video_off(
    edit: impl FnOnce(&mut IosConfiguration),
) -> TestResult<(
    simulator_support::Harness,
    Arc<marathon_ios::apple_simulator_device::AppleSimulatorDevice>,
)> {
    let h = harness(edit)?;
    let d = h.device().await?;
    turn_video_off(&h, &d).await?;
    Ok((h, d))
}

/// D98: `d`'s first recording meets a busy video service; then the simulator is booted, as after its preparation.
/// From here on the run logs with `debug: false`, as on a CI farm: D98's own lines must still show (U260).
async fn turn_video_off(
    h: &simulator_support::Harness,
    d: &Arc<marathon_ios::apple_simulator_device::AppleSimulatorDevice>,
) -> TestResult {
    set_debug(h, false);
    let c = context(h, d.clone());
    let test = c.batch.tests[0].clone();
    let l = ScreenRecordingListener::new(c);
    let launched = marathon_core::executor::shared::SharedDeferred::new();
    let launch = launched.clone();
    *h.worker.session.lock().unwrap() = Arc::new(move |cmd| {
        if !cmd.iter().any(|arg| arg == "recordVideo") {
            return Ok(None);
        }
        launch.complete(Ok(()));
        let (session, stderr) = simulator_support::unconfirmed_recorder_session(|_| Box::pin(async { Ok(None) }));
        let (exit, stdout) = (session.base.exit_code.clone(), session.base.stdout.clone());
        tokio::spawn(async move {
            stderr.send("A recording is already in progress".into()).await.unwrap();
            stdout.close(None);
            stderr.close(None);
            exit.complete(Ok(Some(16)));
        });
        Ok(Some(session))
    });
    l.test_started(&test).await.map_err(|e| format!("{e:?}"))?;
    launched.wait().await.map_err(|e| format!("{e:?}"))?;
    l.test_passed(&test, 0, 0).await.map_err(|e| format!("{e:?}"))?;
    l.settle().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(busy_warnings(h).len(), 1, "video is off");
    h.boot.state.store(3, Ordering::SeqCst);
    Ok(())
}

/// U260: D98's lines have their own target, which `marathon_logging::filter` keeps visible with debug off.
const BUSY_VIDEO_TARGET: &str = "marathon_ios::busy_video";

fn set_debug(h: &simulator_support::Harness, debug: bool) {
    h.services
        .logging
        .registry
        .lock()
        .unwrap()
        .configure(debug, marathon_logging::Vendor::Apple { compact_output: false });
}

fn busy_warnings(h: &simulator_support::Harness) -> Vec<String> {
    h.logs
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.1 == marathon_logging::Level::WARN && r.2.contains("video service is busy with a recording"))
        .inspect(|r| assert_eq!(r.0, BUSY_VIDEO_TARGET, "{r:?}"))
        .map(|r| r.2.clone())
        .collect()
}

/// The `recordVideo` launches and the `simctl shutdown` commands sent for the simulator `udid`.
fn video_commands(h: &simulator_support::Harness, udid: &str) -> (usize, usize) {
    let commands = h.worker.commands.lock().unwrap();
    let ours = |cmd: &&Vec<String>| cmd.iter().any(|arg| arg == udid);
    (
        commands
            .iter()
            .filter(ours)
            .filter(|cmd| cmd.iter().any(|arg| arg == "recordVideo"))
            .count(),
        commands
            .iter()
            .filter(|cmd| cmd.iter().map(String::as_str).eq(["xcrun", "simctl", "shutdown", udid]))
            .count(),
    )
}

/// One test on `d` under a screen recording listener; how its recording ended.
async fn record_one_test(
    h: &simulator_support::Harness,
    d: &Arc<marathon_ios::apple_simulator_device::AppleSimulatorDevice>,
) -> TestResult<marathon_ios::apple_simulator_device::recording::RecordingFinalization> {
    let c = context(h, d.clone());
    let test = c.batch.tests[0].clone();
    let l = ScreenRecordingListener::new(c);
    l.test_started(&test).await.map_err(|e| format!("{e:?}"))?;
    // Paused time: every runnable task, a launch included, runs before this returns.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let ended = l.stop().await.map_err(|e| format!("{e:?}"))?;
    l.settle().await.map_err(|e| format!("{e:?}"))?;
    Ok(ended)
}

fn end_of_run_lines(h: &simulator_support::Harness) -> Vec<String> {
    h.logs
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.2.contains("at the end of the run"))
        .inspect(|r| assert_eq!(r.0, BUSY_VIDEO_TARGET, "{r:?}"))
        .map(|r| r.2.clone())
        .collect()
}

/// The run's end as the provider ends it: the run's stop seals the simulator, then disposal releases it.
async fn end_of_run(d: &Arc<marathon_ios::apple_simulator_device::AppleSimulatorDevice>) -> TestResult {
    use marathon_core::vendor::Device;
    d.seal_for_stop();
    d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_simulator_with_a_busy_video_service_is_shut_down_at_release_d98() -> TestResult {
    // The harness's release window is 200 ms; the shutdown's own timeout fits in it.
    let (h, d) = simulator_with_video_off(|v| v.timeout_configuration.shutdown = support::duration(100)).await?;
    h.worker.commands.lock().unwrap().clear();
    end_of_run(&d).await?;
    assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 1);
    assert!(!d.quarantined());
    let lines = end_of_run_lines(&h);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains(d.udid()) && lines[0].contains("because its video service was busy"));
    h.finish(&[d]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_configured_shutdown_or_erase_is_not_repeated_for_a_busy_video_service_d98() -> TestResult {
    for action in [LifecycleAction::Shutdown, LifecycleAction::Erase] {
        let (h, d) = simulator_with_video_off(|v| v.lifecycle_configuration.on_dispose = vec![action]).await?;
        end_of_run(&d).await?;
        assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 1, "{action:?}");
        assert!(end_of_run_lines(&h).is_empty());
        h.finish(&[d]).await;
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_busy_video_shutdown_longer_than_the_release_window_runs_within_it_d98() -> TestResult {
    // A shutdown timeout longer than the release window (200 ms in this harness, 70 s in a run; the timeout defaults to
    // the shell timeout, which may be minutes): the shutdown still runs, bounded by the window, not by its timeout.
    let (h, d) = simulator_with_video_off(|v| v.timeout_configuration.shutdown = support::duration(300)).await?;
    let started = tokio::time::Instant::now();
    end_of_run(&d).await?;
    assert_eq!(started.elapsed(), std::time::Duration::ZERO);
    assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 1);
    assert!(!d.quarantined());
    let lines = end_of_run_lines(&h);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("shut down at the end of the run"), "{lines:?}");
    h.finish(&[d]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_busy_video_shutdown_is_not_started_without_time_left_in_the_window_d98() -> TestResult {
    // Most of the 200 ms release window has passed before the release (as after a signal's finish): less than twice the
    // force reserve is left once the lease release's reserve is set aside, so the shutdown is not started.
    use marathon_core::vendor::Device;
    let (h, d) = simulator_with_video_off(|v| v.timeout_configuration.shutdown = support::duration(300)).await?;
    d.seal_for_stop();
    tokio::time::advance(std::time::Duration::from_millis(170)).await;
    let started = tokio::time::Instant::now();
    d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
    assert!(started.elapsed() <= std::time::Duration::from_millis(30));
    assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 0);
    assert!(!d.quarantined());
    let lines = end_of_run_lines(&h);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("no time left to shut it down"), "{lines:?}");
    h.finish(&[d]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_busy_video_shutdown_is_skipped_when_a_signal_stopped_the_run_d98() -> TestResult {
    // D97: after a signal the finish is for the exit; the shutdown is not started, even with time left in the window.
    use marathon_core::vendor::Device;
    let (h, d) = simulator_with_video_off(|v| v.timeout_configuration.shutdown = support::duration(100)).await?;
    let now = d.services().context.clock.now_millis();
    d.seal_for_signal(marathon_core::signal_finish::SignalFinish::from_start(now));
    d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 0);
    assert!(!d.quarantined());
    let lines = end_of_run_lines(&h);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("no time left to shut it down"), "{lines:?}");
    h.finish(&[d]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_quarantined_simulator_with_a_busy_video_service_is_not_shut_down_d98() -> TestResult {
    // The configured lifecycle never ends: the release window cuts it and quarantines the simulator.
    let (h, d) = simulator_with_video_off(|v| {
        v.lifecycle_configuration.on_dispose = vec![LifecycleAction::Terminate];
        v.timeout_configuration.shutdown = support::duration(100);
    })
    .await?;
    h.worker.cancellable_before.store(true, Ordering::SeqCst);
    *h.worker.before.lock().unwrap() = Arc::new(|cmd| {
        Box::pin(async move {
            if cmd.get(2).is_some_and(|s| s.starts_with("ps aux")) {
                std::future::pending::<()>().await;
            }
            Ok(None)
        })
    });
    end_of_run(&d).await?;
    assert!(d.quarantined());
    // Let a lifecycle task that outlived its window run on.
    tokio::time::sleep(std::time::Duration::from_secs(120)).await;
    assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 0);
    assert!(end_of_run_lines(&h).is_empty());
    let _ = d.join_retained().await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_healthy_simulator_gets_no_extra_command_at_release_d98() -> TestResult {
    let h = harness(|_| {})?;
    let d = h.device().await?;
    h.boot.state.store(3, Ordering::SeqCst);
    h.worker.commands.lock().unwrap().clear();
    end_of_run(&d).await?;
    assert!(h.worker.commands.lock().unwrap().is_empty());
    assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 0);
    h.finish(&[d]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_recovery_or_replacement_disposal_sends_no_shutdown_d98() -> TestResult {
    // A disposal outside the run's end (a lost simulator recovered, a replaced one): no mid-run cold boot.
    let (h, d) = simulator_with_video_off(|v| v.timeout_configuration.shutdown = support::duration(100)).await?;
    d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 0);
    assert!(end_of_run_lines(&h).is_empty());
    h.finish(&[d]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_hung_busy_video_shutdown_ends_with_the_window_and_never_quarantines_d98() -> TestResult {
    // A state read that never returns, then a `simctl shutdown` that never returns, with its own timeout inside the
    // window and (x101) longer than it.
    for (hung, timeout) in [("cat", 100), ("shutdown", 100), ("shutdown", 300)] {
        let (h, d) =
            simulator_with_video_off(|v| v.timeout_configuration.shutdown = support::duration(timeout)).await?;
        // The check below for no on-dispose lifecycle line needs the device's ordinary target, hidden with debug off.
        set_debug(&h, true);
        h.worker.cancellable_before.store(true, Ordering::SeqCst);
        *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
            Box::pin(async move {
                if cmd.first().is_some_and(|s| s == hung) || cmd.get(2).is_some_and(|s| s == hung) {
                    std::future::pending::<()>().await;
                }
                Ok(None)
            })
        });
        let started = tokio::time::Instant::now();
        end_of_run(&d).await?;
        // The harness's release window is 200 ms; the disposal ends with it less the 14 ms force reserve, not later.
        assert!(
            started.elapsed() <= std::time::Duration::from_millis(186),
            "{hung}: {:?}",
            started.elapsed()
        );
        assert!(!d.quarantined(), "{hung}");
        let lines = end_of_run_lines(&h);
        assert_eq!(lines.len(), 1, "{hung}: {lines:?}");
        // A hung state read meets the window; a hung `simctl shutdown` meets a command timeout inside the window first,
        // also when the configured shutdown timeout is longer than the window.
        let expected = if hung == "cat" {
            "did not finish within its window"
        } else {
            "did not shut down"
        };
        assert!(lines[0].contains(expected), "{hung}: {lines:?}");
        assert!(
            !h.logs
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.2.contains("on-dispose lifecycle")),
            "{hung}"
        );
        let _ = d.join_retained().await;
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_simulator_set_up_again_in_the_run_keeps_video_off_without_a_second_warning_d98() -> TestResult {
    use marathon_ios::apple_simulator_device::recording::RecordingFinalization;
    // x102: a recovery or a re-add sets the simulator up again as a new device object, built from the same run's services.
    let (h, a) = simulator_with_video_off(|v| v.timeout_configuration.shutdown = support::duration(100)).await?;
    // The recovery's disposal, during the run: no shutdown.
    a.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(video_commands(&h, a.udid()), (1, 0));
    let b = h.device().await?;
    assert!(!Arc::ptr_eq(&a, &b) && a.udid() == b.udid());
    assert_eq!(record_one_test(&h, &b).await?, RecordingFinalization::Skipped);
    assert_eq!(
        video_commands(&h, b.udid()),
        (1, 0),
        "the new object launches no recorder"
    );
    assert_eq!(busy_warnings(&h).len(), 1, "and does not warn again");
    // The run's end shuts the simulator down once, through the object that serves it then.
    end_of_run(&b).await?;
    assert_eq!(video_commands(&h, b.udid()), (1, 1));
    let lines = end_of_run_lines(&h);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("shut down at the end of the run"), "{lines:?}");
    h.finish(&[a, b]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_busy_simulator_leaves_another_simulator_on_the_worker_recording_d98() -> TestResult {
    use marathon_ios::apple_simulator_device::recording::RecordingFinalization;
    let (h, a) = simulator_with_video_off(|v| v.timeout_configuration.shutdown = support::duration(100)).await?;
    let other = h
        .device_on(
            h.worker.clone(),
            "b",
            Arc::new(
                marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext::new(
                    marathon_config::devices::Transport::Local,
                    h.clock.clone(),
                ),
            ),
        )
        .await?;
    *h.worker.session.lock().unwrap() = Arc::new(|cmd| {
        if !cmd.iter().any(|arg| arg == "recordVideo") {
            return Ok(None);
        }
        Ok(Some(simulator_support::recorder_session(|signal| {
            Box::pin(async move { Ok((signal == "INT").then_some(0)) })
        })))
    });
    assert_ne!(record_one_test(&h, &other).await?, RecordingFinalization::Skipped);
    assert_eq!(video_commands(&h, "b").0, 1);
    assert_eq!(busy_warnings(&h).len(), 1);
    end_of_run(&other).await?;
    end_of_run(&a).await?;
    assert_eq!(video_commands(&h, "b").1, 0, "a healthy simulator is not shut down");
    assert_eq!(video_commands(&h, "a"), (1, 1));
    h.finish(&[a, other]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_busy_simulator_booted_again_from_shutdown_records_again_d98() -> TestResult {
    use marathon_ios::apple_simulator_device::recording::RecordingFinalization;
    // x102: a simulator that went down without marathon's shutdown (a crash, a failed boot's own shutdown) and that
    // the recovery's preparation boots from Shutdown has a fresh video service.
    let (h, a) = simulator_with_video_off(|v| v.timeout_configuration.shutdown = support::duration(100)).await?;
    a.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
    h.boot.state.store(1, Ordering::SeqCst);
    let b = h.device().await?;
    assert!(b.boot(&h.services.context).await.map_err(|e| format!("{e:?}"))?);
    *h.worker.session.lock().unwrap() = Arc::new(|cmd| {
        if !cmd.iter().any(|arg| arg == "recordVideo") {
            return Ok(None);
        }
        Ok(Some(simulator_support::recorder_session(|signal| {
            Box::pin(async move { Ok((signal == "INT").then_some(0)) })
        })))
    });
    assert_ne!(record_one_test(&h, &b).await?, RecordingFinalization::Skipped);
    assert_eq!(
        video_commands(&h, b.udid()),
        (2, 0),
        "the booted simulator records again"
    );
    assert_eq!(busy_warnings(&h).len(), 1);
    end_of_run(&b).await?;
    assert_eq!(
        video_commands(&h, b.udid()).1,
        0,
        "and is not shut down at the run's end"
    );
    h.finish(&[a, b]).await;
    Ok(())
}

/// x102: a provider over the harness's worker, with the simulator `a` set up and its video service busy.
async fn provider_with_busy_simulator(
    h: &simulator_support::Harness,
) -> TestResult<(
    Arc<marathon_ios::device::simulator_provider::AppleSimulatorProvider>,
    Arc<provider_support::Connections>,
    Arc<marathon_ios::apple_simulator_device::AppleSimulatorDevice>,
)> {
    let connections = Arc::new(provider_support::Connections {
        workers: std::sync::Mutex::new(vec![(marathon_config::devices::Transport::Local, h.worker.clone())]),
        acquired: 0.into(),
        released: 0.into(),
        quarantined: 0.into(),
        fail_release: 0.into(),
        bridge: std::sync::Mutex::default(),
        trace: Arc::default(),
    });
    let provider = provider_support::ProviderHarness::build(h, connections.clone(), Arc::default())?;
    provider
        .process_update(
            provider_support::snapshot(vec![(
                marathon_config::devices::Transport::Local,
                provider_support::targets(&["a"]),
            )]),
            h.services.context.clone(),
        )
        .await
        .map_err(|e| format!("{e:?}"))?;
    let a = provider.devices().into_iter().next().ok_or("no simulator")?.handle;
    turn_video_off(h, &a).await?;
    Ok((provider, connections, a))
}

#[tokio::test(start_paused = true)]
async fn a_busy_simulator_disposed_during_the_run_is_shut_down_once_at_its_end_d98() -> TestResult {
    use marathon_core::vendor::DeviceProvider;
    // Disposed during the run (a recovery that then could not set it up again, a simulator lost too often): no shutdown
    // then, and no later object to shut it down at the run's end; the provider's termination does, once.
    let h = harness(|v| v.timeout_configuration.shutdown = support::duration(100))?;
    let (provider, connections, a) = provider_with_busy_simulator(&h).await?;
    a.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(video_commands(&h, "a"), (1, 0));
    provider.terminate().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(video_commands(&h, "a"), (1, 1));
    assert!(!a.quarantined());
    let lines = end_of_run_lines(&h);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("shut down at the end of the run"), "{lines:?}");
    assert_eq!(
        connections.released.load(Ordering::SeqCst),
        connections.acquired.load(Ordering::SeqCst)
    );
    let _ = a.join_retained().await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_busy_simulator_still_serving_at_the_end_is_shut_down_once_by_its_own_release_d98() -> TestResult {
    use marathon_core::vendor::DeviceProvider;
    // Control: the run-end disposal shuts it down, and the termination's step for disposed objects adds nothing.
    let h = harness(|v| v.timeout_configuration.shutdown = support::duration(100))?;
    let (provider, _, a) = provider_with_busy_simulator(&h).await?;
    provider.terminate().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(video_commands(&h, "a"), (1, 1));
    assert_eq!(end_of_run_lines(&h).len(), 1);
    let _ = a.join_retained().await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_busy_simulator_removed_from_the_devices_file_is_not_shut_down_d98() -> TestResult {
    use marathon_core::vendor::DeviceProvider;
    // It left the run: marathon no longer manages it.
    let h = harness(|v| v.timeout_configuration.shutdown = support::duration(100))?;
    let (provider, _, a) = provider_with_busy_simulator(&h).await?;
    provider
        .process_update(provider_support::snapshot(vec![]), h.services.context.clone())
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert!(a.disposed());
    provider.terminate().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(video_commands(&h, "a"), (1, 0));
    assert!(end_of_run_lines(&h).is_empty());
    let _ = a.join_retained().await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_busy_simulator_disposed_during_a_signal_stopped_run_is_not_shut_down_d98() -> TestResult {
    use marathon_core::vendor::DeviceProvider;
    // x101: after a signal the finish is for the exit; one warning instead.
    let h = harness(|v| v.timeout_configuration.shutdown = support::duration(100))?;
    let (provider, _, a) = provider_with_busy_simulator(&h).await?;
    a.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
    let now = h.services.context.clock.now_millis();
    provider.seal_for_signal(marathon_core::signal_finish::SignalFinish::from_start(now));
    provider.terminate().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(video_commands(&h, "a"), (1, 0));
    let lines = end_of_run_lines(&h);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("no time left to shut it down"), "{lines:?}");
    let _ = a.join_retained().await;
    Ok(())
}

/// The longest path the platform's file calls accept, counting the terminating NUL.
#[cfg(target_os = "macos")]
const PATH_MAX: usize = 1024;
#[cfg(not(target_os = "macos"))]
const PATH_MAX: usize = 4096;

/// U267, review round 1 P1: the movie is checked in the receive's own D97 stage, so a report path that just leaves
/// room for that stage still gets its movie. A second private directory, as round 1 had, ran past the platform's
/// path limit there and turned a complete movie into an undelivered one.
#[tokio::test]
async fn a_movie_at_a_report_path_near_the_platform_limit_is_still_delivered_u267() -> TestResult {
    use marathon_apple::cmd::{
        FileBridge, ReceiveCheck, Received, TransferStop, local::jvm_file_bridge::JvmFileBridge,
    };
    // D97's stage directory: `.marathon-artifact-` and six random characters.
    const STAGE: usize = ".marathon-artifact-XXXXXX/".len();
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("recording.mp4");
    std::fs::write(&source, listener_support::MOVIE)?;
    let name = "Tests.Suite#test1-batch.mp4";
    // The staged movie's path is 10 bytes short of the limit; one more directory level would not fit.
    let staged = PATH_MAX - 10;
    let mut parent = dir.path().join("report");
    let room = |parent: &std::path::Path| staged - (parent.as_os_str().len() + 1 + STAGE + name.len());
    assert!(room(&parent) >= 2);
    while room(&parent) > 0 {
        // Each level costs its name and a separator; never leave a room of 1, which no level fits.
        let left = room(&parent);
        parent.push("d".repeat(match left {
            ..=201 => left - 1,
            202 => 199,
            _ => 200,
        }));
    }
    std::fs::create_dir_all(&parent)?;
    let report = parent.join(name);
    assert_eq!(report.as_os_str().len() + STAGE, staged);
    // The witness is meaningful: a second private level below the stage cannot be created on this platform.
    let deeper = parent
        .join(".marathon-artifact-aaaaaa")
        .join(".marathon-artifact-bbbbbb");
    assert!(deeper.as_os_str().len() + 1 + name.len() >= PATH_MAX);
    if std::fs::create_dir_all(&deeper).is_ok() {
        let refused = std::fs::write(deeper.join(name), b"");
        assert!(refused.is_err(), "{refused:?}");
    }
    let _ = std::fs::remove_dir_all(parent.join(".marathon-artifact-aaaaaa"));

    let bridge = JvmFileBridge::new(Arc::new(listener_support::Runtime), true);
    let src = source.to_string_lossy().into_owned();
    // As before the check: an unchecked receive into this path delivers.
    assert_eq!(
        bridge.receive_with(&src, &report, TransferStop::never()).await,
        Ok(true)
    );
    assert_eq!(std::fs::read(&report)?, listener_support::MOVIE);
    std::fs::remove_file(&report)?;
    let check = || -> ReceiveCheck {
        Box::new(|movie| Ok(marathon_ios::listener::movie::check(movie)?.map_err(|why| why.reason().to_owned())))
    };
    assert_eq!(
        bridge
            .receive_checked(&src, &report, TransferStop::never(), check())
            .await,
        Ok(Received::Published)
    );
    assert_eq!(std::fs::read(&report)?, listener_support::MOVIE);
    // An empty movie at the same path is rejected and leaves the published one, the stage gone, the source kept.
    std::fs::write(&source, b"")?;
    assert_eq!(
        bridge
            .receive_checked(&src, &report, TransferStop::never(), check())
            .await,
        Ok(Received::Rejected("the recording is empty".into()))
    );
    assert_eq!(std::fs::read(&report)?, listener_support::MOVIE);
    assert_eq!(std::fs::read_dir(&parent)?.count(), 1);
    assert!(source.exists());
    Ok(())
}
