//! Port of vendor/vendor-android/src/test/kotlin/com/malinskiy/marathon/android/executor/listeners/filesync/FileSyncTestRunListenerTest.kt,
//! screenshot/ScreenCapturerTestRunListenerTest.kt and video/ScreenRecorderTestRunListenerTest.kt.

use marathon_error::{FailureKind, InjectedKind};
mod support;
use marathon_adb::{
    server::FakeAdbServer,
    testrunner::{MetricMap, TestEvent as E, TestIdentifier},
};
use marathon_android::{
    artifact_listeners::{adam_capture::*, file_sync::*, profiling::*, screenshot::*, video::*, *},
    listeners::*,
};
use marathon_config::{ScreenRecordingPolicy, vendor::*};
use marathon_core::runtime::Deferred;
use marathon_core::{artifacts::AttachmentCollector, listener::*, model::*, vendor::*};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use support::*;
fn id() -> TestIdentifier {
    TestIdentifier {
        class_name: js("pkg.clazz"),
        test_name: js("method"),
    }
}
fn artifacts(device: &marathon_android::device::AndroidDevice) -> ArtifactContext {
    ArtifactContext {
        device: device.clone(),
        pool: DevicePoolId { name: js("pool") },
        batch: Arc::new(TestBatch {
            tests: vec![test()],
            id: js("batch"),
        }),
    }
}
fn start() -> E {
    E::TestStarted { id: id() }
}
fn end() -> E {
    E::TestEnded {
        id: id(),
        metrics: MetricMap::new(),
    }
}
fn failed() -> E {
    E::TestFailed {
        id: id(),
        stack_trace: js("trace"),
    }
}
fn run_failed() -> E {
    E::TestRunFailed { error: js("broken") }
}
fn collector(provider: Arc<dyn AttachmentProvider>) -> Arc<AttachmentCollector> {
    checked(AttachmentCollector::new(&[provider]))
}
fn output(c: &mut marathon_config::Configuration) -> tempfile::TempDir {
    let temp = checked(tempfile::tempdir());
    c.output_dir = PathBuf::from(temp.path().to_string_lossy().into_owned());
    temp
}
fn shell(script: &mut Value, command: &str, stdout: &str) {
    script["shell"]
        .as_array_mut()
        .unwrap()
        .push(json!({"exact":command,"stdout":stdout}));
}
/// Polls rather than spinning: a yield loop keeps a vendor worker busy and starves the fake
/// server and the capture encoder on small CI runners. The first screenshot is a debug-build GIF
/// encode of a 1080x1920 frame, which takes seconds there, so the deadline only catches hangs.
async fn until(mut ready: impl FnMut() -> bool) {
    checked(
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            while !ready() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await,
    );
}
async fn started(server: &FakeAdbServer, prefix: &str, count: usize) {
    // The service request is recorded before dispatch; wait for the handler's command record.
    let command_prefix = prefix.strip_prefix("shell:").unwrap_or(prefix);
    until(|| {
        commands(server)
            .iter()
            .filter(|s| s.starts_with(command_prefix))
            .count()
            >= count
    })
    .await;
}
fn info() -> marathon_dex::apk_parser::InstrumentationInfo {
    marathon_dex::apk_parser::InstrumentationInfo {
        application_package: js("com.example"),
        instrumentation_package: js("com.example.test"),
        test_runner_class: js("androidx.test.runner.AndroidJUnitRunner"),
    }
}
mod file_sync_test_run_listener_test {
    use super::*;
    #[test]
    fn test_default() {
        for mode in [
            AggregationMode::Device,
            AggregationMode::Pool,
            AggregationMode::DeviceAndPool,
            AggregationMode::TestRun,
        ] {
            let mut c = configuration();
            let temp = output(&mut c);
            android(&mut c).file_sync_configuration.pull = vec![FileSyncEntry {
                relative_path: js("screenshots"),
                path_root: PathRoot::ExternalStorage,
                aggregation_mode: mode,
            }];
            run(c, move |runtime, context| {
                Box::pin(async move {
                    let _temp = temp;
                    let mut script = base_script(27, vec![]);
                    for cmd in [
                        "rm -r /sdcard/screenshots",
                        "mkdir /sdcard/screenshots",
                        "ls -l /sdcard/screenshots",
                    ] {
                        shell(&mut script, cmd, "");
                    }
                    let server = server(script).await;
                    let device = device(&server, runtime);
                    device.setup(context.clone()).await?;
                    let a = artifacts(&device);
                    let listener = FileSyncTestRunListener::new(a.clone());
                    listener.before_test_run_in(None, context.clone()).await?;
                    listener.after_test_run_in(context).await?;
                    assert_eq!(
                        commands(&server)[6..],
                        [
                            "rm -r /sdcard/screenshots",
                            "mkdir /sdcard/screenshots",
                            "ls -l /sdcard/screenshots"
                        ]
                    );
                    let mut folder = _temp.path().join("device-files");
                    if matches!(mode, AggregationMode::Pool | AggregationMode::DeviceAndPool) {
                        folder = folder.join("pool");
                    }
                    if matches!(mode, AggregationMode::Device | AggregationMode::DeviceAndPool) {
                        folder = folder.join(jcompat::file_manager::escape(&device.serial_number().to_owned()));
                    }
                    assert!(folder.join("screenshots").is_dir());
                    finish(server, device).await;
                    Ok(())
                })
            });
        }
    }
    #[test]
    fn test_push() {
        let mut c = configuration();
        let temp = output(&mut c);
        let entries = [
            ("fixture1", "cafebabe", PathRoot::LocalTmp, "/data/local/tmp"),
            ("fixture2", "cafebabe1", PathRoot::LocalTmp, "/data/local/tmp"),
            ("fixture3", "cafebabe2", PathRoot::ExternalStorage, "/sdcard"),
        ];
        for (name, content, root, _) in entries {
            let p = temp.path().join(name);
            checked(std::fs::write(&p, content));
            android(&mut c).file_sync_configuration.push.push(FilePushEntry {
                path: js(&p.to_string_lossy()),
                path_root: root,
            });
        }
        run(c, move |runtime, context| {
            Box::pin(async move {
                let _temp = temp;
                let mut script = base_script(27, vec![]);
                for (name, content, _, root) in entries {
                    shell(
                        &mut script,
                        &format!("md5 {root}/{name}"),
                        &format!("{:x}", md5::compute(content)),
                    );
                    shell(&mut script, &format!("rm -r {root}/{name}"), "");
                }
                let server = server(script).await;
                let device = device(&server, runtime);
                device.setup(context.clone()).await?;
                let listener = FileSyncTestRunListener::new(artifacts(&device));
                listener.before_test_run_in(None, context.clone()).await?;
                listener.after_test_run_in(context).await?;
                assert_eq!(
                    commands(&server)[6..],
                    [
                        "md5 /data/local/tmp/fixture1",
                        "md5 /data/local/tmp/fixture2",
                        "md5 /sdcard/fixture3",
                        "rm -r /data/local/tmp/fixture1",
                        "rm -r /data/local/tmp/fixture2",
                        "rm -r /sdcard/fixture3"
                    ]
                );
                for (name, content, _, root) in entries {
                    assert_eq!(
                        server
                            .file("emulator-5554", &format!("{root}/{name}"))
                            .unwrap()
                            .content
                            .unwrap(),
                        content.as_bytes()
                    );
                }
                finish(server, device).await;
                Ok(())
            })
        });
    }
    /// D27: without the storage wipe, a folder left by a run cut short must not stay what the tests read.
    #[test]
    fn a_folder_push_removes_a_leftover_first_d27() {
        let mut c = configuration();
        let temp = output(&mut c);
        let folder = temp.path().join("fixtures-dir");
        checked(std::fs::create_dir(&folder));
        checked(std::fs::write(folder.join("data.txt"), "fresh"));
        android(&mut c).file_sync_configuration.push.push(FilePushEntry {
            path: js(&folder.to_string_lossy()),
            path_root: PathRoot::ExternalStorage,
        });
        run(c, move |runtime, context| {
            Box::pin(async move {
                let _temp = temp;
                let mut script = base_script(27, vec![]);
                shell(
                    &mut script,
                    "rm -r -- '/sdcard/fixtures-dir' || { [ ! -e '/sdcard/fixtures-dir' ] && [ ! -L '/sdcard/fixtures-dir' ]; }",
                    "",
                );
                shell(&mut script, "rm -r /sdcard/fixtures-dir", "");
                script["shell"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({"prefix":"mkdir","stdout":""}));
                let server = server(script).await;
                let device = device(&server, runtime);
                device.setup(context.clone()).await?;
                let listener = FileSyncTestRunListener::new(artifacts(&device));
                listener.before_test_run_in(None, context.clone()).await?;
                let sent = commands(&server);
                let about: Vec<_> = sent.iter().filter(|c| c.contains("fixtures-dir")).collect();
                assert_eq!(
                    about.first().map(|c| c.as_str()),
                    Some(
                        "rm -r -- '/sdcard/fixtures-dir' || { [ ! -e '/sdcard/fixtures-dir' ] && [ ! -L '/sdcard/fixtures-dir' ]; }"
                    ),
                    "{about:?}"
                );
                assert!(about.len() > 1, "{about:?}");
                assert_eq!(
                    server
                        .file("emulator-5554", "/sdcard/fixtures-dir/data.txt")
                        .unwrap()
                        .content
                        .unwrap(),
                    b"fresh"
                );
                listener.after_test_run_in(context).await?;
                finish(server, device).await;
                Ok(())
            })
        });
    }
    #[test]
    fn test_pull() {
        pull(false)
    }
    #[test]
    fn test_pull_folder_with_empty_file() {
        pull(true)
    }
    fn pull(empty: bool) {
        let mut c = configuration();
        let temp = output(&mut c);
        let entries = if empty {
            vec![("fixture", PathRoot::ExternalStorage)]
        } else {
            vec![
                ("fixture1", PathRoot::ExternalStorage),
                ("fixture2", PathRoot::ExternalStorage),
                ("fixture3", PathRoot::LocalTmp),
                ("fixture4", PathRoot::AppData),
            ]
        };
        for (name, root) in &entries {
            android(&mut c).file_sync_configuration.pull.push(FileSyncEntry {
                relative_path: js(name),
                path_root: *root,
                aggregation_mode: AggregationMode::DeviceAndPool,
            });
        }
        run(c, move |runtime, context| {
            Box::pin(async move {
                let _temp = temp;
                let mut script = base_script(27, vec![]);
                let mut expected = vec![];
                for (name, root) in &entries {
                    let root = if *root == PathRoot::ExternalStorage {
                        "/sdcard"
                    } else {
                        "/data/local/tmp"
                    };
                    for cmd in [format!("rm -r {root}/{name}"), format!("mkdir {root}/{name}")] {
                        shell(&mut script, &cmd, "");
                        expected.push(cmd);
                    }
                }
                if !empty {
                    for cmd in [
                        "run-as com.example rm -R /data/data/com.example/fixture4",
                        "run-as com.example mkdir /data/data/com.example/fixture4",
                    ] {
                        shell(&mut script, cmd, "");
                        expected.push(cmd.into());
                    }
                }
                for (name, root) in &entries {
                    let root = if *root == PathRoot::ExternalStorage {
                        "/sdcard"
                    } else {
                        "/data/local/tmp"
                    };
                    if *name == "fixture4" {
                        let cmd = "mkdir -p /data/local/tmp/fixture4 && run-as com.example sh -c 'cd /data/data/com.example/fixture4 && tar cf - .' | tar xvf - -C /data/local/tmp/fixture4 && run-as com.example rm -R /data/data/com.example/fixture4";
                        shell(&mut script, cmd, "");
                        expected.push(cmd.into());
                    }
                    let cmd = format!("ls -l {root}/{name}");
                    shell(
                        &mut script,
                        &cmd,
                        if empty {
                            "-rw-rw---- 1 u0_a100 media_rw 0 2023-04-25 18:34 empty_file"
                        } else {
                            ""
                        },
                    );
                    expected.push(cmd);
                }
                if empty {
                    script["devices"][0]["files"] = json!([{"path":"/sdcard/fixture/empty_file","content":""}]);
                }
                let server = server(script).await;
                let device = device(&server, runtime);
                device.setup(context.clone()).await?;
                let listener = FileSyncTestRunListener::new(artifacts(&device));
                listener.before_test_run_in(Some(&info()), context.clone()).await?;
                listener.after_test_run_in(context).await?;
                assert_eq!(commands(&server)[6..], expected);
                if empty {
                    let path = _temp
                        .path()
                        .join("device-files/pool")
                        .join(jcompat::file_manager::escape(&device.serial_number().to_owned()))
                        .join("fixture/empty_file");
                    assert_eq!(checked(tokio::fs::read(path).await), b"");
                    assert!(server.transcript().iter().any(|r| r["type"] == "sync"));
                }
                finish(server, device).await;
                Ok(())
            })
        });
    }
}
/// D28: the storage is prepared once and pulled once, after every instrumentation ran: a second wipe would lose what the
/// first left there for the only pull. Each instrumentation's app data is prepared and collected.
#[test]
fn file_sync_with_two_instrumentations_prepares_and_pulls_once_d28() {
    let temp = checked(tempfile::tempdir());
    let mut c = configuration();
    c.output_dir = PathBuf::from(temp.path().to_string_lossy().into_owned());
    for (name, root) in [("fixture3", PathRoot::ExternalStorage), ("fixture4", PathRoot::AppData)] {
        android(&mut c).file_sync_configuration.pull.push(FileSyncEntry {
            relative_path: js(name),
            path_root: root,
            aggregation_mode: AggregationMode::DeviceAndPool,
        });
    }
    run(c, move |runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            let tar = |package: &str| {
                format!(
                    "mkdir -p /data/local/tmp/fixture4 && run-as {package} sh -c 'cd /data/data/{package}/fixture4 && tar cf - .' | tar xvf - -C /data/local/tmp/fixture4 && run-as {package} rm -R /data/data/{package}/fixture4"
                )
            };
            let expected = vec![
                "rm -r /sdcard/fixture3".to_owned(),
                "mkdir /sdcard/fixture3".to_owned(),
                "rm -r /data/local/tmp/fixture4".to_owned(),
                "mkdir /data/local/tmp/fixture4".to_owned(),
                "run-as com.example rm -R /data/data/com.example/fixture4".to_owned(),
                "run-as com.example mkdir /data/data/com.example/fixture4".to_owned(),
                "run-as other.app rm -R /data/data/other.app/fixture4".to_owned(),
                "run-as other.app mkdir /data/data/other.app/fixture4".to_owned(),
                "ls -l /sdcard/fixture3".to_owned(),
                tar("com.example"),
                tar("other.app"),
                "ls -l /data/local/tmp/fixture4".to_owned(),
            ];
            let mut script = base_script(27, vec![]);
            for command in &expected {
                shell(&mut script, command, "");
            }
            let server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            let listener = FileSyncTestRunListener::new(artifacts(&device));
            let first = info();
            let second = marathon_dex::apk_parser::InstrumentationInfo {
                application_package: js("other.app"),
                ..info()
            };
            listener.before_test_run_in(Some(&first), context.clone()).await?;
            // The same package again adds nothing.
            listener.before_next_instrumentation_in(&first, context.clone()).await?;
            listener
                .before_next_instrumentation_in(&second, context.clone())
                .await?;
            listener.after_test_run_in(context).await?;
            assert_eq!(commands(&server)[6..], expected);
            finish(server, device).await;
            Ok(())
        })
    });
}
mod screen_recorder_test_run_listener_test {
    use super::*;
    #[test]
    fn test_default() {
        record(27, false, false, true, false)
    }
    #[test]
    fn test_chunked() {
        record(27, true, false, true, false)
    }
    #[test]
    fn test_assumption_failed() {
        record(27, false, true, false, false)
    }
    #[test]
    fn test_run_failed() {
        record(27, false, false, false, true)
    }
    #[test]
    fn api34_and_success_policy() {
        record(34, false, false, true, false);
        record(34, false, false, false, true);
        record(27, false, false, false, false);
    }
    fn record(api: i32, chunked: bool, assumption: bool, fail: bool, run_failure: bool) {
        let mut c = configuration();
        let temp = output(&mut c);
        run(c, move |runtime, context| {
            Box::pin(async move {
                let _temp = temp;
                let mut script = base_script(api, vec![]);
                let paths: Vec<String> = (0..if chunked { 2 } else { 1 })
                    .map(|i| {
                        if api < 34 {
                            format!("/sdcard/pkg.clazz-method-batch-{i}.mp4")
                        } else {
                            "/sdcard/pkg.clazz-method-batch.mp4".into()
                        }
                    })
                    .collect();
                script["devices"][0]["files"] = json!(
                    paths
                        .iter()
                        .map(|p| json!({"path":p,"content":"video"}))
                        .collect::<Vec<_>>()
                );
                for (i, p) in paths.iter().enumerate() {
                    script["shell"].as_array_mut().unwrap().push(json!({"exact":format!("screenrecord --size 720x1280 --bit-rate 1000000 --time-limit 180 {p}"),"hang":!chunked||i==1}));
                    shell(&mut script, &format!("rm {p}"), "");
                }
                shell(&mut script, "ps -A | grep screenrecord", "");
                shell(&mut script, "echo quickhealthcheck", "quickhealthcheck");
                let server = server(script).await;
                let device = device(&server, runtime);
                device.setup(context.clone()).await?;
                let listener = Arc::new(ScreenRecorderTestBatchListener::new(
                    artifacts(&device),
                    ScreenRecordingPolicy::OnFailure,
                ));
                let attachments = collector(listener.clone());
                listener.on_event_in(&start(), context.clone()).await?;
                started(&server, "shell:screenrecord", paths.len()).await;
                if assumption {
                    listener
                        .on_event_in(
                            &E::TestAssumptionFailed {
                                id: id(),
                                stack_trace: js("assumed"),
                            },
                            context.clone(),
                        )
                        .await?;
                }
                if fail {
                    listener.on_event_in(&failed(), context.clone()).await?;
                }
                listener
                    .on_event_in(&if run_failure { run_failed() } else { end() }, context.clone())
                    .await?;
                listener.cleanup_in(context).await?;
                let values = attachments.get(&test()).unwrap_or_default();
                assert_eq!(
                    values.len(),
                    if run_failure || (!fail && !assumption) {
                        0
                    } else {
                        paths.len()
                    }
                );
                for (i, a) in values.iter().enumerate() {
                    assert_eq!(a.kind, AttachmentType::Video);
                    assert_eq!(
                        a.name,
                        Some(js(&if api < 34 {
                            format!("screen-{i}")
                        } else {
                            "screen".into()
                        }))
                    );
                    assert_eq!(checked(tokio::fs::read(a.file.clone()).await), b"video");
                }
                let commands = commands(&server);
                expect_setup(&commands);
                let mut expected = paths
                    .iter()
                    .map(|p| format!("screenrecord --size 720x1280 --bit-rate 1000000 --time-limit 180 {p}"))
                    .collect::<Vec<_>>();
                if run_failure {
                    expected.push("echo quickhealthcheck".into());
                }
                expected.push("ps -A | grep screenrecord".into());
                expected.extend(paths.iter().map(|p| format!("rm {p}")));
                assert_eq!(commands[6..], expected);
                if run_failure {
                    let local = artifacts(&device)
                        .file(marathon_core::output::FileType::Video, None, None, Some(js("0")))
                        .await?;
                    assert_eq!(checked(tokio::fs::read(local.clone()).await), b"video");
                }
                finish(server, device).await;
                Ok(())
            })
        });
    }
}
mod screen_capturer_test_run_listener_test {
    use super::*;
    #[test]
    fn test_default() {
        capture(false, false, false)
    }
    #[test]
    fn test_run_failed() {
        capture(true, false, false)
    }
    #[test]
    fn assumption_deletes_but_failure_attaches() {
        capture(false, true, false);
        capture(false, false, true);
    }
    fn capture(run_failure: bool, assumption: bool, failure: bool) {
        let mut c = configuration();
        let temp = output(&mut c);
        android(&mut c)
            .screen_record_configuration
            .screenshot_configuration
            .delay_ms = 60000;
        run(c, move |runtime, context| {
            Box::pin(async move {
                let _temp = temp;
                let mut script = base_script(27, vec![]);
                script["devices"][0]["framebuffer"] =
                    json!({"version":1,"file":fixture("vendor-android/framebuffer/screencap_1.bin")});
                let server = server(script).await;
                let device = device(&server, runtime);
                device.setup(context.clone()).await?;
                let policy = if assumption || failure {
                    ScreenRecordingPolicy::OnFailure
                } else {
                    ScreenRecordingPolicy::OnAny
                };
                let listener = Arc::new(ScreenCapturerTestRunListener::new(artifacts(&device), policy));
                let attachments = collector(listener.clone());
                listener.on_event_in(&start(), context.clone()).await?;
                until(|| listener.captures_taken() > 0).await;
                if assumption {
                    listener
                        .on_event_in(
                            &E::TestAssumptionFailed {
                                id: id(),
                                stack_trace: js("assumed"),
                            },
                            context.clone(),
                        )
                        .await?;
                }
                if failure {
                    listener.on_event_in(&failed(), context.clone()).await?;
                }
                listener
                    .on_event_in(&if run_failure { run_failed() } else { end() }, context.clone())
                    .await?;
                listener.cleanup_in(context).await?;
                let values = attachments.get(&test()).unwrap_or_default();
                assert_eq!(values.len(), usize::from(!run_failure && !assumption));
                let t = test();
                let path = artifacts(&device)
                    .file(
                        marathon_core::output::FileType::Screenshot,
                        if run_failure { None } else { Some(&t) },
                        None,
                        None,
                    )
                    .await?;
                if assumption {
                    assert!(!path.clone().exists());
                } else {
                    let bytes = checked(tokio::fs::read(path.clone()).await);
                    assert_eq!(&bytes[..6], b"GIF89a");
                    assert_eq!(bytes.last(), Some(&0x3b));
                }
                assert_eq!(
                    request_services(&server)
                        .iter()
                        .filter(|s| s.as_str() == "framebuffer:")
                        .count(),
                    1
                );
                finish(server, device).await;
                Ok(())
            })
        });
    }
}
#[test]
fn adam_extensions_and_missing_file_still_attach() {
    let mut c = configuration();
    let temp = output(&mut c);
    run(c, move |runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            let mut script = base_script(27, vec![]);
            script["devices"][0]["files"] = json!([{"path":"/sdcard/capture.jpg","content":"image"},{"path":"/sdcard/capture.png","content":"image"},{"path":"/sdcard/capture.webp","content":"image"},{"path":"/sdcard/capture.gif","content":"image"}]);
            let server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            let listener = Arc::new(AdamScreenCaptureTestRunListener::new(artifacts(&device)));
            let attachments = collector(listener.clone());
            for extension in ["jpg", "jpeg", "png", "webp", "gif", "JPG", "unknown"] {
                let mut metrics = MetricMap::new();
                metrics.insert(js(METRIC), js(&format!("/sdcard/capture.{extension}")));
                listener
                    .on_event_in(&E::TestEnded { id: id(), metrics }, context.clone())
                    .await?;
            }
            let values = attachments.get(&test()).unwrap();
            assert_eq!(
                values.iter().map(|a| a.kind).collect::<Vec<_>>(),
                [
                    AttachmentType::ScreenshotJpeg,
                    AttachmentType::ScreenshotJpeg,
                    AttachmentType::ScreenshotPng,
                    AttachmentType::ScreenshotWebp,
                    AttachmentType::ScreenshotGif
                ]
            );
            assert!(values.iter().all(|a| a.name == Some(js("screen"))));
            for (i, a) in values.iter().enumerate() {
                let file = a.file.clone();
                if i == 1 {
                    assert!(!file.exists());
                } else {
                    assert_eq!(checked(tokio::fs::read(&file).await), b"image");
                }
                assert_eq!(file.extension().unwrap(), if i == 2 { "png" } else { "jpg" });
            }
            finish(server, device).await;
            Ok(())
        })
    });
}
#[test]
fn adam_capture_with_relative_output_directory() {
    let mut c = configuration();
    let temp = checked(tempfile::tempdir_in("."));
    let relative = std::path::Path::new(temp.path().file_name().unwrap());
    assert!(relative.is_relative());
    c.output_dir = PathBuf::from(relative.to_string_lossy().into_owned());
    run(c, move |mut runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            // The shared harness assumes absolute output and passes an empty cwd.
            let cwd = checked(std::env::current_dir());
            let output = runtime.dependencies.configuration.output_dir.clone();
            let files = marathon_core::artifacts::FileManagerOutput::new(
                runtime.dependencies.io.clone(),
                jcompat::file_manager::FileManager::new(4096, 255, &output.to_string_lossy(), &cwd.to_string_lossy()),
                output,
                runtime.logger("FileManager"),
                runtime.log.clone(),
            );
            Arc::get_mut(&mut runtime).unwrap().dependencies.files = Arc::new(files);
            let mut script = base_script(27, vec![]);
            script["devices"][0]["files"] = json!([{"path":"/sdcard/capture.png","content":"image"}]);
            let server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            let listener = Arc::new(AdamScreenCaptureTestRunListener::new(artifacts(&device)));
            let attachments = collector(listener.clone());
            let mut metrics = MetricMap::new();
            metrics.insert(js(METRIC), js("/sdcard/capture.png"));
            listener
                .on_event_in(&E::TestEnded { id: id(), metrics }, context)
                .await?;
            let values = attachments.get(&test()).unwrap();
            assert_eq!(values.len(), 1);
            let path = values[0].file.clone();
            assert!(path.is_absolute());
            assert!(path.starts_with(_temp.path()));
            assert_eq!(checked(tokio::fs::read(path).await), b"image");
            finish(server, device).await;
            Ok(())
        })
    });
}

