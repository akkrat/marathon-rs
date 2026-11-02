//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AdamDeviceProvider.kt
//! and AdamAndroidDevice.kt (Kotlin-first strict fake-ADB differential scenarios).
mod support;
use jcompat::file_manager::JavaPath;
use marathon_adb::server::{FakeAdbServer, Script, ServerOptions};
use marathon_android::provider::AdamDeviceProvider;
use marathon_core::vendor::{DeviceEvent, DeviceProvider};
use serde_json::Value;
use std::collections::BTreeMap;
use support::*;
fn transcript_commands(rows: &[Value]) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in rows {
        if row["type"] != "request" {
            continue;
        }
        let Some(service) = row["service"].as_str() else {
            continue;
        };
        if service == "shell:logcat -v long" || service.starts_with("host:transport:") {
            continue;
        }
        let serial = if let Some(serial) = row["serial"].as_str() {
            serial.to_owned()
        } else if let Some(rest) = service.strip_prefix("host-serial:") {
            rest.split(':').next().unwrap().to_owned()
        } else {
            continue;
        };
        out.entry(serial).or_default().push(service.to_owned());
    }
    out
}
fn expected(path: &str) -> Vec<Value> {
    checked(std::fs::read_to_string(fixture(path)))
        .lines()
        .map(|line| checked(serde_json::from_str(line)))
        .collect()
}
#[test]
fn kotlin_install_matrix() {
    let expected = expected("fake-adb/android-install-matrix/expected/transcript-1.jsonl");
    let script = checked(Script::load(&fixture(
        "fake-adb/android-install-matrix/script/devices.yaml",
    )));
    let mut c = configuration();
    let a = android(&mut c);
    a.application_output = Some(JavaPath::from_text(
        &fixture("vendor-android/apk/app-debug.apk").to_string_lossy(),
    ));
    a.test_application_output = Some(JavaPath::from_text(
        &fixture("vendor-android/apk/app-debug-androidTest.apk").to_string_lossy(),
    ));
    a.auto_grant_permission = true;
    a.install_options = js("-custom");
    run(c, move |runtime, context| {
        Box::pin(async move {
            let mut server = checked(FakeAdbServer::start(script, ServerOptions::default()).await);
            let runtime = change_runtime(&runtime, &[server.port()]);
            let provider = AdamDeviceProvider::new(runtime.clone())?;
            provider.initialize(context.clone()).await?;
            let mut devices = Vec::new();
            while devices.len() < 2 {
                let e = checked(checked(
                    tokio::time::timeout(std::time::Duration::from_secs(3), provider.subscribe().receive()).await,
                ))
                .unwrap();
                if let DeviceEvent::Connected(d) = e {
                    devices.push(d)
                }
            }
            devices.sort_by_key(|d| d.0.info().serial_number.clone());
            for d in &devices {
                d.0.prepare(runtime.dependencies.configuration.clone(), context.clone())
                    .await?;
                d.0.dispose().await?;
            }
            provider.terminate().await?;
            checked(server.shutdown().await);
            assert_eq!(server.warnings(), 0);
            assert_eq!(
                transcript_commands(&server.transcript()),
                transcript_commands(&expected)
            );
            for row in expected.iter().filter(|r| r["type"] == "sync" && r["op"] == "SEND") {
                assert!(server.transcript().iter().any(|r| r["type"] == "sync"
                    && r["op"] == row["op"]
                    && r["path"] == row["path"]
                    && r["mode"] == row["mode"]));
            }
            Ok(())
        })
    });
}
#[test]
fn kotlin_two_devices_setup_and_install_subset() {
    let original = expected("fake-adb/android-two-devices/expected/transcript.jsonl");
    let script = checked(Script::load(&fixture(
        "fake-adb/android-two-devices/script/devices.yaml",
    )));
    let mut c = configuration();
    let a = android(&mut c);
    a.application_output = Some(JavaPath::from_text(
        &fixture("vendor-android/apk/app-debug.apk").to_string_lossy(),
    ));
    a.test_application_output = Some(JavaPath::from_text(
        &fixture("vendor-android/apk/app-debug-androidTest.apk").to_string_lossy(),
    ));
    a.auto_grant_permission = false;
    run(c, move |runtime, context| {
        Box::pin(async move {
            let mut server = checked(FakeAdbServer::start(script, ServerOptions::default()).await);
            let runtime = change_runtime(&runtime, &[server.port()]);
            let provider = AdamDeviceProvider::new(runtime.clone())?;
            provider.initialize(context.clone()).await?;
            for _ in 0..2 {
                let event = checked(checked(
                    tokio::time::timeout(std::time::Duration::from_secs(3), provider.subscribe().receive()).await,
                ))
                .unwrap();
                let DeviceEvent::Connected(d) = event else {
                    panic!("connect")
                };
                d.0.prepare(runtime.dependencies.configuration.clone(), context.clone())
                    .await?;
                d.0.dispose().await?;
            }
            provider.terminate().await?;
            checked(server.shutdown().await);
            let actual = transcript_commands(&server.transcript());
            let expected = transcript_commands(&original);
            for (serial, commands) in actual {
                let expected = &expected[&serial];
                let through_clear = expected.iter().position(|s| s == "shell:logcat -c;echo x$?").unwrap();
                assert_eq!(commands, expected[..=through_clear]);
            }
            assert_eq!(server.warnings(), 0);
            Ok(())
        })
    });
}
#[test]
fn kotlin_multi_server_disconnect() {
    let scripts = ["server-1.yaml", "server-2.yaml"].map(|s| {
        checked(Script::load(&fixture(&format!(
            "fake-adb/android-provider-multi-server/script/{s}"
        ))))
    });
    let expected = [1, 2].map(|n| {
        expected(&format!(
            "fake-adb/android-provider-multi-server/expected/transcript-{n}.jsonl"
        ))
    });
    run(configuration(), move |runtime, context| {
        Box::pin(async move {
            let [s1, s2] = scripts;
            let mut first = checked(FakeAdbServer::start(s1, ServerOptions::default()).await);
            let mut second = checked(FakeAdbServer::start(s2, ServerOptions::default()).await);
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[first.port(), second.port()]))?;
            provider.initialize(context).await?;
            for _ in 0..2 {
                assert!(matches!(
                    checked(checked(
                        tokio::time::timeout(std::time::Duration::from_secs(3), provider.subscribe().receive()).await
                    )),
                    Some(DeviceEvent::Connected(_))
                ))
            }
            let event = checked(checked(
                tokio::time::timeout(std::time::Duration::from_secs(6), provider.subscribe().receive()).await,
            ));
            let Some(DeviceEvent::Disconnected(device)) = event else {
                panic!("disconnect")
            };
            assert!(!device.0.info().healthy);
            assert_eq!(
                device.0.info().network_state,
                marathon_core::model::NetworkState::Connected
            );
            provider.terminate().await?;
            checked(first.shutdown().await);
            checked(second.shutdown().await);
            assert_eq!(first.warnings() + second.warnings(), 0);
            assert_eq!(
                transcript_commands(&first.transcript()),
                transcript_commands(&expected[0])
            );
            assert_eq!(
                transcript_commands(&second.transcript()),
                transcript_commands(&expected[1])
            );
            Ok(())
        })
    });
}
