//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProviderUpdateTest.kt
mod provider_support;
mod simulator_support;
mod support;
use marathon_config::vendor::DeviceProvider as Source;
use marathon_core::{
    executor::shared::SharedDeferred,
    runtime::Deferred,
    vendor::{Device, DeviceEvent, DeviceProvider},
};
use provider_support::*;
use simulator_support::TestResult;
use std::sync::{Arc, atomic::Ordering};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
};
async fn dynamic() -> TestResult<(ProviderHarness, TcpStream)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let p = ProviderHarness::new(|v| {
        v.device_provider = Source::Dynamic {
            host: "127.0.0.1".into(),
            port: port.into(),
        }
    })?;
    p.provider
        .initialize(p.h.services.context.clone())
        .await
        .map_err(test_error)?;
    let (socket, _) = support::watchdog(listener.accept()).await?;
    Ok((p, socket))
}
fn frame(ids: &[&str]) -> Vec<u8> {
    let mut yaml = String::from("workers:\n- transport:\n    type: local\n  devices:");
    if ids.is_empty() {
        yaml.push_str(" []\n");
    } else {
        yaml.push('\n');
        for id in ids {
            yaml.push_str(&format!("  - type: simulator\n    udid: {id}\n"));
        }
    }
    let mut frame = vec![1, 2, 3, 4];
    frame.extend_from_slice(&(yaml.len() as i32).to_be_bytes());
    frame.extend_from_slice(yaml.as_bytes());
    frame
}
async fn send(socket: &mut TcpStream, ids: &[&str]) -> std::io::Result<()> {
    socket.write_all(&frame(ids)).await
}
#[tokio::test]
async fn an_update_that_adds_a_simulator_sets_up_only_that_one_and_leaves_the_batches_of_the_others_running()
-> TestResult {
    let (p, mut socket) = dynamic().await?;
    p.h.worker.add(["c", "d"]);
    send(&mut socket, &["a", "b"]).await?;
    let devices = p.connected(2).await;
    let (_, a) = batch(&p.h, devices[0].clone(), None).await;
    let (_, b) = batch(&p.h, devices[1].clone(), None).await;
    send(&mut socket, &["a", "b", "c"]).await?;
    send(&mut socket, &["a", "b", "c", "d"]).await?;
    p.connected(2).await;
    for id in ["a", "b", "c", "d"] {
        assert_eq!(home(&p.h.worker, id), 1);
    }
    assert!(!a.is_finished() && !b.is_finished());
    assert!(devices.iter().all(|d| !d.disposed()));
    assert_eq!(p.provider.subscribe().state().queued, 0);
    p.finish().await?;
    let _ = a.await?;
    let _ = b.await?;
    Ok(())
}
#[tokio::test]
async fn an_update_that_removes_a_simulator_disposes_only_that_one_after_its_batch_and_it_is_set_up_again_once_added_back()
-> TestResult {
    let (p, mut socket) = dynamic().await?;
    p.h.worker.add(["c", "d"]);
    send(&mut socket, &["a", "b", "c"]).await?;
    let devices = p.connected(3).await;
    let old = devices.iter().find(|d| d.serial().starts_with("b@")).unwrap().clone();
    let gate = SharedDeferred::new();
    let mut jobs = Vec::new();
    let mut script = None;
    for d in &devices {
        let (s, j) = batch(&p.h, d.clone(), d.serial().starts_with("b@").then(|| gate.clone())).await;
        if d.serial().starts_with("b@") {
            script = Some(s);
        }
        jobs.push((d.clone(), j));
    }
    send(&mut socket, &["a", "c", "d"]).await?;
    let mut disconnected = false;
    let mut added = false;
    for _ in 0..2 {
        match support::watchdog(p.provider.subscribe().receive())
            .await
            .map_err(test_error)?
            .unwrap()
        {
            DeviceEvent::Disconnected(d) => {
                assert!(Arc::ptr_eq(&d.0, &(old.clone() as Arc<dyn Device>)));
                disconnected = true;
            }
            DeviceEvent::Connected(d) => {
                assert!(d.0.info().serial_number.to_string_lossy().starts_with("d@"));
                added = true;
            }
        }
    }
    assert!(disconnected && added);
    script.unwrap().closing.wait().await.map_err(test_error)?;
    assert!(!old.disposed());
    eventually(|| p.connections.released.load(Ordering::SeqCst) == 2).await;
    for (d, j) in &jobs {
        if !d.serial().starts_with("b@") {
            assert!(!j.is_finished());
            assert!(!d.disposed());
        }
    }
    gate.complete(Ok(()));
    eventually(|| old.disposed()).await;
    send(&mut socket, &["a", "b", "c", "d"]).await?;
    let replacement = p.connected(1).await.remove(0);
    assert!(!Arc::ptr_eq(&old, &replacement));
    assert_eq!(home(&p.h.worker, "b"), 2);
    for id in ["a", "c", "d"] {
        assert_eq!(home(&p.h.worker, id), 1);
    }
    p.finish().await?;
    for (_, j) in jobs {
        let _ = j.await?;
    }
    Ok(())
}