#[test]
fn grep_pid_oracle() {
    let rows = checked(std::fs::read_to_string(fixture("oracle/android-artifact-probes.jsonl")));
    for line in rows.lines() {
        let r: Value = checked(serde_json::from_str(line));
        let input = match r["case"].as_str().unwrap_or("") {
            "pid:normal" => Some("shell 123 1 screenrecord\n"),
            "pid:grep-last" => Some("shell 123 1 screenrecord\nshell 456 1 grep screenrecord\n"),
            "pid:bad" => Some("shell pid 1 screenrecord\n"),
            "pid:empty" => Some(""),
            "pid:tab" => Some("shell\t123 1 screenrecord\n"),
            _ => None,
        };
        if let Some(input) = input {
            assert_eq!(grep_pid(input), r["value"].as_str().unwrap());
        }
    }
}

async fn setup_bundle(device: &marathon_android::device::AndroidDevice) {
    use marathon_android::{dex_test_parser::DexTestBundleFiles, models::AndroidTestBundle};
    let bundle = Arc::new(AndroidTestBundle::new(
        DexTestBundleFiles {
            application: None,
            test_application: PathBuf::from(
                fixture("vendor-android/apk/app-debug-androidTest.apk")
                    .to_string_lossy()
                    .into_owned(),
            ),
            extra_applications: vec![],
            split_apks: vec![],
        },
        &PathBuf::from("/"),
    ));
    device.runtime().identifier.put(&test(), &bundle);
}
#[test]
fn kotlin_first_scenarios() {
    for mode in ["video", "video-timeout", "screenshot", "filesync-coverage", "perfetto"] {
        scenario(mode);
    }
}
fn scenario(mode: &'static str) {
    let mut c = configuration();
    let temp = output(&mut c);
    let pbtxt = temp.path().join("config.pbtxt");
    checked(std::fs::write(
        &pbtxt,
        "pid: ${TRACING_TARGET_PID}, pkg: ${TRACING_TARGET_PACKAGE}",
    ));
    c.is_code_coverage_enabled = mode == "filesync-coverage";
    let a = android(&mut c);
    a.screen_record_configuration.screenshot_configuration.delay_ms = 60000;
    a.profiling_configuration.pbtxt = Some(PathBuf::from(pbtxt.to_string_lossy().into_owned()));
    a.profiling_configuration.enabled = true;
    if mode == "filesync-coverage" {
        a.file_sync_configuration.pull.push(FileSyncEntry {
            relative_path: js("screenshots"),
            path_root: PathRoot::ExternalStorage,
            aggregation_mode: AggregationMode::Device,
        });
    }
    if mode == "video-timeout" {
        a.timeout_configuration.screenrecorder = jcompat::jtime::duration_of(0, 80_000_000).unwrap();
    }
    run(c, move |runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            let base = fixture("fake-adb/android-artifacts");
            let script = checked(marathon_adb::server::Script::from_yaml(&checked(
                tokio::fs::read_to_string(base.join(format!("{mode}.json"))).await,
            )));
            let mut server = checked(FakeAdbServer::start(checked(script.compile(&base)), Default::default()).await);
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            setup_bundle(&device).await;
            let a = artifacts(&device);
            let (listener, attachments, capturer): ScenarioListeners = match mode {
                "video" | "video-timeout" => {
                    let l = Arc::new(ScreenRecorderTestBatchListener::new(a, ScreenRecordingPolicy::OnAny));
                    let c = collector(l.clone());
                    (l, Some(c), None)
                }
                "screenshot" => {
                    let l = Arc::new(ScreenCapturerTestRunListener::new(a, ScreenRecordingPolicy::OnAny));
                    let c = collector(l.clone());
                    (l.clone(), Some(c), Some(l))
                }
                "filesync-coverage" => (Arc::new(FileSyncTestRunListener::new(a)), None, None),
                _ => {
                    let l = Arc::new(ProfilingRunListener::new(a));
                    let c = collector(l.clone());
                    (l, Some(c), None)
                }
            };
            listener.before_test_run_in(Some(&info()), context.clone()).await?;
            listener
                .on_event_in(&E::TestRunStartedEvent { test_count: 1 }, context.clone())
                .await?;
            if mode != "filesync-coverage" {
                listener.on_event_in(&start(), context.clone()).await?;
                match mode {
                    "video" | "video-timeout" => {
                        started(&server, "shell:screenrecord", if mode == "video" { 1 } else { 2 }).await
                    }
                    "screenshot" => until(|| capturer.as_ref().unwrap().captures_taken() > 0).await,
                    _ => started(&server, "shell:echo 'pid:", 1).await,
                }
                listener.on_event_in(&end(), context.clone()).await?;
            }
            listener.after_test_run_in(context.clone()).await?;
            listener.cleanup_in(context).await?;
            let reference: Vec<Value> =
                checked(tokio::fs::read_to_string(base.join(format!("{mode}-kotlin-transcript.jsonl"))).await)
                    .lines()
                    .map(|s| checked(serde_json::from_str(s)))
                    .collect();
            let services = |rows: Vec<Value>| {
                rows.into_iter()
                    .filter(|r| r["type"] == "request")
                    .filter_map(|r| r["service"].as_str().map(str::to_owned))
                    .filter(|s| !s.starts_with("host:transport:") && s != "shell:logcat -v long")
                    .collect::<Vec<_>>()
            };
            assert_eq!(services(server.transcript()), services(reference), "{mode}");
            if let Some(attachments) = attachments {
                let values = attachments.get(&test()).unwrap_or_default();
                let observed: Value = checked(serde_json::from_str(&checked(
                    tokio::fs::read_to_string(base.join(format!("{mode}-kotlin.json"))).await,
                )));
                let expected = observed[0]["value"].as_array().unwrap();
                assert_eq!(values.len(), expected.len());
                for (a, e) in values.iter().zip(expected) {
                    assert_eq!(a.name.as_ref().unwrap().to_owned(), e[0].as_str().unwrap());
                    let p = a.file.clone();
                    assert_eq!(p.file_name().unwrap().to_string_lossy(), e[2].as_str().unwrap());
                    if mode.starts_with("video") {
                        assert_eq!(
                            checked(tokio::fs::read(p).await),
                            checked(tokio::fs::read(base.join("sample.mp4")).await)
                        );
                    } else if mode == "screenshot" {
                        compare_gif(
                            &checked(tokio::fs::read(p).await),
                            &checked(tokio::fs::read(base.join("screenshot-kotlin.gif")).await),
                        );
                    } else {
                        assert_eq!(checked(tokio::fs::read(p).await), b"trace");
                    }
                }
            }
            checked(device.dispose().await);
            assert_eq!(server.warnings(), 0);
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
fn compare_gif(actual: &[u8], expected: &[u8]) {
    fn decode(bytes: &[u8]) -> (u16, u16, u16, Vec<u8>) {
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut reader = checked(options.read_info(bytes));
        let dims = (reader.width(), reader.height());
        let frame = checked(reader.read_next_frame()).unwrap();
        let result = (dims.0, dims.1, frame.delay, frame.buffer.to_vec());
        assert!(checked(reader.read_next_frame()).is_none());
        result
    }
    let a = decode(actual);
    let e = decode(expected);
    assert_eq!((a.0, a.1, a.2), (e.0, e.1, e.2));
    assert_eq!(a.3.len(), e.3.len());
    let difference: u64 = a.3.iter().zip(&e.3).map(|(a, e)| a.abs_diff(*e) as u64).sum();
    assert!(
        difference / (a.3.len() as u64) < 12,
        "GIF decoded channel mean difference too large"
    );
}
#[test]
fn profiling_setup_errors_are_batch_failures() {
    let mut c = configuration();
    let temp = output(&mut c);
    android(&mut c).profiling_configuration.pbtxt = Some(PathBuf::from("/nonexistent/config.pbtxt"));
    run(c, move |runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            let server = server(base_script(27, vec![])).await;
            let device = device(&server, runtime);
            let listener = ProfilingRunListener::new(artifacts(&device));
            let error = listener.before_test_run_in(None, context.clone()).await.unwrap_err();
            assert_eq!(error.failure().kind, marathon_error::FailureKind::FileNotFound);
            assert_eq!(
                error.failure().message,
                Some(js("/nonexistent/config.pbtxt (No such file or directory)"))
            );
            let error = listener
                .on_event_in(&E::TestRunStartedEvent { test_count: 1 }, context.clone())
                .await
                .unwrap_err();
            assert!(
                error
                    .failure()
                    .message
                    .as_ref()
                    .unwrap()
                    .to_owned()
                    .contains("can't locate test bundle")
            );
            let mut a = artifacts(&device);
            a.batch = Arc::new(TestBatch {
                tests: vec![],
                id: js("batch"),
            });
            let listener = ProfilingRunListener::new(a);
            assert_eq!(
                listener
                    .on_event_in(&E::TestRunStartedEvent { test_count: 0 }, context)
                    .await
                    .unwrap_err()
                    .failure()
                    .kind
                    .description(),
                js("missing element")
            );
            finish(server, device).await;
            Ok(())
        })
    });
}
#[test]
fn profiling_run_failure_stops_and_joins_without_pull() {
    let mut c = configuration();
    let temp = output(&mut c);
    run(c, move |runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            let mut script = base_script(27, vec![]);
            script["shell"].as_array_mut().unwrap().push(json!({"exact":"echo 'null' | perfetto --txt -c - -o /data/misc/perfetto-traces/pkg.clazz-method-batch.perfetto-trace","hang":true}));
            shell(&mut script, "killall perfetto", "");
            let server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            let listener = Arc::new(ProfilingRunListener::new(artifacts(&device)));
            let attachments = collector(listener.clone());
            listener.on_event_in(&run_failed(), context.clone()).await?;
            assert!(commands(&server)[6..].is_empty());
            listener.on_event_in(&start(), context.clone()).await?;
            started(&server, "shell:echo 'null'", 1).await;
            listener.on_event_in(&run_failed(), context.clone()).await?;
            listener.on_event_in(&run_failed(), context.clone()).await?;
            listener.cleanup_in(context).await?;
            assert!(attachments.get(&test()).is_none());
            assert_eq!(
                commands(&server)[6..],
                [
                    "echo 'null' | perfetto --txt -c - -o /data/misc/perfetto-traces/pkg.clazz-method-batch.perfetto-trace",
                    "killall perfetto"
                ]
            );
            finish(server, device).await;
            Ok(())
        })
    });
}

