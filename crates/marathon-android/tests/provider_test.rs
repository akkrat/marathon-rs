//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AdamDeviceProvider.kt
//! and DeviceStateTracker.kt (fake-server behavior witnesses); adam AdbBinaryIntegrationTest.kt.
mod support;
use jcompat::file_manager::JavaPath;
use marathon_adb::client::device::{Device as AdbDevice, DeviceState as S};
use marathon_android::{
    device::{detect_serial, host_address},
    provider::{AdamDeviceProvider, start_adb},
    tracker::{DeviceStateTracker, TrackingUpdate as U},
};
use marathon_config::vendor::SerialStrategy;
use marathon_core::{
    error::*,
    vendor::{DeviceEvent, DeviceProvider},
};
use std::collections::HashMap;
use support::*;
/// The loopback test releases a free port and expects it to stay free until the provider has
/// asked it for a version; other tests here bind ephemeral ports and could take it meanwhile.
/// It holds this lock exclusively; the others share it.
static PORTS: std::sync::RwLock<()> = std::sync::RwLock::new(());
fn shared_ports() -> std::sync::RwLockReadGuard<'static, ()> {
    PORTS.read().unwrap_or_else(|e| e.into_inner())
}
fn d(s: &str, state: S) -> AdbDevice {
    AdbDevice {
        serial: s.into(),
        state,
    }
}
#[test]
fn tracker_vanished_first_and_non_device_retention() {
    let _ports = shared_ports();
    let t = DeviceStateTracker::default();
    assert_eq!(
        t.update(&[d("a", S::Device), d("b", S::Offline)]),
        vec![("a".into(), U::Connected), ("b".into(), U::NothingToDo)]
    );
    assert_eq!(
        t.update(&[d("c", S::Device)]),
        vec![("a".into(), U::Disconnected), ("c".into(), U::Connected)]
    );
    assert_eq!(t.get_state("a"), None);
    assert_eq!(t.get_state("b"), Some(S::Offline));
    assert_eq!(
        t.update(&[d("c", S::Offline), d("b", S::Device)]),
        vec![("c".into(), U::Disconnected), ("b".into(), U::Connected)]
    );
}
#[test]
fn serial_strategies_and_ipv6() {
    let _ports = shared_ports();
    let mut props = HashMap::new();
    for (k, v) in [
        ("marathon.serialno", "m"),
        ("ro.boot.serialno", "b"),
        ("net.hostname", "h"),
    ] {
        props.insert(k.to_string(), js(v));
    }
    for (strategy, expected) in [
        (SerialStrategy::Automatic, "m"),
        (SerialStrategy::MarathonProperty, "m"),
        (SerialStrategy::BootProperty, "b"),
        (SerialStrategy::Hostname, "h"),
        (SerialStrategy::Ddms, "adb"),
    ] {
        assert_eq!(detect_serial(strategy, "adb", &props), expected)
    }
    props.clear();
    assert_eq!(detect_serial(SerialStrategy::Automatic, "adb", &props), "adb");
    assert_eq!(detect_serial(SerialStrategy::BootProperty, "adb", &props), "");
    assert_eq!(host_address(checked("::1".parse())), "0:0:0:0:0:0:0:1")
}
async fn event(provider: &AdamDeviceProvider) -> DeviceEvent {
    checked(checked(
        tokio::time::timeout(std::time::Duration::from_secs(60), provider.subscribe().receive()).await,
    ))
    .unwrap()
}
#[test]
fn multi_server_disconnect_keeps_identity_and_disposed_borrow() {
    let _ports = shared_ports();
    run(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut first = server(base_script(27, vec![])).await;
            let mut script = base_script(27, vec![]);
            script["devices"][0]["serial"] = serde_json::json!("emulator-5556");
            let mut second = server(script).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[first.port(), second.port()]))?;
            provider.initialize(context.clone()).await?;
            let a = event(&provider).await;
            let b = event(&provider).await;
            let (DeviceEvent::Connected(a), DeviceEvent::Connected(b)) = (a, b) else {
                panic!("connections")
            };
            assert_ne!(a, b);
            let first_ref = if a.0.info().serial_number.to_string_lossy().ends_with("emulator-5554") {
                a.clone()
            } else {
                b.clone()
            };
            checked(first.disconnect_device("emulator-5554"));
            let DeviceEvent::Disconnected(disconnected) = event(&provider).await else {
                panic!("disconnect")
            };
            assert_eq!(disconnected, first_ref);
            assert!(!disconnected.0.info().healthy);
            assert_eq!(
                disconnected.0.info().network_state,
                marathon_core::model::NetworkState::Connected
            );
            checked(second.disconnect_device("emulator-5556"));
            let _ = event(&provider).await;
            let borrowed = provider.borrow(context).await?;
            assert!(borrowed == a || borrowed == b);
            assert!(!borrowed.0.info().healthy);
            provider.terminate().await?;
            provider.terminate().await?;
            assert_eq!(first.warnings(), 0);
            assert_eq!(second.warnings(), 0);
            checked(first.shutdown().await);
            checked(second.shutdown().await);
            Ok(())
        })
    });
}
#[test]
fn disconnect_before_setup_has_no_connected_event() {
    let _ports = shared_ports();
    run(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(27, vec![]);
            script["devices"][0]["getprop"]
                .as_object_mut()
                .unwrap()
                .remove("sys.boot_completed");
            let mut server = server(script).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            provider.initialize(context).await?;
            for _ in 0..100 {
                if commands(&server).contains(&"getprop".into()) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            checked(server.disconnect_device("emulator-5554"));
            assert!(matches!(event(&provider).await, DeviceEvent::Disconnected(_)));
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
#[test]
fn track_eof_ends_loop_without_reconnect() {
    let _ports = shared_ports();
    run(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(27, vec![]);
            script["devices"][0]["state"] = serde_json::json!("offline");
            let mut server = server(script).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            provider.initialize(context).await?;
            for _ in 0..100 {
                if request_services(&server).contains(&"host:track-devices".into()) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            server.kill(Some(std::time::Duration::from_millis(20)));
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            assert_eq!(
                request_services(&server)
                    .iter()
                    .filter(|s| *s == "host:track-devices")
                    .count(),
                1
            );
            assert!(
                include_str!("../../../fixtures/oracle/android-provider-probes.jsonl")
                    .contains("completed=true|cancelled=true|subscriptions=1")
            );
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
#[test]
fn no_devices_messages() {
    // Exclusive: the second half reuses the stopped server's port, which another test here could
    // otherwise bind in the meantime.
    let _ports = PORTS.write().unwrap_or_else(|e| e.into_inner());
    let mut c = configuration();
    android(&mut c).wait_for_devices_timeout_millis = 25;
    run(c, |runtime, context| {
        Box::pin(async move {
            let mut server = server(serde_json::json!({"server":{"strict":true},"devices":[]})).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            let e = provider.initialize(context.clone()).await.unwrap_err();
            assert!(matches!(e, VendorError::NoDevices(_)));
            assert_eq!(e.failure().message, Some(js("No devices found")));
            provider.terminate().await?;
            let port = server.port();
            checked(server.shutdown().await);
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[port]))?;
            let e = provider.initialize(context).await.unwrap_err();
            assert_eq!(e.failure().message, Some(js("All adb servers are unavailable")));
            provider.terminate().await?;
            Ok(())
        })
    });
}
#[test]
fn test_start_and_kill_adb_server() {
    let _ports = shared_ports();
    use std::os::unix::fs::PermissionsExt;
    let temp = checked(tempfile::tempdir());
    checked(std::fs::create_dir(temp.path().join("platform-tools")));
    let binary = temp.path().join("platform-tools/adb");
    let output = temp.path().join("argv");
    let script = format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", output.display());
    checked(std::fs::write(&binary, script));
    checked(std::fs::set_permissions(
        &binary,
        std::fs::Permissions::from_mode(0o755),
    ));
    let sdk = JavaPath::from_text(&temp.path().to_string_lossy());
    run(configuration(), move |_, context| {
        Box::pin(async move {
            assert!(start_adb(Some(&sdk), 5037, context.clone()).await?);
            assert!(start_adb(Some(&sdk), 1234, context).await?);
            assert_eq!(
                checked(tokio::fs::read_to_string(output).await),
                "-P 5037 start-server\n-P 1234 start-server\n"
            );
            drop(temp);
            Ok(())
        })
    });
}

/// Waits until each fake server has seen at least `count` logcat streams, i.e. the devices it announced have finished
/// the transport handshake that follows `Connected`.
/// Polls rather than spinning on yield_now, which holds a vendor worker and starves the fake servers on small runners;
/// the deadline only catches a hang.
async fn logcat_streams(wanted: &[(&marathon_adb::server::FakeAdbServer, usize)]) {
    checked(
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            for (server, count) in wanted {
                while server
                    .transcript()
                    .iter()
                    .filter(|record| record["type"] == "shell" && record["command"] == "logcat -v long")
                    .count()
                    < *count
                {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            }
        })
        .await,
    );
}

/// D18 (supersedes the collision half of N27): the device map is keyed by (adb server, serial), so a state change on
/// one server touches only that server's device, whichever of the two equal serials was stored last.
#[test]
fn same_serial_on_two_servers_is_keyed_per_server_d18() {
    let _ports = shared_ports();
    run(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut first = server(base_script(27, vec![])).await;
            let mut second = server(base_script(27, vec![])).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[first.port(), second.port()]))?;
            provider.initialize(context.clone()).await?;
            let (DeviceEvent::Connected(a), DeviceEvent::Connected(b)) =
                (event(&provider).await, event(&provider).await)
            else {
                panic!("connections")
            };
            assert_ne!(a, b);
            assert!(provider.android_device(&a).is_some());
            assert!(provider.android_device(&b).is_some());
            // Connected is emitted after spawning logcat, before its transport handshake
            // necessarily finishes. Disconnecting (or terminating) then can reset that handshake and warn.
            logcat_streams(&[(&first, 1), (&second, 1)]).await;
            let first_prefix = format!("127.0.0.1:{}:", first.port());
            let (on_first, on_second) = if a.0.info().serial_number.to_string_lossy().starts_with(&first_prefix) {
                (a, b)
            } else {
                (b, a)
            };
            // Whichever device the old map would have held for emulator-5554, the drop on one server must name
            // that server's device: do it in both directions.
            checked(second.disconnect_device("emulator-5554"));
            let DeviceEvent::Disconnected(disconnected) = event(&provider).await else {
                panic!("disconnect")
            };
            assert_eq!(disconnected, on_second);
            assert!(on_first.0.info().healthy);
            // The dropped device is replaced by a fresh object when the serial returns.
            checked(second.connect_device("emulator-5554"));
            let DeviceEvent::Connected(again) = event(&provider).await else {
                panic!("reconnect")
            };
            assert_ne!(again, on_second);
            assert_ne!(again, on_first);
            assert!(
                again
                    .0
                    .info()
                    .serial_number
                    .to_string_lossy()
                    .starts_with(&format!("127.0.0.1:{}:", second.port()))
            );
            checked(first.disconnect_device("emulator-5554"));
            let DeviceEvent::Disconnected(disconnected) = event(&provider).await else {
                panic!("disconnect")
            };
            assert_eq!(disconnected, on_first);
            assert!(again.0.info().healthy);
            // Same for the replacement device: its logcat is the last part of its start-up.
            logcat_streams(&[(&second, 2)]).await;
            provider.terminate().await?;
            checked(first.shutdown().await);
            checked(second.shutdown().await);
            assert_eq!(first.warnings(), 0, "{:?}", first.transcript());
            assert_eq!(second.warnings(), 0, "{:?}", second.transcript());
            Ok(())
        })
    });
}

