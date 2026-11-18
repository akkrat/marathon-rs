//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AdamDeviceProvider.kt
//! and AdamAndroidDevice.kt (Kotlin-first strict fake-ADB differential scenarios).
mod support;
use marathon_adb::server::{FakeAdbServer, Script, ServerOptions};
use marathon_android::provider::AdamDeviceProvider;
use marathon_core::vendor::{DeviceEvent, DeviceProvider};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
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
/// D27: what a Kotlin recording runs in prepare and this port does not. Applied to the recordings only, so a native
/// run that wiped the storage again would differ from them.
const D27_WIPE: [&str; 2] = ["shell:rm -r /sdcard;echo x$?", "shell:mkdir /sdcard;echo x$?"];
fn kotlin_commands(rows: &[Value]) -> BTreeMap<String, Vec<String>> {
    let mut commands = transcript_commands(rows);
    for list in commands.values_mut() {
        list.retain(|service| !D27_WIPE.contains(&service.as_str()));
    }
    commands
}
fn native_commands(rows: &[Value]) -> BTreeMap<String, Vec<String>> {
    let omitted = d85_omitted_connections(rows);
    let requests: Vec<_> = rows
        .iter()
        .filter(|row| row["type"] == "request" && !row["conn"].as_u64().is_some_and(|conn| omitted.contains(&conn)))
        .cloned()
        .collect();
    transcript_commands(&requests)
}
#[test]
fn the_d27_allowance_covers_only_the_root_wipe() {
    let row = |service: &str| serde_json::json!({"type":"request","serial":"s","service":service});
    let rows = [
        row("shell:rm -r /sdcard;echo x$?"),
        row("shell:mkdir /sdcard;echo x$?"),
        row("shell:rm -r /sdcard/screenshots;echo x$?"),
        row("shell:mkdir /sdcard/screenshots;echo x$?"),
        row("shell:rm -r /storage/emulated/0;echo x$?"),
        row("shell:rm /sdcard;echo x$?"),
    ];
    assert_eq!(
        kotlin_commands(&rows)["s"],
        [
            "shell:rm -r /sdcard/screenshots;echo x$?",
            "shell:mkdir /sdcard/screenshots;echo x$?",
            "shell:rm -r /storage/emulated/0;echo x$?",
            "shell:rm /sdcard;echo x$?",
        ]
    );
}
#[test]
fn the_d85_allowance_only_covers_successful_setup_and_marker_commands() {
    use marathon_adb::client::shell::boot_marker::MarkerCommand;
    use serde_json::json;
    let props = "shell:getprop;echo x$?";
    let features = "host-serial:s:features";
    let other = "shell:cat /proc/sys/kernel/random/uuid;echo x$?";
    let commands = |values: &[(&str, &str)]| {
        let mut rows = vec![];
        for (conn, (service, stdout)) in values.iter().enumerate() {
            rows.push(json!({"type":"request","conn":conn,"serial":"s","service":service}));
            if let Some(command) = service
                .strip_prefix("shell:")
                .and_then(|cmd| cmd.strip_suffix(";echo x$?"))
            {
                rows.push(json!({"type":"shell","conn":conn,"command":command,"handler":"builtin",
                    "service":"shell","protocol":"raw","exit_code_trick":true,"exit_code":0,"hang":false}));
                rows.push(json!({"type":"output","conn":conn,"stream":"stdout","bytes":stdout.as_bytes()}));
            }
        }
        native_commands(&rows)["s"].clone()
    };
    let probe = (D85_BOOT_ID_SERVICE, "00000000-0000-4000-8000-000000000001\n");
    assert_eq!(commands(&[(props, ""), probe, (features, "")]), [props, features]);
    assert_eq!(
        commands(&[(props, ""), probe, probe, (features, "")]),
        [props, D85_BOOT_ID_SERVICE, D85_BOOT_ID_SERVICE, features]
    );
    assert_eq!(
        commands(&[(props, ""), (other, ""), (features, "")]),
        [props, other, features]
    );
    assert_eq!(
        commands(&[(props, ""), (features, ""), probe]),
        [props, features, D85_BOOT_ID_SERVICE]
    );
    assert_eq!(
        commands(&[(props, ""), (probe.0, "invalid"), (features, "")]),
        [props, probe.0, features]
    );

    let token = uuid::uuid!("00000000-0000-4000-8000-000000000002");
    let other_token = uuid::uuid!("00000000-0000-4000-8000-000000000003");
    let service = |command: MarkerCommand, token| format!("shell:{};echo x$?", command.command(token));
    let write = service(MarkerCommand::Write, token);
    let read = service(MarkerCommand::Read, token);
    let remove = service(MarkerCommand::Remove, token);
    let wrong_remove = service(MarkerCommand::Remove, other_token);
    let wrote = format!("{token}\n");
    let clear = "shell:logcat -c;echo x$?";
    assert_eq!(
        commands(&[(clear, ""), (&write, &wrote), (&read, ""), (&remove, "")]),
        [clear, &read]
    );
    assert_eq!(
        commands(&[(props, ""), (&write, &wrote), (&remove, "")]),
        [props, &write, &remove]
    );
    assert_eq!(
        commands(&[(clear, ""), (&write, "invalid"), (&remove, "")]),
        [clear, &write, &remove]
    );
    assert_eq!(
        commands(&[
            (clear, ""),
            (&write, &wrote),
            (&wrong_remove, ""),
            (&remove, ""),
            (&remove, "")
        ]),
        [clear, &wrong_remove, &remove]
    );
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
    a.application_output = Some(PathBuf::from(
        fixture("vendor-android/apk/app-debug.apk")
            .to_string_lossy()
            .into_owned(),
    ));
    a.test_application_output = Some(PathBuf::from(
        fixture("vendor-android/apk/app-debug-androidTest.apk")
            .to_string_lossy()
            .into_owned(),
    ));
    a.auto_grant_permission = true;
    a.install_options = js("-custom");
    run(c, move |runtime, context| {
        Box::pin(async move {
            let mut server = checked(FakeAdbServer::start(script, ServerOptions::default()).await);
            server.capture_output();
            let runtime = change_runtime(&runtime, &[server.port()]);
            let provider = AdamDeviceProvider::new(runtime.clone())?;
            provider.initialize(context.clone()).await?;
            let mut devices = Vec::new();
            while devices.len() < 2 {
                let e = checked(checked(
                    tokio::time::timeout(std::time::Duration::from_secs(60), provider.subscribe().receive()).await,
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
            assert_eq!(native_commands(&server.transcript()), kotlin_commands(&expected));
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
    a.application_output = Some(PathBuf::from(
        fixture("vendor-android/apk/app-debug.apk")
            .to_string_lossy()
            .into_owned(),
    ));
    a.test_application_output = Some(PathBuf::from(
        fixture("vendor-android/apk/app-debug-androidTest.apk")
            .to_string_lossy()
            .into_owned(),
    ));
    a.auto_grant_permission = false;
    run(c, move |runtime, context| {
        Box::pin(async move {
            let mut server = checked(FakeAdbServer::start(script, ServerOptions::default()).await);
            server.capture_output();
            let runtime = change_runtime(&runtime, &[server.port()]);
            let provider = AdamDeviceProvider::new(runtime.clone())?;
            provider.initialize(context.clone()).await?;
            for _ in 0..2 {
                let event = checked(checked(
                    tokio::time::timeout(std::time::Duration::from_secs(60), provider.subscribe().receive()).await,
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
            let actual = native_commands(&server.transcript());
            let expected = kotlin_commands(&original);
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
            first.capture_output();
            second.capture_output();
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[first.port(), second.port()]))?;
            provider.initialize(context).await?;
            for _ in 0..2 {
                assert!(matches!(
                    checked(checked(
                        tokio::time::timeout(std::time::Duration::from_secs(60), provider.subscribe().receive()).await
                    )),
                    Some(DeviceEvent::Connected(_))
                ))
            }
            let event = checked(checked(
                tokio::time::timeout(std::time::Duration::from_secs(60), provider.subscribe().receive()).await,
            ));
            let Some(DeviceEvent::Disconnected(device)) = event else {
                panic!("disconnect")
            };
            assert!(!device.0.info().healthy);
            assert_eq!(
                device.0.info().network_state,
                marathon_core::model::NetworkState::Disconnected
            );
            provider.terminate().await?;
            checked(first.shutdown().await);
            checked(second.shutdown().await);
            assert_eq!(first.warnings() + second.warnings(), 0);
            assert_eq!(native_commands(&first.transcript()), transcript_commands(&expected[0]));
            assert_eq!(native_commands(&second.transcript()), transcript_commands(&expected[1]));
            Ok(())
        })
    });
}