#[test]
fn factory_produces_all_attachments_before_results_complete() {
    let mut c = configuration();
    let temp = output(&mut c);
    let pbtxt = temp.path().join("config.pbtxt");
    checked(std::fs::write(&pbtxt, "pid: ${TRACING_TARGET_PID}"));
    c.screen_recording_policy = ScreenRecordingPolicy::OnAny;
    android(&mut c).profiling_configuration.enabled = true;
    android(&mut c).profiling_configuration.pbtxt = Some(PathBuf::from(pbtxt.to_string_lossy().into_owned()));
    run(c, move |runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            let mut script = base_script(33, vec![]);
            script["devices"][0]["files"] = json!([{"path":"/sdcard/pkg.clazz-method-batch-0.mp4","content":"video"},{"path":"/sdcard/capture.png","content":"png"},{"path":"/data/misc/perfetto-traces/pkg.clazz-method-batch.perfetto-trace","content":"trace"}]);
            script["shell"]
                .as_array_mut()
                .unwrap()
                .push(json!({"prefix":"screenrecord ","hang":true}));
            for (cmd, out) in [
                ("pidof com.example", "42"),
                (
                    "echo 'pid: 42' | perfetto --txt -c - -o /data/misc/perfetto-traces/pkg.clazz-method-batch.perfetto-trace",
                    "",
                ),
                ("ps -A | grep screenrecord", ""),
                ("rm /sdcard/pkg.clazz-method-batch-0.mp4", ""),
                ("killall perfetto", ""),
                (
                    "rm -f /data/misc/perfetto-traces/pkg.clazz-method-batch.perfetto-trace",
                    "",
                ),
            ] {
                shell(&mut script, cmd, out);
            }
            let server = server(script).await;
            let device = device(&server, runtime.clone());
            device.setup(context.clone()).await?;
            setup_bundle(&device).await;
            let deferred = marathon_core::executor::shared::SharedDeferred::new();
            let a = artifacts(&device);
            let listener = marathon_android::listener_factory::create(
                &device,
                &runtime.dependencies.configuration,
                a.pool,
                a.batch,
                deferred.clone(),
                &Default::default(),
            )?;
            listener.before_test_run_in(Some(&info()), context.clone()).await?;
            listener
                .on_event_in(&E::TestRunStartedEvent { test_count: 1 }, context.clone())
                .await?;
            listener.on_event_in(&start(), context.clone()).await?;
            started(&server, "shell:screenrecord", 1).await;
            started(&server, "shell:echo 'pid:", 1).await;
            device.on_line(&js("log line")).await?;
            let mut metrics = MetricMap::new();
            metrics.insert(js(METRIC), js("/sdcard/capture.png"));
            listener
                .on_event_in(&E::TestEnded { id: id(), metrics }, context.clone())
                .await?;
            listener.after_test_run_in(context.clone()).await?;
            listener.cleanup_in(context).await?;
            let result = checked(deferred.wait().await);
            let result = &result.finished[0];
            assert_eq!(
                result
                    .attachments
                    .iter()
                    .map(|a| (a.kind, a.name.clone()))
                    .collect::<Vec<_>>(),
                [
                    (AttachmentType::Video, Some(js("screen-0"))),
                    (AttachmentType::Log, Some(js("logcat"))),
                    (AttachmentType::ScreenshotPng, Some(js("screen"))),
                    (AttachmentType::Profiling, Some(js("profiling")))
                ]
            );
            assert_eq!(device.listener_counts(), (0, 0));
            finish(server, device).await;
            Ok(())
        })
    });
}
#[test]
fn stop_uses_last_grep_line_and_five_kill_attempts() {
    run(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(33, vec![]);
            shell(
                &mut script,
                "ps -A | grep screenrecord",
                "shell 123 1 screenrecord\nshell 456 1 grep screenrecord\n",
            );
            shell(&mut script, "kill -2 456", "");
            let server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            stop_screen_record(&device, context).await?;
            assert_eq!(
                commands(&server)[6..],
                ["ps -A | grep screenrecord", "kill -2 456"].repeat(5)
            );
            finish(server, device).await;
            Ok(())
        })
    });
}
#[test]
fn unhealthy_run_failure_joins_recording_without_pull() {
    let mut c = configuration();
    let temp = output(&mut c);
    run(c, move |runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            let mut script = base_script(33, vec![]);
            script["shell"]
                .as_array_mut()
                .unwrap()
                .push(json!({"prefix":"screenrecord ","hang":true}));
            shell(&mut script, "echo quickhealthcheck", "unhealthy");
            let server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            let listener = Arc::new(ScreenRecorderTestBatchListener::new(
                artifacts(&device),
                ScreenRecordingPolicy::OnAny,
            ));
            let attachments = collector(listener.clone());
            listener.on_event_in(&start(), context.clone()).await?;
            started(&server, "shell:screenrecord", 1).await;
            listener.on_event_in(&run_failed(), context.clone()).await?;
            listener.on_event_in(&run_failed(), context.clone()).await?;
            listener.cleanup_in(context).await?;
            assert!(attachments.get(&test()).is_none());
            assert_eq!(commands(&server).last().unwrap(), "echo quickhealthcheck");
            assert!(!request_services(&server).contains(&"sync:".to_owned()));
            finish(server, device).await;
            Ok(())
        })
    });
}
#[test]
fn cancelled_and_dropped_execute_join_artifact_jobs() {
    for drop_waiter in [false, true] {
        let mut c = configuration();
        let temp = output(&mut c);
        android(&mut c).application_pm_clear = false;
        android(&mut c).test_application_pm_clear = false;
        run(c, move |runtime, context| {
            Box::pin(async move {
                use marathon_android::{device::AndroidBatchExecutor, runner::AndroidRunnerExecutor};
                let _temp = temp;
                let mut script = base_script(33, vec![]);
                script["shell"].as_array_mut().unwrap().extend([json!({"prefix":"screenrecord ","hang":true}),json!({"prefix":"am instrument ","stdout":"INSTRUMENTATION_STATUS: class=pkg.clazz\nINSTRUMENTATION_STATUS: test=method\nINSTRUMENTATION_STATUS: numtests=1\nINSTRUMENTATION_STATUS_CODE: 1\n","hang":true})]);
                let server = server(script).await;
                let device = device(&server, runtime.clone());
                device.setup(context.clone()).await?;
                setup_bundle(&device).await;
                let signal = marathon_core::executor::Signal::new();
                let context = VendorContext {
                    cancellation: signal.clone(),
                    ..context
                };
                let cleaned = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let executor = AndroidRunnerExecutor {
                    slots: marathon_android::listener_factory::ListenerSlots {
                        profiling: Some(Arc::new(CleanupWitness(cleaned.clone()))),
                        ..Default::default()
                    },
                };
                let a = artifacts(&device);
                let mut future = executor.execute(
                    &device,
                    runtime.dependencies.configuration.clone(),
                    a.pool,
                    a.batch,
                    marathon_core::executor::shared::SharedDeferred::new(),
                    context,
                );
                tokio::select! {biased; _=&mut future=>panic!("execute ended early"), _=started(&server,"shell:screenrecord",1)=>{}}
                if drop_waiter {
                    drop(future);
                } else {
                    signal.cancel(marathon_core::executor::shared::requested("test cancellation"));
                    assert!(future.await.is_err());
                }
                // Last cleanup callback runs only after recorder and instrumentation joins.
                until(|| cleaned.load(std::sync::atomic::Ordering::Acquire)).await;
                finish(server, device).await;
                Ok(())
            })
        });
    }
}

