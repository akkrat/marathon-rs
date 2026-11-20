//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/video/ScreenRecordingListener.kt
use marathon_error::FailureKind;
mod listener_support;
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
    // simctl on a simulator whose video service is still held by an earlier, unstoppable recording, and an exit
    // status 16 for some other reason, which leaves video on.
    for busy in [true, false] {
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
            let (session, stderr) = simulator_support::unconfirmed_recorder_session(|_| Box::pin(async { Ok(None) }));
            let (exit, stdout) = (session.base.exit_code.clone(), session.base.stdout.clone());
            tokio::spawn(async move {
                let lines: &[&str] = if busy {
                    &[
                        "Error starting video recorder: Error Domain=NSPOSIXErrorDomain Code=16 \"Resource busy\" \
                         UserInfo={NSLocalizedFailureReason=A recording is already in progress}.",
                        "An error was encountered processing the command (domain=NSPOSIXErrorDomain, code=16):",
                        "A recording is already in progress",
                    ]
                } else {
                    &["An error was encountered processing the command (domain=NSPOSIXErrorDomain, code=16):"]
                };
                for line in lines {
                    stderr.send((*line).into()).await.unwrap();
                }
                stdout.close(None);
                stderr.close(None);
                exit.complete(Ok(Some(16)));
            });
            Ok(Some(session))
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
            .map(|r| r.2.clone())
            .collect();
        let undelivered: Vec<_> = logs
            .iter()
            .filter(|r| r.2.contains("was not delivered"))
            .map(|r| r.2.clone())
            .collect();
        assert!(!logs.iter().any(|r| r.2 == "Video recording did not start"));
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

/// D98: a booted simulator whose first recording met a busy video service.
async fn simulator_with_video_off(
    edit: impl FnOnce(&mut IosConfiguration),
) -> TestResult<(
    simulator_support::Harness,
    Arc<marathon_ios::apple_simulator_device::AppleSimulatorDevice>,
)> {
    let h = harness(edit)?;
    let d = h.device().await?;
    let c = context(&h, d.clone());
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
    assert!(
        h.logs
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.2.contains("video service is busy with a recording")),
        "video is off"
    );
    // Booted, as after its preparation.
    h.boot.state.store(3, Ordering::SeqCst);
    Ok((h, d))
}

fn end_of_run_lines(h: &simulator_support::Harness) -> Vec<String> {
    h.logs
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.2.contains("at the end of the run"))
        .map(|r| r.2.clone())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_simulator_with_a_busy_video_service_is_shut_down_at_release_d98() -> TestResult {
    // The harness's release window is 200 ms; the shutdown's own timeout fits in it.
    let (h, d) = simulator_with_video_off(|v| v.timeout_configuration.shutdown = support::duration(100)).await?;
    h.worker.commands.lock().unwrap().clear();
    d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
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
        d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
        assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 1, "{action:?}");
        assert!(end_of_run_lines(&h).is_empty());
        h.finish(&[d]).await;
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn the_busy_video_shutdown_needs_its_timeout_inside_the_release_window_d98() -> TestResult {
    // A shutdown timeout longer than the release window (200 ms in this harness, 70 s in a run): the shutdown is not
    // started, and the window not extended.
    let (h, d) = simulator_with_video_off(|v| v.timeout_configuration.shutdown = support::duration(300)).await?;
    let started = tokio::time::Instant::now();
    d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(started.elapsed(), std::time::Duration::ZERO);
    assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 0);
    assert!(!d.quarantined());
    let lines = end_of_run_lines(&h);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("no time left to shut it down"));
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
    d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
    assert!(d.quarantined());
    // Let a lifecycle task that outlived its window run on.
    tokio::time::sleep(std::time::Duration::from_secs(120)).await;
    assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 0);
    assert!(end_of_run_lines(&h).is_empty());
    // The cut lifecycle task reports its interruption.
    assert!(d.join_retained().await.is_err());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_healthy_simulator_gets_no_extra_command_at_release_d98() -> TestResult {
    let h = harness(|_| {})?;
    let d = h.device().await?;
    h.boot.state.store(3, Ordering::SeqCst);
    h.worker.commands.lock().unwrap().clear();
    d.dispose_and_join().await.map_err(|e| format!("{e:?}"))?;
    assert!(h.worker.commands.lock().unwrap().is_empty());
    assert_eq!(h.boot.shutdowns.load(Ordering::SeqCst), 0);
    h.finish(&[d]).await;
    Ok(())
}
