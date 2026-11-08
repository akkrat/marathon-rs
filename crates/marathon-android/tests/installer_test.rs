//! Port of vendor/vendor-android/src/test/kotlin/com/malinskiy/marathon/android/AndroidAppInstallerTest.kt
mod support;
use jcompat::file_manager::JavaPath;
use marathon_android::installer::AndroidAppInstaller;
use marathon_core::{error::*, vendor::Device};
use serde_json::json;
use support::*;
#[test]
fn test_clean_installation() {
    install_case(false, "", false, false, false, false)
}
#[test]
fn test_clean_installation_with_autogrant_permissions() {
    install_case(true, "", false, false, false, false)
}
#[test]
fn test_reinstall_with_extra_arguments() {
    install_case(false, "-custom", true, false, false, false)
}
#[test]
fn test_install_exception() {
    install_case(false, "", false, true, false, false)
}
#[test]
fn test_pm_list_exception() {
    install_case(false, "", false, false, true, false)
}
#[test]
fn test_installation_with_special_characters_in_path() {
    install_case(false, "", false, false, false, true)
}
fn install_case(grant: bool, options: &'static str, installed: bool, fail: bool, pm_fail: bool, special: bool) {
    let temp = checked(tempfile::tempdir());
    let app = if special {
        let p = temp.path().join("().apk");
        checked(std::fs::copy(fixture("vendor-android/apk/app-debug.apk"), &p));
        p
    } else {
        fixture("vendor-android/apk/app-debug.apk")
    };
    let test_apk = if special {
        let p = temp.path().join("()-androidTest.apk");
        checked(std::fs::copy(
            fixture("vendor-android/apk/app-debug-androidTest.apk"),
            &p,
        ));
        p
    } else {
        fixture("vendor-android/apk/app-debug-androidTest.apk")
    };
    let mut c = configuration();
    let a = android(&mut c);
    a.application_output = Some(JavaPath::from_text(&app.to_string_lossy()));
    a.test_application_output = Some(JavaPath::from_text(&test_apk.to_string_lossy()));
    a.auto_grant_permission = grant;
    a.install_options = js(options);
    a.timeout_configuration.shell = jcompat::jtime::duration_of(0, 100_000_000).unwrap();
    run(c, move |runtime, context| {
        Box::pin(async move {
            let _temp = temp;
            let mut script = base_script(27, vec![]);
            let name = if special { "--.apk" } else { "app-debug.apk" };
            let app_remote = format!("/data/local/tmp/{name}");
            let test_remote = if special {
                "/data/local/tmp/---androidTest.apk"
            } else {
                "/data/local/tmp/app-debug-androidTest.apk"
            };
            let params = format!(
                "-r{}{}",
                if grant { " -g" } else { "" },
                if options.is_empty() {
                    String::new()
                } else {
                    format!(" {options}")
                }
            );
            let shell = script["shell"].as_array_mut().unwrap();
            shell.extend([
            json!({"exact":"pm list packages","stdout":if installed{"package:com.example\npackage:com.example.test"}else{""},"hang":pm_fail}),
            json!({"exact":"pm uninstall com.example","stdout":"Great success"}),json!({"exact":"pm uninstall com.example.test","stdout":"Great success"}),
            json!({"exact":format!("md5 {app_remote}"),"stdout":"122fc3b5d69b262db9b84dfc00e8f1d4"}),
            json!({"exact":format!("pm install {params} {app_remote}"),"stdout":if fail{"Failure"}else{"Success"}}),json!({"exact":format!("rm {app_remote}"),"stdout":""}),
            json!({"exact":format!("md5 {test_remote}"),"stdout":"8d103498247b3711817a9f18624dede7"}),
            json!({"exact":format!("pm install {params} {test_remote}"),"stdout":"Success"}),json!({"exact":format!("rm {test_remote}"),"stdout":""})
        ]);
            let server = server(script).await;
            let device = device(&server, runtime.clone());
            device.setup(context.clone()).await?;
            let result = AndroidAppInstaller::new(runtime)
                .prepare_installation(&device, context)
                .await;
            if fail || pm_fail {
                assert!(matches!(result, Err(VendorError::DeviceSetup(_))), "{result:?}")
            } else {
                result?
            }
            let commands = commands(&server);
            expect_setup(&commands);
            let mut expected = vec![];
            for _ in 0..if fail || pm_fail { 3 } else { 1 } {
                expected.push("pm list packages".to_owned());
                if pm_fail {
                    continue;
                }
                if installed {
                    expected.push("pm uninstall com.example".into())
                }
                expected.extend([
                    format!("md5 {app_remote}"),
                    format!("pm install {params} {app_remote}"),
                    format!("rm {app_remote}"),
                ]);
            }
            if !fail && !pm_fail {
                expected.push("pm list packages".into());
                if installed {
                    expected.push("pm uninstall com.example.test".into())
                }
                expected.extend([
                    format!("md5 {test_remote}"),
                    format!("pm install {params} {test_remote}"),
                    format!("rm {test_remote}"),
                ]);
            }
            assert_eq!(commands[6..], expected);
            if !pm_fail {
                let file = server.file("emulator-5554", &app_remote).unwrap();
                assert_eq!(file.mode, 0o777);
            }
            finish(server, device).await;
            Ok(())
        })
    });
}
#[test]
fn test_clean_split_installation() {
    let mut c = configuration();
    let a = android(&mut c);
    let app = fixture("vendor-android/apk/base-en.apk");
    let split = fixture("vendor-android/apk/standalone-hdpi.apk");
    let total = checked(app.metadata()).len() + checked(split.metadata()).len();
    a.application_output = Some(JavaPath::from_text(&app.to_string_lossy()));
    a.split_apks = Some(vec![JavaPath::from_text(&split.to_string_lossy())]);
    a.test_application_output = Some(JavaPath::from_text(
        &fixture("vendor-android/apk/app-debug-androidTest.apk").to_string_lossy(),
    ));
    run(c, move |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(27, vec!["cmd"]);
            script["shell"].as_array_mut().unwrap().extend([
        json!({"exact":"pm list packages","stdout":""}),json!({"exact":format!("cmd package install-create '-S{total}' -r"),"service":"exec","stdout":"Success: created install session [demo-session-id]"}),json!({"prefix":"cmd package install-write","service":"exec","stdout":"Success"}),json!({"exact":"cmd package install-commit demo-session-id","service":"exec","stdout":"Success"}),json!({"prefix":"cmd package install -r -S","service":"exec","stdout":"Success"})]);
            let server = server(script).await;
            let device = device(&server, runtime.clone());
            device.setup(context.clone()).await?;
            AndroidAppInstaller::new(runtime)
                .prepare_installation(&device, context)
                .await?;
            let commands = commands(&server);
            expect_setup(&commands);
            assert_eq!(commands[6], "pm list packages");
            assert_eq!(commands[7], format!("cmd package install-create '-S{total}' -r"));
            assert!(commands[8].contains("base-en.apk"));
            assert!(commands[9].contains("standalone-hdpi.apk"));
            assert_eq!(commands[10], "cmd package install-commit demo-session-id");
            assert_eq!(commands[11], "pm list packages");
            assert!(commands[12].starts_with("cmd package install -r -S"));
            assert_eq!(commands.len(), 13);
            finish(server, device).await;
            Ok(())
        })
    });
}
#[test]
fn prepare_leaves_external_storage_alone_d27() {
    run(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(27, vec![]);
            // A strict fake with no rule for `rm -r /sdcard` or `mkdir /sdcard`.
            script["shell"]
                .as_array_mut()
                .unwrap()
                .push(json!({"exact":"logcat -c","stdout":""}));
            let server = server(script).await;
            let device = device(&server, runtime.clone());
            device.setup(context.clone()).await?;
            device
                .prepare(runtime.dependencies.configuration.clone(), context)
                .await?;
            let commands = commands(&server);
            expect_setup(&commands);
            assert_eq!(commands[6..], ["logcat -c"]);
            finish(server, device).await;
            Ok(())
        })
    });
}
#[test]
fn inverted_api21_check_rejects_plain_apk() {
    let mut c = configuration();
    let a = android(&mut c);
    a.application_output = Some(JavaPath::from_text(
        &fixture("vendor-android/apk/app-debug.apk").to_string_lossy(),
    ));
    a.test_application_output = Some(JavaPath::from_text(
        &fixture("vendor-android/apk/app-debug-androidTest.apk").to_string_lossy(),
    ));
    run(c, |runtime, context| {
        Box::pin(async move {
            let server = server(base_script(20, vec![])).await;
            let device = device(&server, runtime.clone());
            device.setup(context.clone()).await?;
            let err = AndroidAppInstaller::new(runtime)
                .prepare_installation(&device, context)
                .await
                .unwrap_err();
            assert_eq!(
                err.failure().class,
                js("com.malinskiy.marathon.android.exception.DeviceNotSupportedException")
            );
            assert_eq!(
                err.failure().message,
                Some(js("Device api level should be more then 20"))
            );
            assert_eq!(commands(&server).len(), 6);
            finish(server, device).await;
            Ok(())
        })
    });
}
/// Cancels the caller's signal when the md5 poll starts its second delay. A real-time cancel can
/// land while a request is half sent, which the fake adb server rightly records as a reset.
struct CancelOnSecondDelay {
    inner: std::sync::Arc<dyn marathon_core::runtime::VendorClock>,
    signal: std::sync::Arc<marathon_core::executor::Signal>,
    delays: std::sync::atomic::AtomicUsize,
}
impl marathon_core::runtime::VendorClock for CancelOnSecondDelay {
    fn delay(
        &self,
        millis: i64,
        cancellation: std::sync::Arc<dyn marathon_core::runtime::CancellationSignal>,
    ) -> marathon_core::runtime::VendorFuture<'_, ()> {
        if self.delays.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            self.signal
                .cancel(marathon_core::executor::shared::requested("stop md5 poll"));
        }
        self.inner.delay(millis, cancellation)
    }
    fn with_timeout<'a>(
        &'a self,
        parent: std::sync::Arc<dyn marathon_core::runtime::CancellationSignal>,
        millis: i64,
        body: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> marathon_core::runtime::VendorFuture<'a, ()> {
        self.inner.with_timeout(parent, millis, body)
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: std::sync::Arc<dyn marathon_core::runtime::CancellationSignal>,
        millis: i64,
        body: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> marathon_core::runtime::VendorFuture<'a, Option<()>> {
        self.inner.with_timeout_or_null(parent, millis, body)
    }
}
#[test]
fn md5_poll_is_cancelled_by_its_caller() {
    use marathon_core::executor::Signal;
    run(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(27, vec![]);
            script["shell"]
                .as_array_mut()
                .unwrap()
                .push(json!({"exact":"md5 /mismatch","stdout":"wrong"}));
            let server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            let signal = Signal::new();
            let clock = std::sync::Arc::new(CancelOnSecondDelay {
                inner: context.clock.clone(),
                signal: signal.clone(),
                delays: 0.into(),
            });
            let err = device
                .wait_for_remote_file_sync(
                    "expected",
                    "/mismatch",
                    marathon_core::vendor::VendorContext {
                        cancellation: signal,
                        clock,
                        ..context
                    },
                )
                .await
                .unwrap_err();
            assert!(matches!(err, VendorError::Cancelled(_)));
            assert!(commands(&server).iter().filter(|s| *s == "md5 /mismatch").count() >= 2);
            finish(server, device).await;
            Ok(())
        })
    });
}
/// D23 (N5): the md5 poll has a bound of its own, the push timeout, and the failure names the file.
#[test]
fn md5_poll_is_bounded_by_the_push_timeout_d23() {
    let mut c = configuration();
    android(&mut c).timeout_configuration.push_file = jcompat::jtime::duration_of(0, 200_000_000).unwrap();
    run(c, |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(27, vec![]);
            script["shell"]
                .as_array_mut()
                .unwrap()
                .push(json!({"exact":"md5 /mismatch","stdout":"wrong"}));
            let mut server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            let err = device
                .wait_for_remote_file_sync("expected", "/mismatch", context)
                .await
                .unwrap_err();
            assert!(matches!(err, VendorError::Transfer(_)), "{err:?}");
            assert_eq!(
                err.failure().class,
                js("com.malinskiy.marathon.exceptions.TransferException")
            );
            assert_eq!(
                err.failure().message,
                Some(js(&format!(
                    "Remote file /mismatch on device {} did not reach md5 expected within 200 ms",
                    device.serial_number().to_owned()
                )))
            );
            // Not `finish`: the real timeout can cancel an md5 mid-exchange, which the server warns about.
            checked(device.dispose().await);
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
/// D23: legacy install's own timeout around the push keeps its message when the md5 never matches.
#[test]
fn legacy_install_keeps_its_message_when_the_md5_never_matches_d23() {
    let mut c = configuration();
    android(&mut c).timeout_configuration.push_file = jcompat::jtime::duration_of(0, 500_000_000).unwrap();
    run(c, |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(27, vec![]);
            script["shell"]
                .as_array_mut()
                .unwrap()
                .push(json!({"exact":"md5 /data/local/tmp/sample-fake.apk","stdout":"wrong"}));
            let server = server(script).await;
            let device = device(&server, runtime);
            device.setup(context.clone()).await?;
            let path = fixture("adam/fixture/sample-fake.apk").to_string_lossy().to_string();
            let err = device.install_package(&path, true, &[], context).await.unwrap_err();
            assert_eq!(err.failure().message, Some(js(&format!("Timeout transferring {path}"))));
            finish(server, device).await;
            Ok(())
        })
    });
}
#[test]
fn install_timeout_messages() {
    for streaming in [false, true] {
        let mut c = configuration();
        let a = android(&mut c);
        a.timeout_configuration.install = jcompat::jtime::duration_of(0, 200_000_000).unwrap();
        run(c, move |runtime, context| {
            Box::pin(async move {
                let mut script = base_script(27, if streaming { vec!["cmd"] } else { vec![] });
                script["shell"].as_array_mut().unwrap().extend([
                    json!({"prefix":"cmd package install","service":"exec","hang":true}),
                    json!({"prefix":"pm install","hang":true}),
                    json!({"exact":"md5 /data/local/tmp/sample-fake.apk","stdout":"949a57f2f6fc5f328a39d7372c42e69d"}),
                ]);
                let server = server(script).await;
                let device = device(&server, runtime);
                device.setup(context.clone()).await?;
                let path = fixture("adam/fixture/sample-fake.apk").to_string_lossy().to_string();
                let err = device
                    .install_package(&path, true, &[], context.clone())
                    .await
                    .unwrap_err();
                assert_eq!(
                    err.failure().message,
                    Some(js(&format!(
                        "Timeout {} {path}",
                        if streaming { "installing" } else { "transferring" }
                    )))
                );
                let err = device
                    .install_split_packages(&[path], true, &[], context)
                    .await
                    .unwrap_err();
                assert_eq!(err.failure().message, Some(js("Timeout transferring absolutePath")));
                finish(server, device).await;
                Ok(())
            })
        });
    }
}