#[test]
fn executor_preserves_execution_error_when_cleanup_fails() {
    let mut c = configuration();
    let temp = output(&mut c);
    run(c, move |mut runtime, context| {
        Box::pin(async move {
            use marathon_android::{device::AndroidBatchExecutor, runner::AndroidRunnerExecutor};
            let _temp = temp;
            let errors = Arc::new(std::sync::Mutex::new(Vec::new()));
            let logged = errors.clone();
            Arc::get_mut(&mut runtime).unwrap().log = Arc::new(move |_, _, _, error| {
                if let Some(error) = error {
                    logged.lock().unwrap().push(error.clone());
                }
            });
            let server = server(base_script(27, vec![])).await;
            let device = device(&server, runtime.clone());
            device.setup(context.clone()).await?;
            let executor = AndroidRunnerExecutor {
                slots: marathon_android::listener_factory::ListenerSlots {
                    recorder: Some(Arc::new(FailingCleanup)),
                    ..Default::default()
                },
            };
            let error = executor
                .execute(
                    &device,
                    runtime.dependencies.configuration.clone(),
                    artifacts(&device).pool,
                    Arc::new(TestBatch {
                        tests: vec![],
                        id: js("batch"),
                    }),
                    marathon_core::executor::shared::SharedDeferred::new(),
                    context,
                )
                .await
                .unwrap_err();
            assert_eq!(error.failure().message, Some(js("execution failure")));
            assert!(
                errors
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| e.message == Some(js("cleanup failure")))
            );
            finish(server, device).await;
            Ok(())
        })
    });
}

