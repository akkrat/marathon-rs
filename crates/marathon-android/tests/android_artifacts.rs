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
const PS_HEADER: &str = "USER PID PPID NAME\n";
const PS_OUTPUT: &str = "USER PID PPID NAME\nshell 123 1 screenrecord\nshell 456 1 grep screenrecord\n";
const PID_PROBE: &str = "if [ -d /proc/123 ]; then echo alive; else echo gone; fi";
const VIDEO_REMOTE: &str = "/sdcard/pkg.clazz-method-batch-0.mp4";

fn recorder_stop(script: &mut Value, ps: &str) {
    shell(script, ps, PS_OUTPUT);
    shell(script, "kill -2 123", "");
    shell(script, PID_PROBE, "gone");
}

type ArtifactLog = Arc<std::sync::Mutex<Vec<(String, marathon_logging::Level, String)>>>;

fn log_lines(runtime: &mut Arc<marathon_android::services::AndroidRuntime>) -> ArtifactLog {
    let lines = Arc::new(std::sync::Mutex::new(vec![]));
    let sink = lines.clone();
    Arc::get_mut(runtime).unwrap().log = Arc::new(move |logger, level, message, _| {
        sink.lock()
            .unwrap()
            .push((logger.target().to_owned(), level, message.to_owned()));
    });
    lines
}

fn extract_fixture_app_data(package: &str) -> String {
    format!(
        "probe=$(run-as {package} sh -c '[ -d /data/data/{package}/fixture4 ] || exit 44' 2>&1); check=$?; if [ \"$check\" -eq 44 ]; then echo marathon-app-data-absent; elif [ \"$check\" -ne 0 ]; then case \"$probe\" in *'not debuggable'*) echo marathon-app-data-refused;; esac; else set -o pipefail && mkdir -p /data/local/tmp/fixture4 && run-as {package} sh -c 'cd /data/data/{package}/fixture4 && tar cf - .' | tar xvf - -C /data/local/tmp/fixture4 && echo marathon-app-data-extracted; fi"
    )
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
                        commands(&server)[SETUP_COMMANDS.len()..],
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
                    commands(&server)[SETUP_COMMANDS.len()..],
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
        pull(false, false)
    }
    #[test]
    fn test_pull_folder_with_empty_file() {
        pull(true, false)
    }
    #[test]
    fn missing_app_data_is_quiet_and_is_not_deleted_d97() {
        pull(false, true)
    }
    fn pull(empty: bool, missing_app_data: bool) {
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
        run(c, move |mut runtime, context| {
            Box::pin(async move {
                let _temp = temp;
                let warnings = Arc::new(std::sync::Mutex::new(Vec::new()));
                let messages = warnings.clone();
                Arc::get_mut(&mut runtime).unwrap().log = Arc::new(move |_, level, message, _| {
                    if level == marathon_logging::Level::WARN && message.contains("archive extraction failed") {
                        messages.lock().unwrap().push(message.to_owned());
                    }
                });
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
                        let cmd = extract_fixture_app_data("com.example");
                        let reply = if missing_app_data {
                            "marathon-app-data-absent\n"
                        } else {
                            "marathon-app-data-extracted\n"
                        };
                        shell(&mut script, &cmd, reply);
                        expected.push(cmd);
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
                    if *name == "fixture4" && !missing_app_data {
                        let remove = "run-as com.example rm -R /data/data/com.example/fixture4";
                        shell(&mut script, remove, "");
                        expected.push(remove.into());
                    }
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
                assert_eq!(commands(&server)[SETUP_COMMANDS.len()..], expected);
                assert!(warnings.lock().unwrap().is_empty(), "{warnings:?}");
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
            let archives = [
                extract_fixture_app_data("com.example"),
                extract_fixture_app_data("other.app"),
            ];
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
                archives[0].clone(),
                archives[1].clone(),
                "ls -l /data/local/tmp/fixture4".to_owned(),
                "run-as com.example rm -R /data/data/com.example/fixture4".to_owned(),
                "run-as other.app rm -R /data/data/other.app/fixture4".to_owned(),
            ];
            let mut script = base_script(27, vec![]);
            for command in &expected {
                let reply = if archives.contains(command) {
                    "marathon-app-data-extracted\n"
                } else {
                    ""
                };
                shell(&mut script, command, reply);
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
            assert_eq!(commands(&server)[SETUP_COMMANDS.len()..], expected);
            finish(server, device).await;
            Ok(())
        })
    });
}
/// x98: runs `batches` file-sync batches of one APP_DATA folder on one fake-adb device whose app refuses `run-as`, and
/// returns what was logged about it and the commands sent after setup.
fn refusing_batches(batches: usize) -> (Vec<(marathon_logging::Level, String)>, Vec<String>) {
    let temp = checked(tempfile::tempdir());
    let mut c = configuration();
    c.output_dir = PathBuf::from(temp.path().to_string_lossy().into_owned());
    android(&mut c).file_sync_configuration.pull.push(FileSyncEntry {
        relative_path: js("fixture4"),
        path_root: PathRoot::AppData,
        aggregation_mode: AggregationMode::DeviceAndPool,
    });
    let logged = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (into, sent_into) = (logged.clone(), sent.clone());
    run(c, move |mut runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            Arc::get_mut(&mut runtime).unwrap().log = Arc::new(move |_, level, message, _| {
                if message.contains("not delivered") || message.contains("debuggable") {
                    into.lock().unwrap().push((level, message.to_owned()));
                }
            });
            let archive = extract_fixture_app_data("com.example");
            let mut script = base_script(27, vec![]);
            let rules = script["shell"].as_array_mut().unwrap();
            rules.push(json!({"prefix":"probe=$(run-as ","stdout":"marathon-app-data-refused\n"}));
            for prefix in ["rm ", "mkdir ", "run-as ", "ls -l "] {
                rules.push(json!({"prefix":prefix,"stdout":""}));
            }
            let server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            for _ in 0..batches {
                let listener = FileSyncTestRunListener::new(artifacts(&device));
                listener.before_test_run_in(Some(&info()), context.clone()).await?;
                listener.after_test_run_in(context.clone()).await?;
            }
            let commands = commands(&server)[SETUP_COMMANDS.len()..].to_vec();
            assert!(commands.contains(&archive), "{commands:?}");
            *sent_into.lock().unwrap() = commands;
            finish(server, device).await;
            Ok(())
        })
    });
    let logged = std::mem::take(&mut *logged.lock().unwrap());
    let sent = std::mem::take(&mut *sent.lock().unwrap());
    (logged, sent)
}

