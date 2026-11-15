//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AdamDeviceProvider.kt
//! (U170): a device that comes back, from a grace period (D41) or with its adb server (D22), is probed and restored outside
//! the tracking loop, so that it holds no other device.
//!
//! The vendor clock moves only when a test moves it, so a grace period ends, or a probe times out, exactly when the test
//! says. Slow work is a fake-adb gate that the test opens.
mod support;
use marathon_android::provider::AdamDeviceProvider;
use marathon_core::vendor::{DeviceEvent, DeviceProvider, DeviceRef};
use serde_json::{Value, json};
use std::time::Duration;
use support::*;

const PROBE: &str = "probe";
const REVERSE: &str = "reverse";

async fn until(mut condition: impl FnMut() -> bool) {
    checked(
        tokio::time::timeout(Duration::from_secs(60), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await,
    );
}
async fn event(provider: &AdamDeviceProvider) -> DeviceEvent {
    let channel = provider.subscribe();
    checked(checked(
        tokio::time::timeout(Duration::from_secs(60), channel.receive()).await,
    ))
    .unwrap()
}
async fn pending_event(provider: &AdamDeviceProvider) -> Option<DeviceEvent> {
    let channel = provider.subscribe();
    tokio::time::timeout(Duration::ZERO, channel.receive())
        .await
        .ok()
        .and_then(checked)
}
async fn disconnected(provider: &AdamDeviceProvider) -> DeviceRef {
    match event(provider).await {
        DeviceEvent::Disconnected(device) => device,
        DeviceEvent::Connected(_) => panic!("a connection where a disconnect was due"),
    }
}
fn in_grace(device: &DeviceRef) -> bool {
    !device.0.info().healthy
}
/// The devices connected, by adb serial.
async fn connected_all(provider: &AdamDeviceProvider, count: usize) -> Vec<(String, DeviceRef)> {
    let mut devices = vec![];
    for _ in 0..count {
        let DeviceEvent::Connected(device) = event(provider).await else {
            panic!("connection")
        };
        let serial = provider.android_device(&device).unwrap().adb_serial().to_owned();
        devices.push((serial, device));
    }
    devices
}
fn find(devices: &[(String, DeviceRef)], serial: &str) -> DeviceRef {
    devices.iter().find(|(s, _)| s == serial).unwrap().1.clone()
}
/// Return probes waiting at the gate or past it: the `getprop`s of each gated device after its setup's two.
fn probes(server: &marathon_adb::server::FakeAdbServer) -> usize {
    server
        .transcript()
        .iter()
        .filter(|r| r["type"] == "shell" && r["command"] == "getprop")
        .filter(|r| r["rule"].as_str().is_some_and(|rule| rule.ends_with(".shell[1]")))
        .count()
}
fn getprops_on(server: &marathon_adb::server::FakeAdbServer, serial: &str) -> usize {
    server
        .transcript()
        .iter()
        .filter(|r| r["type"] == "shell" && r["command"] == "getprop" && r["serial"] == serial)
        .count()
}
fn reverse_forwards_requested(server: &marathon_adb::server::FakeAdbServer) -> usize {
    request_services(server)
        .iter()
        .filter(|s| s.starts_with("reverse:forward:"))
        .count()
}
/// The base script's device under each serial. Each one in `gated` answers its setup's two `getprop`s, then holds its
/// next one, the return probe, until the test opens the gate `probe`; later ones answer at once.
fn script(serials: &[&str], gated: &[&str]) -> Value {
    let mut script = base_script(27, vec![]);
    let template = script["devices"][0].clone();
    let all: String = template["getprop"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| format!("[{k}]: [{}]\n", v.as_str().map_or_else(|| v.to_string(), str::to_owned)))
        .collect();
    let devices: Vec<Value> = serials
        .iter()
        .map(|serial| {
            let mut device = template.clone();
            device["serial"] = json!(serial);
            if gated.contains(serial) {
                device["shell"] = json!([
                    {"exact": "getprop", "times": 2, "stdout": all},
                    {"exact": "getprop", "times": 1, "output": [{"gate": PROBE}, {"stdout": all}]},
                ]);
            }
            device
        })
        .collect();
    script["devices"] = json!(devices);
    script
}
/// A device goes through `offline` and comes back, as in a blip.
async fn blip(server: &marathon_adb::server::FakeAdbServer, device: &DeviceRef, serial: &str) {
    checked(server.set_device_state(serial, "offline"));
    until(|| in_grace(device)).await;
    checked(server.set_device_state(serial, "device"));
}
/// A killed fake server listens on its port again, so nothing else may bind it meanwhile (`docs/testing.md`, rule 3).
static PORTS: std::sync::RwLock<()> = std::sync::RwLock::new(());
fn shared_ports() -> std::sync::RwLockReadGuard<'static, ()> {
    PORTS.read().unwrap_or_else(|e| e.into_inner())
}
fn support_ports() -> std::sync::RwLockWriteGuard<'static, ()> {
    PORTS.write().unwrap_or_else(|e| e.into_inner())
}