struct FailingCleanup;
impl marathon_android::listener_factory::AndroidListenerFactory for FailingCleanup {
    fn create(
        &self,
        _: &marathon_android::device::AndroidDevice,
        _: &marathon_config::Configuration,
        _: &DevicePoolId,
        _: &BatchRef,
    ) -> marathon_core::error::VendorResult<marathon_android::listener_factory::InstalledListener> {
        Ok(marathon_android::listener_factory::InstalledListener {
            listener: Arc::new(Self),
            attachments: None,
        })
    }
}
impl AndroidTestRunListener for FailingCleanup {
    fn before_test_run_in<'a>(
        &'a self,
        _: Option<&'a marathon_dex::apk_parser::InstrumentationInfo>,
        _: VendorContext,
    ) -> marathon_core::runtime::VendorFuture<'a, ()> {
        Box::pin(async {
            Err(marathon_core::error::VendorError::Other(
                marathon_core::error::Failure {
                    kind: FailureKind::Injected(InjectedKind::ExecutionError),
                    message: Some(js("execution failure")),
                    cause: None,
                    stacktrace: None,
                },
            ))
        })
    }
    fn cleanup_in(&self, _: VendorContext) -> marathon_core::runtime::VendorFuture<'_, ()> {
        Box::pin(async {
            Err(marathon_core::error::VendorError::Other(
                marathon_core::error::Failure {
                    kind: FailureKind::Injected(InjectedKind::CleanupError),
                    message: Some(js("cleanup failure")),
                    cause: None,
                    stacktrace: None,
                },
            ))
        })
    }
}