/// x98: a CI build is often not debuggable, so Android refuses `run-as` for it, in every batch. That is said once per
/// device and application, not as a failed extraction after every test, and nothing is deleted from the app.
#[test]
fn a_refused_run_as_is_reported_once_per_device_x98() {
    let (logged, sent) = refusing_batches(2);
    let batch = [
        "rm -r /data/local/tmp/fixture4".to_owned(),
        "mkdir /data/local/tmp/fixture4".to_owned(),
        "run-as com.example rm -R /data/data/com.example/fixture4".to_owned(),
        "run-as com.example mkdir /data/data/com.example/fixture4".to_owned(),
        extract_fixture_app_data("com.example"),
        "ls -l /data/local/tmp/fixture4".to_owned(),
    ];
    assert_eq!(sent, [batch.clone(), batch].concat());
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].0, marathon_logging::Level::WARN);
    assert!(logged[0].1.contains("com.example is not debuggable"), "{logged:?}");
    assert!(logged[0].1.contains("on device 127.0.0.1:"), "{logged:?}");
}

/// x98: the command marathon sends tells a refusal from other failures, an absent folder and a copy. It is replayed
/// with `bash` and a stand-in `run-as` on `PATH` that answers as Android's does.
#[test]
fn the_app_data_command_tells_a_refusal_from_other_failures_x98() {
    let (_, sent) = refusing_batches(1);
    let root = checked(tempfile::tempdir());
    let bin = root.path().join("bin");
    checked(std::fs::create_dir(&bin));
    let command = sent
        .iter()
        .find(|command| command.contains("marathon-app-data-extracted"))
        .unwrap()
        .replace("/data/", &format!("{}/data/", root.path().display()));
    let replay = |run_as: Option<&str>| {
        let tool = bin.join("run-as");
        let _ = std::fs::remove_file(&tool);
        if let Some(run_as) = run_as {
            checked(std::fs::write(&tool, format!("#!/bin/sh\n{run_as}\n")));
            checked(std::fs::set_permissions(
                &tool,
                std::os::unix::fs::PermissionsExt::from_mode(0o755),
            ));
        }
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let output = checked(
            std::process::Command::new("bash")
                .arg("-c")
                .arg(&command)
                .env("PATH", path)
                .output(),
        );
        let marker = String::from_utf8_lossy(&output.stdout)
            .lines()
            .last()
            .unwrap_or("")
            .to_owned();
        (output.status.success(), marker)
    };
    // Marathon reads a marker only from a command that exited 0.
    let marker = |run_as: Option<&str>| {
        let (success, marker) = replay(run_as);
        assert!(success, "{run_as:?} gave {marker:?} with a failed status");
        marker
    };
    let refuse = "echo \"run-as: package not debuggable: $1\" >&2; exit 1";
    let unknown = "echo \"run-as: unknown package: $1\" >&2; exit 1";
    let debuggable = "shift; exec \"$@\"";
    let probe_then_refuse =
        "case \"$*\" in *tar*) echo \"run-as: package not debuggable: $1\" >&2; exit 1;; esac; shift; exec \"$@\"";
    assert_eq!(marker(Some(refuse)), "marathon-app-data-refused");
    // Not a refusal: no marker, so marathon reports a failed extraction.
    assert_eq!(marker(Some(unknown)), "");
    assert_eq!(marker(None), "");
    assert_eq!(marker(Some(debuggable)), "marathon-app-data-absent");
    let source = root.path().join("data/data/com.example/fixture4");
    checked(std::fs::create_dir_all(&source));
    checked(std::fs::write(source.join("result.json"), "{}"));
    assert_eq!(marker(Some(refuse)), "marathon-app-data-refused");
    // A copy that fails after the folder was found is a failed extraction, whatever the reason: its status fails.
    assert!(!replay(Some(probe_then_refuse)).0);
    assert_eq!(marker(Some(debuggable)), "marathon-app-data-extracted");
    let copied = root.path().join("data/local/tmp/fixture4/result.json");
    assert_eq!(checked(std::fs::read_to_string(copied)), "{}");
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
                        .map(|p| json!({"path":p,"file":fixture("fake-adb/android-artifacts/sample.mp4")}))
                        .collect::<Vec<_>>()
                );
                for (i, p) in paths.iter().enumerate() {
                    script["shell"].as_array_mut().unwrap().push(json!({"exact":format!("screenrecord --size 720x1280 --bit-rate 1000000 --time-limit 180 {p}"),"hang":!chunked||i==1}));
                    shell(&mut script, &format!("rm {p}"), "");
                }
                recorder_stop(&mut script, "ps -A");
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
                    assert_eq!(
                        checked(tokio::fs::read(&a.file).await),
                        checked(tokio::fs::read(fixture("fake-adb/android-artifacts/sample.mp4")).await)
                    );
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
                expected.extend(["ps -A", "kill -2 123", PID_PROBE].map(str::to_owned));
                expected.extend(paths.iter().map(|p| format!("rm {p}")));
                assert_eq!(commands[SETUP_COMMANDS.len()..], expected);
                if run_failure {
                    let local = artifacts(&device)
                        .file(marathon_core::output::FileType::Video, None, None, Some(js("0")))
                        .await?;
                    assert_eq!(
                        checked(tokio::fs::read(local).await),
                        checked(tokio::fs::read(fixture("fake-adb/android-artifacts/sample.mp4")).await)
                    );
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
fn adam_extensions_require_a_delivered_file_to_attach_d97() {
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
                    AttachmentType::ScreenshotPng,
                    AttachmentType::ScreenshotWebp,
                    AttachmentType::ScreenshotGif
                ]
            );
            assert!(values.iter().all(|a| a.name == Some(js("screen"))));
            for (i, a) in values.iter().enumerate() {
                let file = a.file.clone();
                assert_eq!(checked(tokio::fs::read(&file).await), b"image");
                assert_eq!(file.extension().unwrap(), if i == 1 { "png" } else { "jpg" });
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
                runtime.component("FileManager"),
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
    run(c, move |runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            let base = fixture("fake-adb/android-artifacts");
            let raw = checked(tokio::fs::read_to_string(base.join(format!("{mode}.json"))).await);
            let mut input: Value = checked(serde_json::from_str(&raw));
            if mode.starts_with("video") {
                let rules = input["shell"].as_array_mut().unwrap();
                rules.retain(|rule| rule["exact"] != "ps -A | grep screenrecord" && rule["prefix"] != "screenrecord ");
                // D102: these script files model both recorders as hanging, and the Kotlin trace
                // never sees a PID. Model a normal first-chunk exit and an INT stop for the last one.
                if mode == "video-timeout" {
                    shell(
                        &mut input,
                        "screenrecord --size 720x1280 --bit-rate 1000000 --time-limit 180 /sdcard/pkg.clazz-method-batch-0.mp4",
                        "",
                    );
                }
                input["shell"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"prefix":"screenrecord ","hang":true}));
                recorder_stop(&mut input, "ps -A");
            }
            let script = checked(marathon_adb::server::Script::from_yaml(&input.to_string()));
            let mut server = checked(FakeAdbServer::start(checked(script.compile(&base)), Default::default()).await);
            server.capture_output();
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
            let mut reference = services(reference);
            if mode.starts_with("video") {
                reference = d102_video_stop_reference(reference).expect("unrecognized JVM recorder stop");
            }
            if mode == "filesync-coverage" {
                // D81 allowance: the file-sync listener no longer handles coverage (the coverage listener does, without
                // `run-as`). Kotlin's coverage rows go: its shell commands on the coverage folders and the two sync
                // sessions that pulled the coverage file; `/sdcard/screenshots` is empty and pulls nothing.
                reference.retain(|s| !s.contains("coverage") && s != "sync:");
            }
            let actual = server.transcript();
            let omitted = d85_omitted_connections(&actual);
            let actual = services(
                actual
                    .into_iter()
                    .filter(|r| !r["conn"].as_u64().is_some_and(|conn| omitted.contains(&conn)))
                    .collect(),
            );
            assert_eq!(actual, reference, "{mode}");
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
fn d102_video_stop_reference(mut reference: Vec<String>) -> Option<Vec<String>> {
    let old = "shell:ps -A | grep screenrecord;echo x$?";
    let index = reference.iter().position(|service| service == old)?;
    if reference.iter().skip(index + 1).any(|service| service == old)
        || !reference.get(index.checked_sub(1)?)?.starts_with("shell:screenrecord ")
        || reference.get(index + 1)? != "sync:"
    {
        return None;
    }
    reference.splice(
        index..=index,
        ["ps -A", "kill -2 123", PID_PROBE].map(|cmd| format!("shell:{cmd};echo x$?")),
    );
    Some(reference)
}

