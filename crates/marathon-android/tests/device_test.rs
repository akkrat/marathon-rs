//! Port of vendor/vendor-android/src/test/kotlin/com/malinskiy/marathon/android/RemoteFileManagerTest.kt,
//! BaseAndroidDeviceTest.kt and VideoConfigurationTest.kt.
mod support;
use marathon_android::models::*;
use marathon_config::vendor::*;
use marathon_core::error::*;
use serde_json::json;
use support::*;
#[test]
fn test_create_remote_directory() {
    remote_command("mkdir /sdcard", 0)
}
#[test]
fn test_remove_remote_directory() {
    remote_command("rm -r /sdcard", 1)
}
#[test]
fn test_remove_remote_path() {
    remote_command("rm /sdcard/path", 2)
}
fn remote_command(cmd: &'static str, operation: u8) {
    run(configuration(), move |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(27, vec![]);
            script["shell"]
                .as_array_mut()
                .unwrap()
                .push(json!({"exact":cmd,"stdout":""}));
            let server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            match operation {
                0 => device.file_manager().create_remote_directory(None, context).await?,
                1 => device.file_manager().remove_remote_directory(context).await?,
                _ => {
                    device
                        .file_manager()
                        .remove_remote_path("/sdcard/path", false, context)
                        .await?
                }
            };
            let commands = commands(&server);
            expect_setup(&commands);
            assert_eq!(commands[6..], [cmd]);
            finish(server, device).await;
            Ok(())
        })
    });
}
/// D21: with the idle timeout in real milliseconds, silence under a configured timeout must still work: a v1
/// `screenrecord`, and every shell command up to `shell`. The same silence fails on the plain client.
#[test]
fn silent_requests_under_a_timeout_outlast_the_idle_limit_d21() {
    use marathon_adb::client::{
        AdbError, AndroidDebugBridgeClient, SocketFactory,
        device::{Device as AdbDevice, DeviceState},
        shell::v1::ShellCommandRequest,
    };
    use marathon_android::{device::AndroidDevice, tracker::DeviceStateTracker};
    run(configuration(), |runtime, context| {
        Box::pin(async move {
            let record = "screenrecord --size 720x1280 --bit-rate 1000000 --time-limit 180 /sdcard/v.mp4";
            let mut script = base_script(27, vec![]);
            for command in [record, "pm clear silent", "perfetto silent"] {
                script["shell"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"exact":command,"stdout":"","delay_ms":1000}));
            }
            let server = server(script).await;
            let client = AndroidDebugBridgeClient::new(
                server.addr().ip(),
                server.port(),
                std::sync::Arc::new(SocketFactory::new(10_000, 50)),
                None,
            );
            let tracker = std::sync::Arc::new(DeviceStateTracker::default());
            tracker.update(&[AdbDevice {
                serial: "emulator-5554".into(),
                state: DeviceState::Device,
            }]);
            let device = AndroidDevice::new(client.clone(), "emulator-5554".into(), tracker, runtime.clone());
            let error = client
                .execute(ShellCommandRequest::new("pm clear silent"), Some("emulator-5554"))
                .await
                .unwrap_err();
            assert_eq!(
                error,
                AdbError::RequestRejected("No exit code delimiter found in ".into())
            );
            assert!(
                device
                    .safe_execute_shell_command("pm clear silent", "", context.clone())
                    .await?
                    .is_some_and(|r| r.exit_code == 0)
            );
            device
                .critical_execute_shell_command("pm clear silent", "", context.clone())
                .await?;
            assert!(
                device
                    .execute_shell_command("perfetto silent", "", context.clone())
                    .await?
                    .is_some_and(|r| r.exit_code == 0)
            );
            device
                .safe_start_screen_recorder(
                    "/sdcard/v.mp4",
                    &runtime.configuration.screen_record_configuration.video_configuration,
                    context,
                )
                .await?;
            let mut server = server;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
#[test]
fn test_remote_video() {
    run(configuration(), |runtime, context| {
        Box::pin(async move {
            let server = server(base_script(27, vec![])).await;
            let device = device(&server, runtime);
            device.setup(context).await?;
            assert_eq!(
                device.file_manager().remote_video_for_test(&test(), &js("batch-id"))?,
                js("/sdcard/pkg.clazz-method-batch-id.mp4")
            );
            expect_setup(&commands(&server));
            finish(server, device).await;
            Ok(())
        })
    });
}
#[test]
fn test_remote_video_with_long_file_name() {
    run(configuration(), |runtime, context| {
        Box::pin(async move {
            let server = server(base_script(27, vec![])).await;
            let device = device(&server, runtime);
            device.setup(context).await?;
            let mut test = test();
            test.method = js(
                "testWithAVeryLongNameThatExceeds255CharactersqwertyuiopasdfghjklzxcvbnmQWERTYUIOPASDFGHJKLZXCVBNMqwertyuiopasdfghjklzxcvbnmQWERTYUIOPASDFGHJKLZXCVBNMqwertyuiopasdfghjklzxcvbnmQWERTYUIOPASDFGHJKLZXCVBNMqwertyuiopasdfghjklzxcvbnmQWERTYUIOPASDFGHJKLZXCVBNM",
            );
            assert_eq!(
                device.file_manager().remote_video_for_test(&test, &js("batch-id"))?,
                js(
                    "/sdcard/pkg.clazz-testWithAVeryLongNameThatExceeds255CharactersqwertyuiopasdfghjklzxcvbnmQWERTYUIOPASDFGHJKLZXCVBNMqwertyuiopasdfghjklzxcvbnmQWERTYUIOPASDFGHJKLZXCVBNMqwertyuiopasdfghjklzxcvbnmQWERTYUIOPASDFGHJKLZXCVBNMqwertyuiopasdfghjklzxcvbnmQWERT-batch-id.mp4"
                )
            );
            finish(server, device).await;
            Ok(())
        })
    });
}
#[test]
fn test_allure_configuration() {
    effective(true, false)
}
#[test]
fn test_codecov_configuration() {
    effective(false, true)
}
fn effective(allure: bool, coverage: bool) {
    let mut c = configuration();
    c.is_code_coverage_enabled = coverage;
    let a = android(&mut c);
    a.allure_configuration.enabled = allure;
    a.allure_configuration.relative_results_directory = js("/allure-results");
    a.allure_configuration.path_root = PathRoot::AppData;
    let expected = if allure {
        FileSyncEntry {
            relative_path: a.allure_configuration.relative_results_directory.clone(),
            path_root: a.allure_configuration.path_root,
            aggregation_mode: AggregationMode::TestRun,
        }
    } else {
        FileSyncEntry {
            relative_path: js("coverage"),
            path_root: PathRoot::AppData,
            aggregation_mode: AggregationMode::Pool,
        }
    };
    run(c, move |runtime, context| {
        Box::pin(async move {
            let server = server(base_script(27, vec![])).await;
            let device = device(&server, runtime.clone());
            device.setup(context).await?;
            assert_eq!(runtime.file_sync.pull, vec![expected]);
            expect_setup(&commands(&server));
            finish(server, device).await;
            Ok(())
        })
    });
}
#[test]
fn test_defaults() {
    let mut c = configuration();
    let video = &android(&mut c).screen_record_configuration.video_configuration;
    assert_eq!(
        to_screen_recorder_command(video, &js("/sdcard/video.mp4"), false),
        js("screenrecord --size 720x1280 --bit-rate 1000000 --time-limit 180 /sdcard/video.mp4")
    );
}
#[test]
fn test_long_video() {
    let mut c = configuration();
    let video = &mut android(&mut c).screen_record_configuration.video_configuration;
    video.time_limit = 200;
    assert_eq!(
        to_screen_recorder_command(video, &js("/sdcard/video.mp4"), false),
        js("screenrecord --size 720x1280 --bit-rate 1000000 --time-limit 180 /sdcard/video.mp4")
    );
}
#[test]
fn test_long_video_depends_on_api_level() {
    for (sdk, limit) in [(33, 180), (34, 200)] {
        run(configuration(), move |runtime, context| {
            Box::pin(async move {
                let server = server(base_script(sdk, vec![])).await;
                let device = device(&server, runtime.clone());
                device.setup(context).await?;
                let mut video = runtime
                    .configuration
                    .screen_record_configuration
                    .video_configuration
                    .clone();
                video.time_limit = 200;
                assert_eq!(
                    to_screen_recorder_command(&video, &js("/sdcard/video.mp4"), device.api_level() >= 34),
                    js(&format!(
                        "screenrecord --size 720x1280 --bit-rate 1000000 --time-limit {limit} /sdcard/video.mp4"
                    ))
                );
                expect_setup(&commands(&server));
                finish(server, device).await;
                Ok(())
            })
        });
    }
}
#[test]
fn shell_wrapper_semantics() {
    let mut c = configuration();
    android(&mut c).timeout_configuration.shell = jcompat::jtime::duration_of(0, 20_000_000).unwrap();
    run(c, |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(27, vec![]);
            script["shell"].as_array_mut().unwrap().extend([
                json!({"exact":"fail","stdout":"bad","exit_code":9}),
                json!({"exact":"hang","hang":true}),
            ]);
            let server = server(script).await;
            let device = device(&server, runtime);
            assert_eq!(
                device
                    .critical_execute_shell_command("fail", "ignored", context.clone())
                    .await?
                    .exit_code,
                9
            );
            assert!(
                device
                    .safe_execute_shell_command("hang", "", context.clone())
                    .await?
                    .is_none()
            );
            let err = device
                .critical_execute_shell_command("hang", "rejected", context.clone())
                .await
                .unwrap_err();
            assert_eq!(
                err.failure().class,
                js("com.malinskiy.marathon.android.exception.CommandRejectedException")
            );
            assert!(matches!(
                marathon_android::execution_error(err),
                VendorError::DeviceLost(_)
            ));
            finish(server, device).await;
            Ok(())
        })
    });
}
#[test]
fn setup_failure_shapes() {
    for mode in [0, 1, 2] {
        let mut c = configuration();
        android(&mut c).timeout_configuration.boot = jcompat::jtime::duration_of(0, 30_000_000).unwrap();
        android(&mut c).timeout_configuration.shell = jcompat::jtime::duration_of(0, 30_000_000).unwrap();
        run(c, move |runtime, context| {
            Box::pin(async move {
                let mut script = base_script(27, vec![]);
                match mode {
                    0 => {
                        script["devices"][0]["getprop"]
                            .as_object_mut()
                            .unwrap()
                            .remove("sys.boot_completed");
                    }
                    1 => script["devices"][0]["getprop"]["ro.build.version.sdk"] = json!("not-an-int"),
                    _ => {
                        script["shell"][1] = json!({"exact":"echo $EXTERNAL_STORAGE","hang":true});
                    }
                }
                let server = server(script).await;
                let device = device(&server, runtime);
                let error = device.setup(context).await.unwrap_err();
                if mode == 1 {
                    assert_eq!(error.failure().class, js("java.lang.NumberFormatException"))
                } else {
                    assert!(matches!(error, VendorError::DeviceSetup(_)))
                }
                finish(server, device).await;
                Ok(())
            })
        });
    }
}
#[test]
fn folder_pull_uses_shell_listing_breadth_first() {
    let temp = checked(tempfile::tempdir());
    let local = temp.path().to_string_lossy().to_string();
    run(configuration(), move |runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            let mut script = base_script(27, vec![]);
            script["devices"][0]["files"] = json!([{"path":"/remote/a","content":"A"},{"path":"/remote/sub/b","content":"B"},{"path":"/remote/empty","content":""}]);
            script["shell"].as_array_mut().unwrap().extend([
        json!({"exact":"ls -l /remote","stdout":"-rw-r--r-- 1 root root 1 2024-01-01 12:00 a\ndrwxr-xr-x 1 root root 0 2024-01-01 12:00 sub\n-rw-r--r-- 1 root root 0 2024-01-01 12:00 empty\n"}),json!({"exact":"ls -l /remote/sub","stdout":"-rw-r--r-- 1 root root 1 2024-01-01 12:00 b\n"})]);
            let server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            device.pull_folder("/remote", &local, context.clone()).await?;
            let commands = commands(&server);
            expect_setup(&commands);
            assert_eq!(commands[6..], ["ls -l /remote", "ls -l /remote/sub"]);
            assert_eq!(checked(tokio::fs::read(format!("{local}/a")).await), b"A");
            assert_eq!(checked(tokio::fs::read(format!("{local}/sub/b")).await), b"B");
            assert!(checked(tokio::fs::read(format!("{local}/empty")).await).is_empty());
            let err = device
                .pull_file("/missing", &format!("{local}/missing"), context)
                .await
                .unwrap_err();
            assert!(matches!(err, VendorError::Transfer(_)));
            finish(server, device).await;
            Ok(())
        })
    });
}