struct CleanupWitness(Arc<std::sync::atomic::AtomicBool>);
impl marathon_android::listener_factory::AndroidListenerFactory for CleanupWitness {
    fn create(
        &self,
        _: &marathon_android::device::AndroidDevice,
        _: &marathon_config::Configuration,
        _: &DevicePoolId,
        _: &BatchRef,
    ) -> marathon_core::error::VendorResult<marathon_android::listener_factory::InstalledListener> {
        Ok(marathon_android::listener_factory::InstalledListener {
            listener: Arc::new(CleanupWitness(self.0.clone())),
            attachments: None,
        })
    }
}
impl AndroidTestRunListener for CleanupWitness {
    fn cleanup_in(&self, _: VendorContext) -> marathon_core::runtime::VendorFuture<'_, ()> {
        Box::pin(async move {
            self.0.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        })
    }
}

type ScenarioListeners = (
    Arc<dyn AndroidTestRunListener>,
    Option<Arc<AttachmentCollector>>,
    Option<Arc<ScreenCapturerTestRunListener>>,
);

#[test]
fn missing_and_timed_out_pulls_still_emit_video_capture_and_trace_attachments() {
    for timeout in [false, true] {
        let mut c = configuration();
        let temp = output(&mut c);
        if timeout {
            android(&mut c).timeout_configuration.pull_file = jcompat::jtime::Duration::ZERO;
        }
        run(c, move |runtime, context| {
            Box::pin(async move {
                let _temp = temp;
                let mut script = base_script(33, vec![]);
                for command in [
                    "ps -A | grep screenrecord",
                    "rm /sdcard/pkg.clazz-method-batch-0.mp4",
                    "killall perfetto",
                    "rm -f /data/misc/perfetto-traces/pkg.clazz-method-batch.perfetto-trace",
                ] {
                    shell(&mut script, command, "");
                }
                let server = server(script).await;
                let device = device(&server, runtime);
                device.setup(context.clone()).await?;
                let video = Arc::new(ScreenRecorderTestBatchListener::new(
                    artifacts(&device),
                    ScreenRecordingPolicy::OnAny,
                ));
                let capture = Arc::new(AdamScreenCaptureTestRunListener::new(artifacts(&device)));
                let trace = Arc::new(ProfilingRunListener::new(artifacts(&device)));
                let collected = checked(AttachmentCollector::new(&[
                    video.clone(),
                    capture.clone(),
                    trace.clone(),
                ]));
                video.on_event_in(&end(), context.clone()).await?;
                let mut metrics = MetricMap::new();
                metrics.insert(js(METRIC), js("/missing.png"));
                capture
                    .on_event_in(&E::TestEnded { id: id(), metrics }, context.clone())
                    .await?;
                trace.on_event_in(&end(), context).await?;
                let values = collected.get(&test()).unwrap();
                assert_eq!(values.len(), 3);
                assert!(values.iter().all(|a| !a.file.clone().exists()));
                let sync = server.transcript().iter().filter(|r| r["type"] == "sync").count();
                assert_eq!(sync, if timeout { 0 } else { 3 });
                finish(server, device).await;
                Ok(())
            })
        });
    }
}