#[test]
fn d102_video_allowance_only_rewrites_the_recorded_stop_slot() {
    let start = "shell:screenrecord --time-limit 180 /sdcard/test.mp4;echo x$?";
    let stop = "shell:ps -A | grep screenrecord;echo x$?";
    let as_vec = |rows: &[&str]| rows.iter().map(|row| row.to_string()).collect();
    let expected =
        d102_video_stop_reference(as_vec(&[start, stop, "sync:", "shell:rm /sdcard/test.mp4;echo x$?"])).unwrap();
    assert_eq!(expected[2], "shell:kill -2 123;echo x$?");
    assert_eq!(expected[5], "shell:rm /sdcard/test.mp4;echo x$?");
    assert!(d102_video_stop_reference(as_vec(&[start, stop, stop, "sync:"])).is_none());
    assert!(d102_video_stop_reference(as_vec(&[stop, start, "sync:"])).is_none());
    assert!(d102_video_stop_reference(as_vec(&[start, stop, "shell:rm other;echo x$?", "sync:"])).is_none());
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
            assert!(commands(&server)[SETUP_COMMANDS.len()..].is_empty());
            listener.on_event_in(&start(), context.clone()).await?;
            started(&server, "shell:echo 'null'", 1).await;
            listener.on_event_in(&run_failed(), context.clone()).await?;
            listener.on_event_in(&run_failed(), context.clone()).await?;
            listener.cleanup_in(context).await?;
            assert!(attachments.get(&test()).is_none());
            assert_eq!(
                commands(&server)[SETUP_COMMANDS.len()..],
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
            script["devices"][0]["files"] = json!([{"path":VIDEO_REMOTE,"file":fixture("fake-adb/android-artifacts/sample.mp4")},{"path":"/sdcard/capture.png","content":"png"},{"path":"/data/misc/perfetto-traces/pkg.clazz-method-batch.perfetto-trace","content":"trace"}]);
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
                ("rm /sdcard/pkg.clazz-method-batch-0.mp4", ""),
                ("killall perfetto", ""),
                (
                    "rm -f /data/misc/perfetto-traces/pkg.clazz-method-batch.perfetto-trace",
                    "",
                ),
            ] {
                shell(&mut script, cmd, out);
            }
            recorder_stop(&mut script, "ps -A");
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
fn d102_stop_signals_the_recorder_once_and_waits_before_pull_without_warning() {
    run(configuration(), |mut runtime, context| {
        Box::pin(async move {
            let logs = log_lines(&mut runtime);
            let mut script = base_script(25, vec![]);
            let rules = script["shell"].as_array_mut().unwrap();
            rules.extend([
                json!({"prefix":"screenrecord ","hang":true}),
                json!({"exact":PID_PROBE,"stdout":"alive","times":1}),
                json!({"exact":PID_PROBE,"output":[{"gate":"recorder-exited"},{"stdout":"gone"}],"times":1}),
            ]);
            shell(&mut script, "ps", PS_OUTPUT);
            shell(&mut script, "kill -2 123", "");
            shell(&mut script, &format!("rm {VIDEO_REMOTE}"), "");
            script["devices"][0]["files"] =
                json!([{"path":VIDEO_REMOTE,"file":fixture("fake-adb/android-artifacts/sample.mp4")}]);
            let server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            let listener = Arc::new(ScreenRecorderTestBatchListener::new(
                artifacts(&device),
                ScreenRecordingPolicy::OnAny,
            ));
            let collected = collector(listener.clone());
            listener.on_event_in(&start(), context.clone()).await?;
            started(&server, "shell:screenrecord", 1).await;
            let event = end();
            let mut ended = listener.on_event_in(&event, context);
            tokio::select! {
                result = &mut ended => panic!("test end before recorder exit: {result:?}"),
                () = started(&server, PID_PROBE, 2) => {}
            }
            assert!(!request_services(&server).contains(&"sync:".into()));
            assert!(collected.get(&test()).is_none());
            server.open_gate("recorder-exited");
            ended.await?;
            let commands = commands(&server);
            assert_eq!(
                commands[SETUP_COMMANDS.len() + 1..],
                ["ps", "kill -2 123", PID_PROBE, PID_PROBE, &format!("rm {VIDEO_REMOTE}")]
            );
            assert_eq!(collected.get(&test()).unwrap().len(), 1);
            assert!(
                !logs
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(_, level, _)| *level == marathon_logging::Level::WARN)
            );
            finish(server, device).await;
            Ok(())
        })
    });
}

