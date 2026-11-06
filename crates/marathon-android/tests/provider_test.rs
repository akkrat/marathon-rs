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
/// A test that kills a fake server has the same need: the server listens on that port again, and until then the
/// provider retries it. They hold this lock exclusively; the others share it.
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
fn multi_server_disconnect_keeps_identity_and_borrow_skips_the_lost_device_d24() {
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
            // Connected comes before the device's logcat handshake; see logcat_streams.
            logcat_streams(&[(&first, 1), (&second, 1)]).await;
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
                marathon_core::model::NetworkState::Disconnected
            );
            let on_second = if first_ref == a { &b } else { &a };
            assert_eq!(
                on_second.0.info().network_state,
                marathon_core::model::NetworkState::Connected
            );
            // Only the healthy device is left to borrow, however often the random pick runs.
            for _ in 0..20 {
                assert_eq!(provider.borrow(context.clone()).await?, *on_second);
            }
            let on_second = on_second.clone();
            checked(second.disconnect_device("emulator-5556"));
            let _ = event(&provider).await;
            // With nothing left, borrow waits; the reconnect below is what ends the wait.
            let waiting = {
                let provider = provider.clone();
                let context = context.clone();
                tokio::spawn(async move { provider.borrow(context).await })
            };
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            assert!(!waiting.is_finished(), "borrow returned a disposed device");
            checked(second.connect_device("emulator-5556"));
            let DeviceEvent::Connected(again) = event(&provider).await else {
                panic!("reconnect")
            };
            let borrowed = checked(checked(
                tokio::time::timeout(std::time::Duration::from_secs(60), waiting).await,
            ))?;
            assert_eq!(borrowed, again);
            assert_ne!(borrowed, on_second);
            assert!(borrowed.0.info().healthy);
            // The old object stays lost although its serial is back on the same tracker.
            assert!(!on_second.0.info().healthy);
            assert_eq!(
                on_second.0.info().network_state,
                marathon_core::model::NetworkState::Disconnected
            );
            assert_eq!(
                borrowed.0.info().network_state,
                marathon_core::model::NetworkState::Connected
            );
            logcat_streams(&[(&second, 2)]).await;
            provider.terminate().await?;
            provider.terminate().await?;
            assert_eq!(first.warnings(), 0, "{:?}", first.transcript());
            assert_eq!(second.warnings(), 0, "{:?}", second.transcript());
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
/// Waits (the deadline only catches a hang) until the server has answered `count` track-devices requests.
async fn track_requests(server: &marathon_adb::server::FakeAdbServer, count: usize) {
    checked(
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            while request_services(server)
                .iter()
                .filter(|s| *s == "host:track-devices")
                .count()
                < count
            {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await,
    );
}
/// An event already waiting, without waiting for one.
async fn pending_event(provider: &AdamDeviceProvider) -> Option<DeviceEvent> {
    let channel = provider.subscribe();
    tokio::time::timeout(std::time::Duration::ZERO, channel.receive())
        .await
        .ok()
        .and_then(checked)
}
/// D22 (N16). The Kotlin provider's job ends with the failed stream (the JVM probe's `subscriptions=1`). Here tracking
/// resumes; a device still listed keeps its object with no event at all, so it can have no second actor, and what lived
/// on the lost connections comes back: its logcat stream.
#[test]
fn track_eof_resumes_tracking_and_keeps_a_listed_device_d22() {
    let _ports = PORTS.write().unwrap_or_else(|e| e.into_inner());
    run(configuration(), |runtime, context| {
        Box::pin(async move {
            assert!(
                include_str!("../../../fixtures/oracle/android-provider-probes.jsonl")
                    .contains("completed=true|cancelled=true|subscriptions=1")
            );
            let mut server = server(base_script(27, vec![])).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            provider.initialize(context).await?;
            let DeviceEvent::Connected(first) = event(&provider).await else {
                panic!("connection")
            };
            logcat_streams(&[(&server, 1)]).await;
            server.kill(Some(std::time::Duration::from_millis(20)));
            track_requests(&server, 2).await;
            // The device's logcat comes back after the reconcile, so no event can still be on its way by then.
            logcat_streams(&[(&server, 2)]).await;
            assert!(pending_event(&provider).await.is_none());
            assert!(first.0.info().healthy);
            // The new stream really drives events, with the same device object.
            checked(server.disconnect_device("emulator-5554"));
            let DeviceEvent::Disconnected(gone) = event(&provider).await else {
                panic!("disconnect")
            };
            assert_eq!(gone, first);
            checked(server.connect_device("emulator-5554"));
            let DeviceEvent::Connected(again) = event(&provider).await else {
                panic!("reconnect")
            };
            assert_ne!(again, first);
            logcat_streams(&[(&server, 3)]).await;
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
/// D22: after the window, a device the returned server does not list is disconnected the normal way, and one that
/// joined meanwhile connects, once.
#[test]
fn returned_server_reconciles_its_device_list_d22() {
    let _ports = PORTS.write().unwrap_or_else(|e| e.into_inner());
    let mut c = configuration();
    android(&mut c).wait_for_devices_timeout_millis = 1500;
    run(c, |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(27, vec![]);
            let mut other = script["devices"][0].clone();
            other["serial"] = serde_json::json!("emulator-5556");
            other["connected"] = serde_json::json!(false);
            script["devices"].as_array_mut().unwrap().push(other);
            let mut server = server(script).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            provider.initialize(context).await?;
            let DeviceEvent::Connected(first) = event(&provider).await else {
                panic!("connection")
            };
            logcat_streams(&[(&server, 1)]).await;
            server.kill(Some(std::time::Duration::from_millis(500)));
            // The list must change while nothing is listening, or the live stream would report it.
            checked(
                tokio::time::timeout(std::time::Duration::from_secs(60), async {
                    while !server.transcript().iter().any(|r| r["event"] == "server_killed") {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                })
                .await,
            );
            checked(server.disconnect_device("emulator-5554"));
            checked(server.connect_device("emulator-5556"));
            track_requests(&server, 2).await;
            let mut connected = 0;
            let mut disconnected = 0;
            for _ in 0..2 {
                match event(&provider).await {
                    DeviceEvent::Connected(d) => {
                        assert!(d.0.info().serial_number.to_string_lossy().ends_with("emulator-5556"));
                        connected += 1;
                    }
                    DeviceEvent::Disconnected(d) => {
                        assert_eq!(d, first);
                        assert_eq!(
                            d.0.info().network_state,
                            marathon_core::model::NetworkState::Disconnected
                        );
                        disconnected += 1;
                    }
                }
            }
            assert_eq!((connected, disconnected), (1, 1));
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
/// D22 with D18: an outage on one server leaves the other alone, and a server that never returns loses its own devices
/// only, by the bound that also lets the run end. Terminating the provider then ends its retry loop.
#[test]
fn a_server_that_never_returns_loses_only_its_own_devices_d22() {
    let _ports = PORTS.write().unwrap_or_else(|e| e.into_inner());
    let mut c = configuration();
    android(&mut c).wait_for_devices_timeout_millis = 1500;
    run(c, |runtime, context| {
        Box::pin(async move {
            let mut dying = server(base_script(27, vec![])).await;
            let mut healthy = server(base_script(27, vec![])).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[dying.port(), healthy.port()]))?;
            provider.initialize(context).await?;
            let (DeviceEvent::Connected(a), DeviceEvent::Connected(b)) =
                (event(&provider).await, event(&provider).await)
            else {
                panic!("connections")
            };
            logcat_streams(&[(&dying, 1), (&healthy, 1)]).await;
            let dying_prefix = format!("127.0.0.1:{}:", dying.port());
            let (on_dying, on_healthy) = if a.0.info().serial_number.to_string_lossy().starts_with(&dying_prefix) {
                (a, b)
            } else {
                (b, a)
            };
            dying.kill(None);
            let DeviceEvent::Disconnected(gone) = event(&provider).await else {
                panic!("the dead server's device")
            };
            assert_eq!(gone, on_dying);
            assert!(on_healthy.0.info().healthy);
            // The healthy server's stream was never interrupted and still drives events.
            assert_eq!(
                request_services(&healthy)
                    .iter()
                    .filter(|s| *s == "host:track-devices")
                    .count(),
                1
            );
            checked(healthy.disconnect_device("emulator-5554"));
            let DeviceEvent::Disconnected(gone) = event(&provider).await else {
                panic!("the healthy server's device")
            };
            assert_eq!(gone, on_healthy);
            checked(tokio::time::timeout(std::time::Duration::from_secs(60), provider.terminate()).await)?;
            checked(dying.shutdown().await);
            checked(healthy.shutdown().await);
            Ok(())
        })
    });
}
/// D22: during an outage a batch waits for the server rather than failing at once, which would burn its retries
/// (`uncompletedTestRetryQuota`) until the server is back. Once the server is back, the batch runs.
#[test]
fn a_batch_waits_for_a_lost_server_d22() {
    let _ports = PORTS.write().unwrap_or_else(|e| e.into_inner());
    use marathon_core::{
        executor::shared::SharedDeferred,
        model::{DevicePoolId, TestBatch},
    };
    run(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(27, vec![])).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            provider.initialize(context.clone()).await?;
            let DeviceEvent::Connected(device) = event(&provider).await else {
                panic!("connection")
            };
            logcat_streams(&[(&server, 1)]).await;
            let android = provider.android_device(&device).unwrap();
            assert!(android.adb_server_available());
            server.kill(Some(std::time::Duration::from_secs(2)));
            checked(
                tokio::time::timeout(std::time::Duration::from_secs(60), async {
                    while android.adb_server_available() {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                })
                .await,
            );
            let batch = {
                let device = device.clone();
                let configuration = runtime.dependencies.configuration.clone();
                tokio::spawn(async move {
                    device
                        .0
                        .execute(
                            configuration,
                            DevicePoolId { name: "pool".into() },
                            std::sync::Arc::new(TestBatch {
                                tests: vec![],
                                id: "batch".into(),
                            }),
                            std::sync::Arc::new(SharedDeferred::default()),
                            context,
                        )
                        .await
                })
            };
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            if !android.adb_server_available() {
                assert!(!batch.is_finished(), "the batch did not wait for the server");
            }
            // The test's executor refuses every batch, which shows that this one got that far.
            let outcome = checked(checked(
                tokio::time::timeout(std::time::Duration::from_secs(60), batch).await,
            ));
            assert!(android.adb_server_available());
            assert_eq!(
                outcome.unwrap_err().failure().message,
                Some(js("Android batch execution is supplied by WP 6.6a"))
            );
            logcat_streams(&[(&server, 2)]).await;
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
/// D22: a batch's device loss while the adb server is away fails only the batch, because a pool ends with its last
/// device; with the server there, it stays a device loss.
#[test]
fn device_loss_counts_only_while_the_server_is_there_d22() {
    let _ports = PORTS.write().unwrap_or_else(|e| e.into_inner());
    use marathon_android::{
        device::{AndroidBatchExecutor, AndroidDevice},
        services::AndroidRuntime,
    };
    use marathon_core::{
        executor::shared::SharedDeferred,
        model::{BatchRef, DevicePoolId, TestBatch, TestBatchResults},
        runtime::{Deferred, VendorFuture},
        vendor::VendorContext,
    };
    use std::sync::Arc;
    #[derive(Default)]
    struct Lost(tokio::sync::Notify);
    impl AndroidBatchExecutor for Lost {
        fn execute<'a>(
            &'a self,
            _: &'a AndroidDevice,
            _: Arc<marathon_config::Configuration>,
            _: DevicePoolId,
            _: BatchRef,
            _: Arc<dyn Deferred<TestBatchResults>>,
            _: VendorContext,
        ) -> VendorFuture<'a, ()> {
            Box::pin(async move {
                self.0.notified().await;
                Err(VendorError::DeviceLost(Failure {
                    class: "com.malinskiy.marathon.exceptions.DeviceLostException".into(),
                    message: Some(js("lost")),
                    cause: None,
                    stacktrace: None,
                }))
            })
        }
    }
    run(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(27, vec![])).await;
            let lost = Arc::new(Lost::default());
            let changed = change_runtime(&runtime, &[server.port()]);
            let changed = Arc::new(AndroidRuntime {
                dependencies: changed.dependencies.clone(),
                configuration: changed.configuration.clone(),
                file_sync: changed.file_sync.clone(),
                bundles: changed.bundles.clone(),
                identifier: changed.identifier.clone(),
                executor: lost.clone(),
                log: changed.log.clone(),
            });
            let provider = AdamDeviceProvider::new(changed)?;
            provider.initialize(context.clone()).await?;
            let DeviceEvent::Connected(device) = event(&provider).await else {
                panic!("connection")
            };
            logcat_streams(&[(&server, 1)]).await;
            let batch = || {
                let device = device.clone();
                let configuration = runtime.dependencies.configuration.clone();
                let context = context.clone();
                tokio::spawn(async move {
                    device
                        .0
                        .execute(
                            configuration,
                            DevicePoolId { name: "pool".into() },
                            Arc::new(TestBatch {
                                tests: vec![],
                                id: "batch".into(),
                            }),
                            Arc::new(SharedDeferred::default()),
                            context,
                        )
                        .await
                })
            };
            let running = batch();
            lost.0.notify_one();
            let outcome = checked(checked(
                tokio::time::timeout(std::time::Duration::from_secs(60), running).await,
            ));
            assert!(matches!(outcome, Err(VendorError::DeviceLost(_))), "{outcome:?}");
            let running = batch();
            server.kill(Some(std::time::Duration::from_secs(2)));
            checked(
                tokio::time::timeout(std::time::Duration::from_secs(60), async {
                    while !server.transcript().iter().any(|r| r["event"] == "server_killed") {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                })
                .await,
            );
            lost.0.notify_one();
            let outcome = checked(checked(
                tokio::time::timeout(std::time::Duration::from_secs(60), running).await,
            ));
            assert!(matches!(outcome, Err(VendorError::BatchExecution(_))), "{outcome:?}");
            logcat_streams(&[(&server, 2)]).await;
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
/// D22: a loopback server that refuses the connection is restarted through the path initial setup uses.
#[test]
fn a_lost_loopback_server_is_started_again_through_the_setup_path_d22() {
    let _ports = PORTS.write().unwrap_or_else(|e| e.into_inner());
    use std::os::unix::fs::PermissionsExt;
    let temp = checked(tempfile::tempdir());
    checked(std::fs::create_dir(temp.path().join("platform-tools")));
    let binary = temp.path().join("platform-tools/adb");
    let argv = temp.path().join("argv");
    checked(std::fs::write(
        &binary,
        format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", argv.display()),
    ));
    checked(std::fs::set_permissions(
        &binary,
        std::fs::Permissions::from_mode(0o755),
    ));
    let mut c = configuration();
    android(&mut c).android_sdk = Some(JavaPath::from_text(&temp.path().to_string_lossy()));
    run(c, move |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(27, vec![])).await;
            let port = server.port();
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[port]))?;
            provider.initialize(context).await?;
            assert!(matches!(event(&provider).await, DeviceEvent::Connected(_)));
            logcat_streams(&[(&server, 1)]).await;
            // Long enough that the first reconnect attempt, one second in, finds the port closed.
            server.kill(Some(std::time::Duration::from_millis(1500)));
            track_requests(&server, 2).await;
            assert!(pending_event(&provider).await.is_none());
            let started = checked(tokio::fs::read_to_string(&argv).await);
            assert!(
                started.lines().all(|l| l == format!("-P {port} start-server")) && !started.is_empty(),
                "{started}"
            );
            logcat_streams(&[(&server, 2)]).await;
            provider.terminate().await?;
            checked(server.shutdown().await);
            drop(temp);
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
            // Connected comes before the device's logcat handshake; see logcat_streams.
            logcat_streams(&[(&server, 1)]).await;
            provider.terminate().await?;
            checked(server.shutdown().await);
            assert_eq!(server.warnings(), 0, "{:?}", server.transcript());
            drop(temp);
            Ok(())
        })
    });
}
