//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/RemoteFileManager.kt
//! Supplemental rule tests: this source has no Kotlin test rows.
use marathon_error::FailureKind;
mod support;
use jcompat::logging::{Level, Logger, MarathonLogging};
use marathon_apple::{
    apple_device::apple_arch,
    cmd::{CommandResult, cancelled, failure, remote::ssh::sshj::config::SshLogging},
    model::Sdk,
    remote_file_manager::RemoteFileManager,
    worker_commands::WorkerCommands,
};
use marathon_config::vendor::ScreenshotType;
use marathon_core::{
    error::{VendorError, VendorResult},
    model::Test,
    runtime::VendorFuture,
    vendor::VendorContext,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct Scripted {
    mode: String,
    argv: Mutex<Vec<Vec<String>>>,
}
impl WorkerCommands for Scripted {
    fn execute_worker_command<'a>(
        &'a self,
        command: Vec<String>,
        _: &'a VendorContext,
    ) -> VendorFuture<'a, Option<CommandResult>> {
        self.argv.lock().unwrap().push(command);
        Box::pin(async move {
            match self.mode.as_str() {
                "null" => Ok(None),
                "io" => Err(VendorError::Other(failure(FailureKind::Io, "probe io"))),
                "cancel" => Err(VendorError::Cancelled(cancelled("probe cancel"))),
                mode => Ok(Some(CommandResult {
                    stdout: vec![" stdout ".into()],
                    stderr: vec![
                        match mode {
                            "blank" => "\u{a0}\t\n ",
                            "stderr" => " \u{a0}problem\u{a0} ",
                            _ => "",
                        }
                        .into(),
                    ],
                    exit_code: match mode {
                        "nonzero" => Some(1),
                        "timeout" => None,
                        "143" => Some(143),
                        _ => Some(0),
                    },
                })),
            }
        })
    }
}
type Logs = Arc<Mutex<Vec<Value>>>;
fn setup(storage: &str, mode: &str) -> (RemoteFileManager, Arc<Scripted>, Logs) {
    let commands = Arc::new(Scripted {
        mode: mode.into(),
        argv: Mutex::new(vec![]),
    });
    let logs = Arc::new(Mutex::new(vec![]));
    let out = logs.clone();
    let logging = SshLogging::new(
        Arc::new(Mutex::new(MarathonLogging::default())),
        Arc::new(move |logger: &Logger, level, message, cause| {
            out.lock().unwrap().push(json!({"logger":logger.name(),"level":if level == Level::Error { "ERROR" } else { "unexpected" },"message":message,"throwable":cause.map(|f| f.kind.legacy_class().to_owned())}));
        }),
    );
    (
        RemoteFileManager::new(storage.into(), "U".into(), commands.clone(), logging),
        commands,
        logs,
    )
}
fn rows() -> impl Iterator<Item = Value> {
    include_str!("../../../fixtures/oracle/apple-remote-file-manager.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
}
fn string(value: &Value) -> &str {
    value.as_str().unwrap()
}
fn units(value: &Value) -> String {
    String::from_utf16_lossy(
        &value
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_u64().unwrap() as u16)
            .collect::<Vec<_>>(),
    )
}