#[test]
fn d102_missing_or_unconfirmed_pid_does_not_pull_the_pending_recording() {
    for missing in [true, false] {
        run(configuration(), move |mut runtime, context| {
            Box::pin(async move {
                let logs = log_lines(&mut runtime);
                let mut script = base_script(33, vec![]);
                script["shell"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"prefix":"screenrecord ","hang":true}));
                if missing {
                    shell(&mut script, "ps -A", PS_HEADER);
                } else {
                    script["shell"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"exact":"ps -A","stdout":PS_OUTPUT,"times":1}));
                    shell(&mut script, "ps -A", PS_HEADER); // D92's later cleanup.
                    shell(&mut script, "kill -2 123", "");
                    shell(&mut script, PID_PROBE, "alive");
                }
                let server = server(script).await;
                let device = device(&server, runtime);
                device.setup(context.clone()).await?;
                let listener = Arc::new(ScreenRecorderTestBatchListener::new(
                    artifacts(&device),
                    ScreenRecordingPolicy::OnAny,
                ));
                let collected = collector(listener.clone());
                listener.on_event_in(&start(), context.clone()).await?;
                started(&server, "shell:screenrecord", 1).await;
                checked(
                    tokio::time::timeout(
                        std::time::Duration::from_secs(15),
                        listener.on_event_in(&end(), context),
                    )
                    .await,
                )?;
                assert!(collected.get(&test()).is_none());
                assert!(!request_services(&server).contains(&"sync:".into()));
                let commands = commands(&server);
                assert_eq!(commands.iter().filter(|cmd| *cmd == "ps -A").count(), 1);
                assert_eq!(
                    commands.iter().filter(|cmd| *cmd == "kill -2 123").count(),
                    usize::from(!missing)
                );
                assert_eq!(
                    logs.lock()
                        .unwrap()
                        .iter()
                        .filter(|(_, level, message)| *level == marathon_logging::Level::WARN
                            && message == "No screen recorder was found to stop")
                        .count(),
                    usize::from(missing)
                );
                finish(server, device).await;
                Ok(())
            })
        });
    }
}