/// The file sync commands sent for one folder named `name` pushed to external storage, before and after a batch, as the
/// fake received them. The folder lives in the returned directory.
fn folder_push_commands(name: &str) -> (tempfile::TempDir, Vec<String>) {
    let mut c = configuration();
    let temp = output(&mut c);
    let folder = temp.path().join(name);
    checked(std::fs::create_dir(&folder));
    checked(std::fs::write(folder.join("data.txt"), "fresh"));
    android(&mut c).file_sync_configuration.push.push(FilePushEntry {
        path: js(&folder.to_string_lossy()),
        path_root: PathRoot::ExternalStorage,
    });
    let sent = Arc::new(std::sync::Mutex::new(vec![]));
    let into = sent.clone();
    run(c, move |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(27, vec![]);
            let rules = script["shell"].as_array_mut().unwrap();
            rules.push(json!({"prefix":"rm ","stdout":""}));
            rules.push(json!({"prefix":"mkdir","stdout":""}));
            let server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            let before = commands(&server).len();
            let listener = FileSyncTestRunListener::new(artifacts(&device));
            listener.before_test_run_in(None, context.clone()).await?;
            listener.after_test_run_in(context).await?;
            *into.lock().unwrap() = commands(&server).split_off(before);
            finish(server, device).await;
            Ok(())
        })
    });
    let sent = std::mem::take(&mut *sent.lock().unwrap());
    (temp, sent)
}

