//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/video/ScreenRecordingListener.kt
mod listener_support;
mod simulator_support;
mod support;
use listener_support::{Bridge, context, device, harness};
use marathon_apple::{cmd::CommandResult, listener::AppleTestRunListener, logparser::parser::DeviceFailureReason};
use marathon_config::{schema::ScreenRecordingPolicy, vendor::*};
use marathon_core::{artifacts::AttachmentCollector, error::VendorError, model::DeviceFeature, runtime::Deferred};
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
        let result = l.test_failed(&test, 0, 0, None).await;
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
async fn stop_is_idempotent_and_blank_pidfile_is_ignored() -> TestResult {
    for pid in [None, Some("\u{a0} "), Some("123")] {
        let h = harness(|_| {})?;
        let d = h.device().await.map_err(|e| format!("{e:?}"))?;
        let l = ScreenRecordingListener::new(context(&h, d.clone()));
        let value = Arc::new(std::sync::Mutex::new(pid.map(str::to_owned)));
        *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
            let value = value.clone();
            Box::pin(async move {
                Ok(Some(if cmd[0] == "cat" {
                    value.lock().unwrap().clone().map(|v| ok([v])).unwrap_or_else(absent)
                } else if cmd == ["rm", "-rf", "/tmp/marathon/a/a.pid"] {
                    *value.lock().unwrap() = None;
                    ok(Vec::<String>::new())
                } else {
                    ok(Vec::<String>::new())
                }))
            })
        });
        h.worker.commands.lock().unwrap().clear();
        l.before_test_run().await.map_err(|e| format!("{e:?}"))?;
        l.after_test_run().await.map_err(|e| format!("{e:?}"))?;
        let cmds = h.worker.commands.lock().unwrap().clone();
        assert_eq!(
            cmds.iter()
                .filter(|c| c.last().is_some_and(|s| s.starts_with("kill -s INT")))
                .count(),
            usize::from(pid == Some("123"))
        );
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn waiting_start_warns_once_starts_anyway_and_failed_stop_still_logs() -> TestResult {
    let h = harness(|_| {})?;
    let d = h.device().await?;
    let c = context(&h, d.clone());
    let test = c.batch.tests[0].clone();
    let l = ScreenRecordingListener::new(c);
    let started = marathon_core::executor::shared::SharedDeferred::new();
    let event = started.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let event = event.clone();
        Box::pin(async move {
            Ok(Some(if cmd[0] == "cat" {
                ok(["123"])
            } else if cmd.last().is_some_and(|a| a.contains("nohup")) {
                event.complete(Ok(()));
                ok(Vec::<String>::new())
            } else if cmd.last().is_some_and(|a| a.starts_with("ps -p")) {
                // D17: the recorder is still alive, so the failed stop stays failed (A96).
                ok(["123"])
            } else {
                absent()
            }))
        })
    });
    let start = h.clock.now();
    l.test_started(&test).await.map_err(|e| format!("{e:?}"))?;
    started.wait().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(h.clock.now() - start, 10_000);
    l.after_test_run().await.map_err(|e| format!("{e:?}"))?;
    let logs = h.logs.lock().unwrap().clone();
    assert_eq!(
        logs.iter()
            .filter(|r| r.2.starts_with("Previous video recording didn't finish yet."))
            .count(),
        1
    );
    assert!(
        logs.iter()
            .any(|r| r.2 == "Previous video recording didn't finish in 10s, starting a new one anyway")
    );
    assert!(logs.iter().any(|r| r.2 == "stopped video recording pid=123"));
    h.finish(&[d]).await;
    Ok(())
}
/// The worker models the recorder as a process that lives from its nohup start until the INT in the stop command.
/// Real-host captures showed the second test of an ON_ANY run losing its video; nothing in the listener ends the
/// recorder before that INT, so both recordings must start and both videos must be attached.
#[tokio::test(start_paused = true)]
async fn two_tests_keep_the_recorder_alive_until_stop_and_attach_both_videos() -> TestResult {
    let mut h = harness(|_| {})?;
    Arc::make_mut(&mut h.services.configuration).screen_recording_policy = ScreenRecordingPolicy::OnAny;
    let bridge = Arc::new(Bridge::default());
    let d = device(&h, bridge.clone()).await.map_err(|e| format!("{e:?}"))?;
    let c = context(&h, d.clone());
    let tests = c.batch.tests.clone();
    let l = ScreenRecordingListener::new(c);
    let collector =
        AttachmentCollector::new(&[l.attachments.clone() as Arc<dyn marathon_core::listener::AttachmentProvider>])
            .map_err(|e| format!("{e:?}"))?;
    #[derive(Default)]
    struct Recorder {
        pid: Option<String>,
        alive: bool,
        starts: usize,
        alive_at_stop: Vec<bool>,
    }
    let recorder = Arc::new(std::sync::Mutex::new(Recorder::default()));
    let started: Arc<std::sync::Mutex<Vec<Arc<marathon_core::executor::shared::SharedDeferred<()>>>>> =
        Arc::new(std::sync::Mutex::new(vec![
            marathon_core::executor::shared::SharedDeferred::new(),
            marathon_core::executor::shared::SharedDeferred::new(),
        ]));
    let (model, events) = (recorder.clone(), started.clone());
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let (model, events) = (model.clone(), events.clone());
        Box::pin(async move {
            let mut r = model.lock().unwrap();
            Ok(Some(if cmd[0] == "cat" {
                r.pid.clone().map(|p| ok([p])).unwrap_or_else(absent)
            } else if cmd.last().is_some_and(|a| a.contains("nohup")) {
                r.starts += 1;
                r.alive = true;
                r.pid = Some(format!("{}", 100 + r.starts));
                events.lock().unwrap()[r.starts - 1].complete(Ok(()));
                ok(Vec::<String>::new())
            } else if cmd.last().is_some_and(|a| a.starts_with("kill -s INT")) {
                // lsof waits for the recorder to finish, so a live recorder makes the stop succeed.
                let alive = r.alive;
                r.alive_at_stop.push(alive);
                r.alive = false;
                if alive { ok(Vec::<String>::new()) } else { absent() }
            } else if cmd == ["rm", "-rf", "/tmp/marathon/a/a.pid"] {
                r.pid = None;
                ok(Vec::<String>::new())
            } else {
                ok(Vec::<String>::new())
            }))
        })
    });
    for (n, test) in tests.iter().enumerate() {
        l.test_started(test).await.map_err(|e| format!("{e:?}"))?;
        let event = started.lock().unwrap()[n].clone();
        event.wait().await.map_err(|e| format!("{e:?}"))?;
        l.test_passed(test, 0, 0).await.map_err(|e| format!("{e:?}"))?;
    }
    {
        let r = recorder.lock().unwrap();
        assert_eq!(
            (r.starts, r.alive_at_stop.clone(), r.pid.clone()),
            (2, vec![true, true], None)
        );
    }
    assert!(tests.iter().all(|t| collector.get(t).is_some()));
    assert!(
        !h.logs
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.2.starts_with("Previous video recording didn't finish"))
    );
    h.finish(&[d]).await;
    Ok(())
}
/// D17: the recorder exits before lsof's first pass, so the stop command returns 1 although the INT was delivered.
/// The recorder pid is gone, so the stop counts as done, the pidfile is removed and the next recording is not held up.
#[tokio::test(start_paused = true)]
async fn fast_recorder_exit_keeps_failed_stop_from_blocking_next_recording() -> TestResult {
    let mut h = harness(|_| {})?;
    Arc::make_mut(&mut h.services.configuration).screen_recording_policy = ScreenRecordingPolicy::OnAny;
    let bridge = Arc::new(Bridge::default());
    let d = device(&h, bridge.clone()).await.map_err(|e| format!("{e:?}"))?;
    let c = context(&h, d.clone());
    let tests = c.batch.tests.clone();
    let l = ScreenRecordingListener::new(c);
    let collector =
        AttachmentCollector::new(&[l.attachments.clone() as Arc<dyn marathon_core::listener::AttachmentProvider>])
            .map_err(|e| format!("{e:?}"))?;
    let pid = Arc::new(std::sync::Mutex::new(None::<String>));
    let started: Arc<std::sync::Mutex<Vec<Arc<marathon_core::executor::shared::SharedDeferred<()>>>>> =
        Arc::new(std::sync::Mutex::new(vec![
            marathon_core::executor::shared::SharedDeferred::new(),
            marathon_core::executor::shared::SharedDeferred::new(),
        ]));
    let starts = Arc::new(AtomicUsize::new(0));
    let (model, events, counter) = (pid.clone(), started.clone(), starts.clone());
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let (model, events, counter) = (model.clone(), events.clone(), counter.clone());
        Box::pin(async move {
            let mut pid = model.lock().unwrap();
            Ok(Some(if cmd[0] == "cat" {
                pid.clone().map(|p| ok([p])).unwrap_or_else(absent)
            } else if cmd.last().is_some_and(|a| a.contains("nohup")) {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                *pid = Some(format!("{}", 101 + n));
                events.lock().unwrap()[n].complete(Ok(()));
                ok(Vec::<String>::new())
            } else if cmd
                .last()
                .is_some_and(|a| a.starts_with("kill -s INT") || a.starts_with("ps -p"))
            {
                // lsof lost the race with the recorder's exit, and the probe finds no such pid.
                absent()
            } else if cmd == ["rm", "-rf", "/tmp/marathon/a/a.pid"] {
                *pid = None;
                ok(Vec::<String>::new())
            } else {
                ok(Vec::<String>::new())
            }))
        })
    });
    h.worker.commands.lock().unwrap().clear();
    let begin = h.clock.now();
    for (n, test) in tests.iter().enumerate() {
        l.test_started(test).await.map_err(|e| format!("{e:?}"))?;
        let event = started.lock().unwrap()[n].clone();
        event.wait().await.map_err(|e| format!("{e:?}"))?;
        l.test_passed(test, 0, 0).await.map_err(|e| format!("{e:?}"))?;
    }
    assert_eq!((starts.load(Ordering::SeqCst), pid.lock().unwrap().clone()), (2, None));
    assert!(h.clock.now() - begin < 10_000);
    let cmds = h.worker.commands.lock().unwrap().clone();
    let count = |prefix: &str| {
        cmds.iter()
            .filter(|c| c.last().is_some_and(|s| s.starts_with(prefix)))
            .count()
    };
    assert_eq!((count("kill -s INT"), count("ps -p")), (2, 2));
    assert!(tests.iter().all(|t| collector.get(t).is_some()));
    let logs = h.logs.lock().unwrap().clone();
    assert!(
        !logs
            .iter()
            .any(|r| r.2.starts_with("Previous video recording didn't finish"))
    );
    assert!(
        !logs
            .iter()
            .any(|r| r.2.starts_with("error while killing recording process"))
    );
    h.finish(&[d]).await;
    Ok(())
}
/// D17 keeps A96's warn-and-keep path for a stop that failed while the recorder pid still exists, and for a probe
/// that could not run.
#[tokio::test(start_paused = true)]
async fn live_recorder_and_failed_stop_keep_pidfile_and_warn() -> TestResult {
    // 0 alive pid, 1 exit code null, 2 `ps` rejecting the pid, 3 null result, 4 I/O error from the probe.
    for probe in 0..5 {
        let h = harness(|_| {})?;
        let d = h.device().await.map_err(|e| format!("{e:?}"))?;
        let l = ScreenRecordingListener::new(context(&h, d.clone()));
        *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
            Box::pin(async move {
                Ok(Some(if cmd[0] == "cat" {
                    ok(["123"])
                } else if cmd.last().is_some_and(|a| a.starts_with("ps -p")) {
                    match probe {
                        0 => ok(["123"]),
                        1 => CommandResult {
                            stdout: vec![],
                            stderr: vec![],
                            exit_code: None,
                        },
                        2 => CommandResult {
                            stdout: vec![],
                            stderr: vec!["Invalid process id".into()],
                            exit_code: Some(1),
                        },
                        3 => return Ok(None),
                        _ => {
                            return Err(VendorError::Other(marathon_core::error::Failure {
                                class: "java.io.IOException".into(),
                                message: Some("probe failed".into()),
                                cause: None,
                                stacktrace: None,
                            }));
                        }
                    }
                } else if cmd.last().is_some_and(|a| a.starts_with("kill -s INT")) {
                    absent()
                } else {
                    ok(Vec::<String>::new())
                }))
            })
        });
        h.worker.commands.lock().unwrap().clear();
        l.before_test_run().await.map_err(|e| format!("{e:?}"))?;
        l.after_test_run().await.map_err(|e| format!("{e:?}"))?;
        let cmds = h.worker.commands.lock().unwrap().clone();
        assert!(!cmds.iter().any(|c| c.first().is_some_and(|a| a == "rm")), "{cmds:?}");
        assert!(
            h.logs
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.2.starts_with("error while killing recording process"))
        );
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn stop_cancels_waiting_start_before_reading_pidfile() -> TestResult {
    let h = harness(|_| {})?;
    let d = h.device().await?;
    let c = context(&h, d.clone());
    let test = c.batch.tests[0].clone();
    let l = ScreenRecordingListener::new(c);
    let first_read = marathon_core::executor::shared::SharedDeferred::new();
    let event = first_read.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let event = event.clone();
        Box::pin(async move {
            Ok(Some(if cmd[0] == "cat" {
                event.complete(Ok(()));
                ok(["123"])
            } else {
                absent()
            }))
        })
    });
    h.worker.commands.lock().unwrap().clear();
    l.test_started(&test).await.map_err(|e| format!("{e:?}"))?;
    first_read.wait().await.map_err(|e| format!("{e:?}"))?;
    l.after_test_run().await.map_err(|e| format!("{e:?}"))?;
    assert!(
        !h.worker
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.last().is_some_and(|a| a.contains("nohup")))
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
        assert_eq!(h.worker.commands.lock().unwrap().len(), if mode == 0 { 2 } else { 1 });
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
            .map_err(|e| format!("{e:?}"))?
            .to_file_path_buf()?;
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
#[tokio::test(start_paused = true)]
async fn non_transfer_failure_aborts_composite_before_next_listener() -> TestResult {
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
    let composite = marathon_apple::listener::CompositeTestRunListener {
        listeners: vec![Arc::new(ScreenRecordingListener::new(c)), Arc::new(Next(next.clone()))],
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
    assert!(composite.test_failed(&test, 0, 0, None).await.is_err());
    assert_eq!(next.load(Ordering::SeqCst), 0);
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