#[test]
fn android_movies_publish_only_after_validating_the_staged_bytes_u273() {
    for (bytes, run_failure, why) in [
        ("", false, "the recording is empty"),
        ("short", true, "the recording is not a complete movie (it is cut short)"),
    ] {
        let mut c = configuration();
        let temp = output(&mut c);
        run(c, move |mut runtime, context| {
            Box::pin(async move {
                let _temp = temp;
                let logs = log_lines(&mut runtime);
                let mut script = base_script(33, vec![]);
                script["devices"][0]["files"] = json!([{"path": VIDEO_REMOTE, "content":bytes}]);
                script["shell"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"prefix":"screenrecord ","hang":true}));
                recorder_stop(&mut script, "ps -A");
                shell(&mut script, "echo quickhealthcheck", "quickhealthcheck");
                shell(&mut script, &format!("rm {VIDEO_REMOTE}"), "");
                let server = server(script).await;
                let device = device(&server, runtime);
                device.setup(context.clone()).await?;
                let a = artifacts(&device);
                let path = a
                    .file(
                        marathon_core::output::FileType::Video,
                        (!run_failure).then_some(&test()),
                        None,
                        Some("0".into()),
                    )
                    .await?;
                if run_failure {
                    checked(tokio::fs::write(&path, b"prior movie").await);
                }
                let listener = Arc::new(ScreenRecorderTestBatchListener::new(a, ScreenRecordingPolicy::OnAny));
                let collected = collector(listener.clone());
                listener.on_event_in(&start(), context.clone()).await?;
                started(&server, "shell:screenrecord", 1).await;
                listener
                    .on_event_in(&if run_failure { run_failed() } else { end() }, context)
                    .await?;
                assert!(collected.get(&test()).is_none());
                if run_failure {
                    assert_eq!(checked(tokio::fs::read(path).await), b"prior movie");
                } else {
                    assert!(!path.exists());
                }
                assert!(
                    server
                        .transcript()
                        .iter()
                        .any(|row| row["op"] == "RECV" && row["path"] == VIDEO_REMOTE)
                );
                assert!(!commands(&server).iter().any(|cmd| cmd.starts_with("rm ")));
                let diagnostics = logs
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, _, line)| line.contains("was not delivered"))
                    .map(|(_, _, line)| line.clone())
                    .collect::<Vec<_>>();
                assert_eq!(diagnostics, [format!("Video {VIDEO_REMOTE} was not delivered: {why}")]);
                finish(server, device).await;
                Ok(())
            })
        });
    }
}