/// Runs a device command with the local `sh`, with `/sdcard` standing for `root`. No real device or adb is involved.
fn replay(command: &str, root: &std::path::Path) -> std::process::Output {
    let command = command.replace("/sdcard", &root.to_string_lossy());
    checked(
        std::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .current_dir(root)
            .output(),
    )
}

/// x37: the removal before a folder push named `payload data` used to be `rm -r /sdcard/payload data`, which deleted a
/// sibling `payload`.
#[test]
fn a_folder_push_with_a_space_removes_only_its_own_destination_d27() {
    let (temp, sent) = folder_push_commands("payload data");
    let removal = sent.iter().find(|c| c.starts_with("rm -r ")).unwrap();
    let root = temp.path().join("device");
    checked(std::fs::create_dir_all(root.join("payload data")));
    checked(std::fs::write(root.join("payload data/data.txt"), "stale"));
    checked(std::fs::create_dir(root.join("payload")));
    let result = replay(removal, &root);
    assert!(
        root.join("payload").exists() && !root.join("payload data").exists(),
        "cleanup command={removal:?}; guard_exists={}; stale_exists={}; status={:?}",
        root.join("payload").exists(),
        root.join("payload data").exists(),
        result.status.code()
    );
    assert_eq!(result.status.code(), Some(0), "{removal}");
    // An absent destination, the usual case, is no failure.
    assert_eq!(replay(removal, &root).status.code(), Some(0), "{removal}");
}

/// N41: Kotlin removed a pushed folder named `payload data` after the batch with `rm -r /sdcard/payload data`, deleting a
/// sibling `payload`. A path that needs no quoting keeps Kotlin's command byte for byte.
#[test]
fn the_removal_after_a_batch_quotes_only_a_path_that_needs_it_d27() {
    let (_temp, sent) = folder_push_commands("payload");
    assert_eq!(
        sent.last().map(String::as_str),
        Some("rm -r /sdcard/payload"),
        "{sent:?}"
    );
    let (temp, sent) = folder_push_commands("payload data");
    let removal = sent.last().unwrap();
    assert_eq!(removal, "rm -r '/sdcard/payload data'", "{sent:?}");
    let root = temp.path().join("device");
    checked(std::fs::create_dir_all(root.join("payload data")));
    checked(std::fs::write(root.join("payload data/data.txt"), "pushed"));
    checked(std::fs::create_dir(root.join("payload")));
    assert_eq!(replay(removal, &root).status.code(), Some(0), "{removal}");
    assert!(root.join("payload").exists() && !root.join("payload data").exists());
}

/// x37 review: `-e` follows a link, so a dangling link that `rm` failed to remove read as gone, and the push went on.
#[cfg(unix)]
#[test]
fn a_dangling_link_left_by_a_failed_removal_fails_the_push_d27() {
    let (temp, sent) = folder_push_commands("payload");
    let removal = sent.iter().find(|c| c.starts_with("rm -r ")).unwrap();
    let root = temp.path().join("device");
    checked(std::fs::create_dir(&root));
    let link = root.join("payload");
    checked(std::os::unix::fs::symlink(root.join("gone"), &link));
    // `rm` failing, as it would for lack of permission.
    let failed = replay(&format!("rm() {{ return 1; }}; {removal}"), &root);
    assert_ne!(failed.status.code(), Some(0), "{removal}");
    assert!(link.symlink_metadata().is_ok());
    // A working `rm` removes the link.
    assert_eq!(replay(removal, &root).status.code(), Some(0), "{removal}");
    assert!(link.symlink_metadata().is_err());
}

/// x37: a leftover that can't be removed used to stay what the tests read, with the fresh folder pushed inside it.
#[test]
fn a_folder_push_fails_when_its_leftover_cannot_be_removed_d27() {
    let mut c = configuration();
    let temp = output(&mut c);
    let folder = temp.path().join("payload");
    checked(std::fs::create_dir(&folder));
    checked(std::fs::write(folder.join("data.txt"), "fresh"));
    android(&mut c).file_sync_configuration.push.push(FilePushEntry {
        path: js(&folder.to_string_lossy()),
        path_root: PathRoot::ExternalStorage,
    });
    run(c, move |runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            let mut script = base_script(27, vec![]);
            script["devices"][0]["files"] = json!([
                {"path":"/sdcard/payload","dir":true},
                {"path":"/sdcard/payload/data.txt","content":"stale"}
            ]);
            script["shell"].as_array_mut().unwrap().extend([
                json!({"exact":"rm -r -- '/sdcard/payload' || { [ ! -e '/sdcard/payload' ] && [ ! -L '/sdcard/payload' ]; }","stdout":"rm: /sdcard/payload/data.txt: Permission denied\n","exit_code":1}),
                json!({"prefix":"mkdir","stdout":""})
            ]);
            let server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            let listener = FileSyncTestRunListener::new(artifacts(&device));
            let outcome = listener.before_test_run_in(None, context.clone()).await;
            let root_data = server
                .file("emulator-5554", "/sdcard/payload/data.txt")
                .unwrap()
                .content
                .unwrap();
            let nested = server
                .file("emulator-5554", "/sdcard/payload/payload/data.txt")
                .is_some();
            finish(server, device).await;
            let error = outcome
                .err()
                .unwrap_or_else(|| panic!("setup succeeded: root_data={root_data:?}, fresh_nested={nested}"));
            assert_eq!(
                error.failure().message.as_ref().map(|m| m.to_owned()).as_deref(),
                Some(
                    "Could not delete remote file(s): /sdcard/payload: exit code 1: \
                     rm: /sdcard/payload/data.txt: Permission denied"
                )
            );
            assert!(!nested, "the fresh folder was pushed inside the leftover");
            Ok(())
        })
    });
}
