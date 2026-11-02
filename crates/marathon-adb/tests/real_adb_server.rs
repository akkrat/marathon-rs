//! The adb client against a real adb server (WP 6.1). Opt-in: set `MARATHON_REAL_ADB_SERVER` to the
//! server's `host:port`, else the tests skip with a message. A second adb server started here would scan
//! for emulators and USB devices and could take them from the one already running, so the tests use a
//! server that is already there and only read from it. The `#[ignore]`d smoke test also needs
//! `MARATHON_REAL_ADB_SERIAL`, a booted device, and runs two harmless shell commands on it.

use std::net::SocketAddr;
use std::sync::Arc;

use marathon_adb::client::device::{AsyncDeviceMonitorRequest, FetchDeviceFeaturesRequest, ListDevicesRequest};
use marathon_adb::client::misc::GetAdbServerVersionRequest;
use marathon_adb::client::shell::v1::ShellCommandRequest;
use marathon_adb::client::shell::v2::ChanneledShellCommandRequest;
use marathon_adb::client::{AdbError, AndroidDebugBridgeClient, SocketFactory};

fn client() -> Option<AndroidDebugBridgeClient> {
    let Some(addr) = std::env::var("MARATHON_REAL_ADB_SERVER").ok() else {
        eprintln!("skipping: set MARATHON_REAL_ADB_SERVER=host:port to run against a real adb server");
        return None;
    };
    let addr: SocketAddr = addr.parse().expect("MARATHON_REAL_ADB_SERVER is host:port");
    let factory = Arc::new(SocketFactory::new(10_000, 30_000));
    Some(AndroidDebugBridgeClient::new(addr.ip(), addr.port(), factory, None))
}

#[tokio::test]
async fn host_services() {
    let Some(client) = client() else { return };
    let version = client.execute(GetAdbServerVersionRequest, None).await.unwrap();
    assert_eq!(version, 41, "adb 1.0.41 reports protocol 41");
    let devices = client.execute(ListDevicesRequest, None).await.unwrap();
    let mut track = client.execute_channel(AsyncDeviceMonitorRequest, None).unwrap();
    let first = track.next().await.unwrap().unwrap();
    let serials = |d: &[marathon_adb::client::device::Device]| {
        let mut s: Vec<_> = d.iter().map(|d| d.serial.clone()).collect();
        s.sort();
        s
    };
    assert_eq!(serials(&first), serials(&devices));
    let error = client
        .execute(FetchDeviceFeaturesRequest::new("no-such-serial-wp61"), None)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        AdbError::RequestRejected("device 'no-such-serial-wp61' not found".into())
    );
    eprintln!("adb {version}; devices {devices:?}");
}

#[tokio::test]
#[ignore = "needs MARATHON_REAL_ADB_SERVER and a booted device in MARATHON_REAL_ADB_SERIAL"]
async fn device_shells() {
    let Some(client) = client() else { return };
    let serial = std::env::var("MARATHON_REAL_ADB_SERIAL").expect("MARATHON_REAL_ADB_SERIAL");
    let r = client
        .execute(ShellCommandRequest::new("echo xbox"), Some(&serial))
        .await
        .unwrap();
    assert_eq!((r.output(), r.exit_code), ("xbox\n".to_string(), 0));
    let mut stream = client
        .execute_channel(ChanneledShellCommandRequest::new("exit 255", None), Some(&serial))
        .unwrap();
    let mut exit = None;
    while let Some(chunk) = stream.next().await {
        exit = exit.or(chunk.unwrap().exit_code);
    }
    assert_eq!(exit, Some(-1));
}