#[test]
fn no_frames_means_no_gif_attachment_or_file_u274() {
    let mut c = configuration();
    let temp = output(&mut c);
    let timeouts = &mut android(&mut c).timeout_configuration;
    timeouts.screencapturer = jcompat::jtime::Duration::ZERO;
    run(c, move |runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            let server = server(base_script(33, vec![])).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            let a = artifacts(&device);
            let path = a
                .file(marathon_core::output::FileType::Screenshot, Some(&test()), None, None)
                .await?;
            let listener = Arc::new(ScreenCapturerTestRunListener::new(a, ScreenRecordingPolicy::OnAny));
            let collected = collector(listener.clone());
            listener.on_event_in(&start(), context.clone()).await?;
            until(|| path.exists()).await;
            listener.on_event_in(&end(), context).await?;
            assert_eq!(listener.captures_taken(), 0);
            assert!(collected.get(&test()).is_none());
            assert!(!path.exists());
            finish(server, device).await;
            Ok(())
        })
    });
}

#[test]
fn screenshot_listeners_log_on_their_android_targets_u275() {
    let mut c = configuration();
    let temp = output(&mut c);
    android(&mut c).timeout_configuration.screencapturer = jcompat::jtime::Duration::ZERO;
    run(c, move |mut runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            let logs = log_lines(&mut runtime);
            let server = server(base_script(33, vec![])).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            let a = artifacts(&device);
            let gif = ScreenCapturerTestRunListener::new(a.clone(), ScreenRecordingPolicy::OnAny);
            let adam = AdamScreenCaptureTestRunListener::new(a);
            gif.on_event_in(&start(), context.clone()).await?;
            gif.on_event_in(&end(), context.clone()).await?;
            let mut metrics = MetricMap::new();
            metrics.insert(js(METRIC), js("/sdcard/capture.unknown"));
            adam.on_event_in(&E::TestEnded { id: id(), metrics }, context).await?;
            let lines = logs
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, _, message)| {
                    message.starts_with("Starting recording for ")
                        || message.starts_with("Finished recording for ")
                        || message.starts_with("Unable to decode image format")
                })
                .map(|(target, _, _)| target.clone())
                .collect::<Vec<_>>();
            assert_eq!(
                lines,
                [
                    "marathon_android::artifact_listeners::screenshot",
                    "marathon_android::artifact_listeners::screenshot",
                    "marathon_android::artifact_listeners::adam_capture"
                ]
            );
            finish(server, device).await;
            Ok(())
        })
    });
}