/// The review's witness: one device's return probe hangs, and another device's grace still ends at its 10 s, not when
/// the probe gives up at 20 s. The device whose probe hangs stays held until it answers, and keeps its actor.
#[test]
fn a_slow_return_probe_does_not_hold_another_devices_grace_u170() {
    let _ports = shared_ports();
    run_with_driven_clock(configuration(), |runtime, context, clock| {
        Box::pin(async move {
            let (slow, other) = ("emulator-5554", "emulator-5556");
            let mut server = server(script(&[slow, other], &[slow])).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            let start = context.clock.now_millis();
            provider.initialize(context).await?;
            let devices = connected_all(&provider, 2).await;
            let (a, b) = (find(&devices, slow), find(&devices, other));
            logcat_streams(&[(&server, 2)]).await;

            checked(server.set_device_state(other, "offline"));
            until(|| in_grace(&b)).await;
            blip(&server, &a, slow).await;
            until(|| probes(&server) == 1).await;
            clock.advance_to(start + 10_500).await;
            assert_eq!(disconnected(&provider).await, b);
            assert!(in_grace(&a));

            server.open_gate(PROBE);
            until(|| !in_grace(&a)).await;
            assert!(pending_event(&provider).await.is_none());
            assert_eq!(provider.android_device(&a).unwrap().grace_periods(), 1);
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

/// The review's second witness: with `shell` at 30 s, two returned devices whose probes hang are probed at once and both
/// give up at 30 s, where they used to take 30 s each; a third device's grace ends at its 10 s meanwhile.
#[test]
fn two_hanging_return_probes_do_not_add_up_u170() {
    let _ports = shared_ports();
    let mut c = configuration();
    android(&mut c).timeout_configuration.shell = checked(jcompat::jtime::duration_of(30, 0).ok_or("duration"));
    run_with_driven_clock(c, |runtime, context, clock| {
        Box::pin(async move {
            let serials = ["emulator-5554", "emulator-5556", "emulator-5558"];
            let mut server = server(script(&serials, &serials[..2])).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            let start = context.clock.now_millis();
            provider.initialize(context).await?;
            let devices = connected_all(&provider, 3).await;
            let [a, b, c] = serials.map(|serial| find(&devices, serial));
            logcat_streams(&[(&server, 3)]).await;

            checked(server.set_device_state(serials[2], "offline"));
            until(|| in_grace(&c)).await;
            blip(&server, &a, serials[0]).await;
            blip(&server, &b, serials[1]).await;
            until(|| probes(&server) == 2).await;
            clock.advance_to(start + 10_500).await;
            assert_eq!(disconnected(&provider).await, c);
            assert!(in_grace(&a) && in_grace(&b));

            // A device that does not answer is taken for one that restarted, and set up again.
            clock.advance_to(start + 30_500).await;
            let mut gone = vec![];
            let mut back = 0;
            while gone.len() < 2 || back < 2 {
                match event(&provider).await {
                    DeviceEvent::Disconnected(device) => gone.push(device),
                    DeviceEvent::Connected(_) => back += 1,
                }
            }
            assert!(gone.contains(&a) && gone.contains(&b), "{gone:?}");
            server.open_gate(PROBE);
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

/// A device whose recovery is still queued leaves and comes back as a new device under the same serial. The old recovery
/// is dropped with the device: once the queue moves it does not probe the new device, nor take it for one that restarted.
/// The eight slots are taken by devices whose probes hang, so the queue is the only place it could be. The fake may send
/// a return and the next change as one list, so another device's grace shows when the return has been read.
#[test]
fn a_recovery_made_stale_by_a_replacement_releases_nothing_u170() {
    let _ports = shared_ports();
    run_with_driven_clock(configuration(), |runtime, context, _clock| {
        Box::pin(async move {
            let serials: Vec<String> = (0..10).map(|i| format!("emulator-{}", 5554 + 2 * i)).collect();
            let serials: Vec<&str> = serials.iter().map(String::as_str).collect();
            let (busy, replaced, marker) = (&serials[..8], serials[8], serials[9]);
            let mut server = server(script(&serials, busy)).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            provider.initialize(context).await?;
            let devices = connected_all(&provider, 10).await;
            logcat_streams(&[(&server, 10)]).await;
            for serial in busy {
                blip(&server, &find(&devices, serial), serial).await;
            }
            until(|| probes(&server) == 8).await;

            let old = find(&devices, replaced);
            blip(&server, &old, replaced).await;
            let read = find(&devices, marker);
            checked(server.set_device_state(marker, "offline"));
            until(|| in_grace(&read)).await;
            assert_eq!(probes(&server), 8);
            checked(server.disconnect_device(replaced));
            assert_eq!(disconnected(&provider).await, old);
            checked(server.connect_device(replaced));
            let DeviceEvent::Connected(new) = event(&provider).await else {
                panic!("connection")
            };
            logcat_streams(&[(&server, 11)]).await;
            let getprops = getprops_on(&server, replaced);

            server.open_gate(PROBE);
            for serial in busy {
                let device = find(&devices, serial);
                until(|| !in_grace(&device)).await;
            }
            // A stale recovery would start as the first slot freed up, and its probe needs no gate.
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(getprops_on(&server, replaced), getprops);
            assert!(!in_grace(&new));
            assert!(pending_event(&provider).await.is_none());
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

/// Twenty devices kept through an adb server restart are restored eight at a time, and the restoration runs outside the
/// tracking loop: a grace period still ends on time meanwhile. Each reverse port forward waits at the fake until the test
/// lets it through.
#[test]
fn restorations_after_a_server_restart_are_bounded_and_leave_the_loop_free_u170() {
    let _ports = support_ports();
    let mut c = configuration();
    android(&mut c).wait_for_devices_timeout_millis = 600_000;
    android(&mut c).test_access_configuration.adb = true;
    run_with_driven_clock(c, |runtime, context, clock| {
        Box::pin(async move {
            let serials: Vec<String> = (1..=20).map(|i| format!("device-{i:02}")).collect();
            let serials: Vec<&str> = serials.iter().map(String::as_str).collect();
            let mut script = script(&serials, &[]);
            script["server"]["reverse_gate"] = json!(REVERSE);
            let mut server = server(script).await;
            server.open_gate(REVERSE);
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            let start = context.clock.now_millis();
            provider.initialize(context).await?;
            let devices = connected_all(&provider, 20).await;
            logcat_streams(&[(&server, 20)]).await;
            until(|| reverse_forwards_requested(&server) == 20).await;
            server.close_gate(REVERSE);

            server.kill(Some(Duration::from_millis(50)));
            // The provider tries again on the vendor clock: 1 s, then 2 s, 4 s and 8 s apart.
            let mut now = start;
            for _ in 0..100 {
                if reverse_forwards_requested(&server) > 20 {
                    break;
                }
                now += 1000;
                clock.advance_to(now).await;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            until(|| reverse_forwards_requested(&server) == 20 + 8).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(reverse_forwards_requested(&server), 20 + 8);

            // The loop is free meanwhile: a device going offline has its grace, which ends at its limit.
            let (serial, device) = &devices[19];
            checked(server.set_device_state(serial, "offline"));
            until(|| in_grace(device)).await;
            clock.advance_to(now + 10_500).await;
            assert_eq!(disconnected(&provider).await, *device);
            // The others are all restored. The one that left may have sent its request before it did.
            server.open_gate(REVERSE);
            until(|| reverse_forwards_requested(&server) >= 20 + 19).await;
            assert!(pending_event(&provider).await.is_none());
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

/// D52: a cancel while recoveries hang ends them and waits only for that, and a recovery cut short releases nothing.
#[test]
fn termination_during_recoveries_is_bounded_and_releases_nothing_u170() {
    let _ports = shared_ports();
    run_with_driven_clock(configuration(), |runtime, context, _clock| {
        Box::pin(async move {
            let serials = ["emulator-5554", "emulator-5556", "emulator-5558"];
            let mut server = server(script(&serials, &serials)).await;
            let provider = AdamDeviceProvider::new(change_runtime(&runtime, &[server.port()]))?;
            provider.initialize(context).await?;
            let devices = connected_all(&provider, 3).await;
            logcat_streams(&[(&server, 3)]).await;
            for serial in serials {
                blip(&server, &find(&devices, serial), serial).await;
            }
            until(|| probes(&server) == 3).await;
            let logcats = logcat_count(&server);
            // The probes would time out only when the clock moves, which it does not.
            checked(tokio::time::timeout(Duration::from_secs(60), provider.terminate()).await)?;
            server.open_gate(PROBE);
            tokio::time::sleep(Duration::from_millis(200)).await;
            for (_, device) in &devices {
                assert!(in_grace(device));
            }
            assert_eq!(logcat_count(&server), logcats);
            assert!(pending_event(&provider).await.is_none());
            checked(server.shutdown().await);
            Ok(())
        })
    });
}
fn logcat_count(server: &marathon_adb::server::FakeAdbServer) -> usize {
    server
        .transcript()
        .iter()
        .filter(|r| r["type"] == "shell" && r["command"] == "logcat -v long")
        .count()
}