#[test]
fn loopback_start_server_retries_version_after_fake_binary() {
    let _ports = PORTS.write().unwrap_or_else(|e| e.into_inner());
    use marathon_adb::server::{FakeAdbServer, Script, ServerOptions};
    use std::os::unix::fs::PermissionsExt;
    let temp = checked(tempfile::tempdir());
    checked(std::fs::create_dir(temp.path().join("platform-tools")));
    let binary = temp.path().join("platform-tools/adb");
    let marker = temp.path().join("started");
    let ready = temp.path().join("ready");
    checked(std::fs::write(
        &binary,
        format!(
            "#!/bin/sh\ntouch '{}'\ni=0\nwhile [ ! -f '{}' ]; do i=$((i+1)); [ $i -lt 6000 ] || exit 1; sleep 0.01; done\n",
            marker.display(),
            ready.display(),
        ),
    ));
    checked(std::fs::set_permissions(
        &binary,
        std::fs::Permissions::from_mode(0o755),
    ));
    let mut config = configuration();
    android(&mut config).android_sdk = Some(JavaPath::from_text(&temp.path().to_string_lossy()));
    run(config, move |runtime, context| {
        Box::pin(async move {
            let listener = checked(tokio::net::TcpListener::bind("127.0.0.1:0").await);
            let port = checked(listener.local_addr()).port();
            drop(listener);
            let startup = tokio::spawn(async move {
                // Real time: the provider spawns the fake binary as a process, which a loaded
                // host can delay by seconds; the deadline only catches a hang.
                checked(
                    tokio::time::timeout(std::time::Duration::from_secs(60), async {
                        while !checked(tokio::fs::try_exists(&marker).await) {
                            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        }
                    })
                    .await,
                );
                let script = checked(Script::from_yaml(&base_script(27, vec![]).to_string()));
                let server = checked(
                    FakeAdbServer::start(
                        checked(script.compile(std::path::Path::new("."))),
                        ServerOptions {
                            port,
                            ..ServerOptions::default()
                        },
                    )
                    .await,
                );
                checked(tokio::fs::write(ready, b"ready").await);
                server
            });
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[port]))?;
            let result = provider.initialize(context).await;
            let mut server = checked(startup.await);
            result?;
            assert!(matches!(event(&provider).await, DeviceEvent::Connected(_)));
            provider.terminate().await?;
            checked(server.shutdown().await);
            assert_eq!(server.warnings(), 0);
            drop(temp);
            Ok(())
        })
    });
}