#[test]
fn a_failed_recorder_stop_does_not_restart_probes_at_disposal_d92() {
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
            let server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            let listener = ScreenRecorderTestBatchListener::new(artifacts(&device), ScreenRecordingPolicy::OnAny);
            listener.on_event_in(&start(), context.clone()).await?;
            started(&server, "shell:screenrecord", 1).await;
            let cancelled = marathon_core::executor::Signal::new();
            cancelled.cancel(marathon_core::executor::shared::requested("stop was cancelled"));
            let outcome = listener
                .on_event_in(
                    &end(),
                    VendorContext {
                        cancellation: cancelled,
                        ..context
                    },
                )
                .await;
            assert!(matches!(outcome, Err(marathon_core::error::VendorError::Cancelled(_))));
            // The stop attempt joined its recorder. The strict fake refuses any later ps or kill command.
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
            shell(&mut script, "ps -A", PS_HEADER);
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
                shell(&mut script, "ps -A", PS_HEADER);
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
fn missing_and_timed_out_pulls_publish_no_video_capture_or_trace_attachments_d97() {
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
                    "rm /sdcard/pkg.clazz-method-batch-0.mp4",
                    "killall perfetto",
                    "rm -f /data/misc/perfetto-traces/pkg.clazz-method-batch.perfetto-trace",
                ] {
                    shell(&mut script, command, "");
                }
                script["shell"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"prefix":"screenrecord ","hang":true}));
                recorder_stop(&mut script, "ps -A");
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
                video.on_event_in(&start(), context.clone()).await?;
                started(&server, "shell:screenrecord", 1).await;
                video.on_event_in(&end(), context.clone()).await?;
                let mut metrics = MetricMap::new();
                metrics.insert(js(METRIC), js("/missing.png"));
                capture
                    .on_event_in(&E::TestEnded { id: id(), metrics }, context.clone())
                    .await?;
                trace.on_event_in(&end(), context).await?;
                assert!(collected.get(&test()).is_none());
                assert!(!commands(&server).iter().any(|c| c.starts_with("rm ")));
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
/// D81: a coverage file path per instrumentation; later instrumentations of a batch (D28) get their own name.
#[test]
fn coverage_file_names_and_commands_d81() {
    use marathon_android::artifact_listeners::coverage::CoverageFile;
    let first = CoverageFile::new("/sdcard/", "com.example", "b1", 0);
    assert_eq!(
        first.remote(),
        "/sdcard/Android/data/com.example/files/coverage/coverage-b1.ec"
    );
    assert_eq!(
        first.prepare_command(),
        "rm -rf /sdcard/Android/data/com.example/files/coverage; mkdir -p /sdcard/Android/data/com.example/files/coverage && { chmod o+rwx /sdcard/Android/data/com.example /sdcard/Android/data/com.example/files /sdcard/Android/data/com.example/files/coverage 2>/dev/null; true; }"
    );
    assert_eq!(
        first.remove_command(),
        "rm -rf /sdcard/Android/data/com.example/files/coverage"
    );
    assert_eq!(
        CoverageFile::new("/sdcard", "com.example", "b1", 1).name,
        "coverage-b1-2.ec"
    );
    assert_eq!(
        CoverageFile::new("/sdcard", "com.example", "b1", 2).name,
        "coverage-b1-3.ec"
    );
}
/// D81: every test the instrumentation started names the pulled file, and so does the merged result of a
/// parameterised test, which the results listener looks up without the index.
#[test]
fn coverage_is_attached_to_every_started_test_and_merged_parameterised_result_d81() {
    use marathon_android::artifact_listeners::coverage::{CoverageFile, CoverageTestRunListener};
    let mut c = configuration();
    let temp = output(&mut c);
    run(c, move |mut runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            let errors = Arc::new(std::sync::Mutex::new(Vec::new()));
            let messages = errors.clone();
            Arc::get_mut(&mut runtime).unwrap().log = Arc::new(move |_, level, message, _| {
                if level == marathon_logging::Level::ERROR && message.contains("coverage-batch-2.ec") {
                    messages.lock().unwrap().push(message.to_owned());
                }
            });
            let mut script = base_script(30, vec![]);
            script["devices"][0]["files"] = json!([{
                "path": "/sdcard/Android/data/com.example/files/coverage/coverage-batch.ec",
                "content": "jacoco"
            }]);
            shell(
                &mut script,
                "rm -rf /sdcard/Android/data/com.example/files/coverage",
                "",
            );
            let mut server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            let listener = Arc::new(CoverageTestRunListener::new(artifacts(&device)));
            let attachments = collector(listener.clone());
            let id = |name: &str| TestIdentifier {
                class_name: js("pkg.clazz"),
                test_name: js(name),
            };
            for name in ["method", "param[0]", "param[1]", "method"] {
                listener
                    .on_event_in(&E::TestStarted { id: id(name) }, context.clone())
                    .await?;
            }
            let file = CoverageFile::new("/sdcard", "com.example", "batch", 0);
            listener.on_coverage_in(&file, context.clone()).await?;
            let named = |method: &str| {
                attachments
                    .get(&Test {
                        pkg: js("pkg"),
                        clazz: js("clazz"),
                        method: js(method),
                        meta_properties: vec![],
                    })
                    .unwrap_or_default()
            };
            for method in ["method", "param[0]", "param[1]", "param"] {
                let found = named(method);
                assert_eq!(found.len(), 1, "{method}: {found:?}");
                assert_eq!(found[0].kind, AttachmentType::Coverage);
                assert!(found[0].file.ends_with("device-files/pool/coverage/coverage-batch.ec"));
                assert_eq!(checked(tokio::fs::read_to_string(&found[0].file).await), "jacoco");
            }
            // A second instrumentation's pull attaches only to the tests it started.
            let second = CoverageFile::new("/sdcard", "com.example", "batch", 1);
            listener.on_coverage_in(&second, context.clone()).await?;
            assert_eq!(named("method").len(), 1);
            assert_eq!(errors.lock().unwrap().len(), 1, "{errors:?}");
            checked(device.dispose().await);
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