#[test]
fn paths_and_utf16_video_names_match_jvm() {
    for row in rows() {
        match string(&row["kind"]) {
            "paths" => {
                let (fm, _, _) = setup(string(&row["storage"]), "success");
                for (key, expected) in row["paths"].as_object().unwrap() {
                    let actual = match key.as_str() {
                        "directory" => fm.remote_directory().into(),
                        "root" => fm.remote_test_root().into(),
                        "shared" => fm.remote_shared_directory().into(),
                        "media" => fm.remote_shared_media_directory().into(),
                        "xctestrun" => fm.remote_xctestrun_file(),
                        "xctest" => fm.remote_xctest_file(),
                        "runner" => fm.remote_test_runner_application(),
                        "parser" => fm.remote_xctest_parser_file(),
                        "app" => fm.remote_application(),
                        "log" => fm.remote_log(),
                        "pid" => fm.remote_video_pidfile(),
                        "xcresult" => fm.remote_xcresult_for_id("b/id/").to_owned(),
                        "xctestrunName" => fm.xctestrun_file_name().into(),
                        "appName" => fm.app_under_test_file_name().into(),
                        "runnerName" => fm.test_runner_file_name().into(),
                        key if key.starts_with("screenshot/") => fm.remote_screenshot(
                            "Q",
                            match &key[11..] {
                                "png" => ScreenshotType::Png,
                                "tiff" => ScreenshotType::Tiff,
                                "bmp" => ScreenshotType::Bmp,
                                "jpeg" => ScreenshotType::Jpeg,
                                _ => panic!("{key}"),
                            },
                        ),
                        key if key.starts_with("extra/") => fm.remote_extra_application(&key[6..]),
                        key if key.starts_with("media/") => fm.remote_media_file(&key[6..]),
                        key if key.starts_with("parent/") => fm.parent_of(&key[7..]).into(),
                        key if key.starts_with("join/") => fm.join_path(string(&row["storage"]), &[&key[5..], "/", ""]),
                        _ => panic!("{key}"),
                    };
                    assert_eq!(actual, string(expected), "{row:?}, {key}");
                }
            }
            "video" => {
                let (fm, _, _) = setup(string(&row["storage"]), "success");
                let test = Test {
                    pkg: units(&row["pkg"]),
                    clazz: units(&row["clazz"]),
                    method: units(&row["method"]),
                    meta_properties: vec![],
                };
                assert_eq!(
                    fm.remote_video_for_test(&test, &units(&row["batch"])),
                    units(&row["video"])
                );
                assert_eq!(
                    fm.remote_temp_video_for_test(&test, &units(&row["batch"])),
                    units(&row["temp"])
                );
            }
            _ => {}
        }
    }
}
#[test]
fn architecture_is_recomputed_for_every_sdk_abi_and_udid() {
    for row in rows().filter(|r| r["kind"] == "arch") {
        let sdk = Sdk::ALL
            .into_iter()
            .find(|sdk| sdk.name() == string(&row["sdk"]))
            .unwrap();
        assert_eq!(
            apple_arch(sdk, string(&row["abi"]), string(&row["udid"])).value(),
            string(&row["arch"])
        );
    }
}
async fn invoke(fm: &RemoteFileManager, operation: &str, c: &VendorContext) -> VendorResult<Value> {
    match operation {
        "mkdir" => fm.create_remote_directory(None, c).await?,
        "mkdir-explicit" => fm.create_remote_directory(Some("/d/"), c).await?,
        "shared" => fm.create_remote_shared_directory(c).await?,
        "media" => fm.create_remote_shared_media_directory(c).await?,
        "remove" => fm.remove_remote_directory(&[], c).await?,
        "keep" => fm.remove_remote_directory(&["/x/a*".into(), "/x/b/".into()], c).await?,
        "path" => fm.remove_remote_path("/p", c).await?,
        "copy" => fm.copy("/a", "/b", true, c).await?,
        "copy-no" => fm.copy("/a", "/b", false, c).await?,
        "move" => fm.move_file("/a", "/b", c).await?,
        "symlink" => fm.symlink("/a", "/b", true, c).await?,
        "symlink-no" => fm.symlink("/a", "/b", false, c).await?,
        "exists" => return Ok(json!(fm.exists("/p", c).await?)),
        "test" => return Ok(json!(fm.test("/p", c).await?)),
        _ => panic!("{operation}"),
    }
    Ok(json!("unit"))
}
#[tokio::test]
async fn every_command_result_and_log_matches_jvm_including_swallowed_cancellation() {
    for row in rows().filter(|r| r["kind"] == "command") {
        let (fm, commands, logs) = setup("/s", string(&row["mode"]));
        let result = invoke(&fm, string(&row["operation"]), &support::context()).await;
        let (result, thrown) = match result {
            Ok(value) => (value, Value::Null),
            Err(error) => (Value::Null, json!(error.failure().kind.legacy_class().to_owned())),
        };
        assert_eq!(result, row["result"], "{row}");
        assert_eq!(thrown, row["thrown"], "{row}");
        assert_eq!(json!(*commands.argv.lock().unwrap()), row["argv"], "{row}");
        assert_eq!(json!(*logs.lock().unwrap()), row["logs"], "{row}");
    }
}
