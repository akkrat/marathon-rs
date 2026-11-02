//! The adb client against the scripted server (WP 6.1): the request strings Kotlin marathon sent, and the
//! client's behaviour on shells, streams, timeouts, cancellation and close.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use marathon_adb::client::abb::{AbbExecRequest, serialize_abb_exec};
use marathon_adb::client::device::{
    AsyncDeviceMonitorRequest, Device, DeviceState, FetchDeviceFeaturesRequest, ListDevicesRequest,
};
use marathon_adb::client::exec::ExecRequest;
use marathon_adb::client::misc::GetAdbServerVersionRequest;
use marathon_adb::client::shell::compat::CompatShellCommandRequest;
use marathon_adb::client::shell::v1::{ChanneledShellCommandRequest, ShellCommandRequest};
use marathon_adb::client::shell::v2::{self, ShellCommandInputChunk, ShellCommandResultChunk};
use marathon_adb::client::transform::{ResponseTransformer, StringResponseTransformer};
use marathon_adb::client::{
    AdbError, AndroidDebugBridgeClient, ComplexRequest, Feature, Request, Socket, SocketFactory,
};
use marathon_adb::server::{FakeAdbServer, Script, ServerOptions};
use serde_json::Value;

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(rel)
}

async fn start(yaml: &str) -> FakeAdbServer {
    let script = Script::from_yaml(yaml).unwrap().compile(Path::new(".")).unwrap();
    FakeAdbServer::start(script, ServerOptions::default()).await.unwrap()
}

fn client(server: &FakeAdbServer, idle_timeout_ms: i64) -> AndroidDebugBridgeClient {
    let factory = SocketFactory::new(10_000, idle_timeout_ms);
    AndroidDebugBridgeClient::new(IpAddr::V4(Ipv4Addr::LOCALHOST), server.port(), Arc::new(factory), None)
}

fn records(server: &FakeAdbServer, kind: &str) -> Vec<Value> {
    server.transcript().into_iter().filter(|r| r["type"] == kind).collect()
}

/// Waits (bounded) until the server has recorded `event`.
async fn wait_for_event(server: &FakeAdbServer, event: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !records(server, "event").iter().any(|r| r["event"] == event) {
        assert!(
            Instant::now() < deadline,
            "no {event} event in {:?}",
            server.transcript()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// `abb_exec:package\0install…` with the APK streamed after OKAY, as 6.2's streaming install does.
struct AbbInstall {
    args: Vec<String>,
    payload: Vec<u8>,
}

impl Request for AbbInstall {
    fn serialize(&self) -> Vec<u8> {
        serialize_abb_exec(&self.args)
    }
}

impl ComplexRequest for AbbInstall {
    type Output = String;

    async fn read_element(&mut self, socket: &mut Socket) -> Result<String, AdbError> {
        socket.write_fully(&self.payload).await?;
        let mut t = StringResponseTransformer::default();
        socket.copy_to(&mut t, 4088, None).await;
        t.transform()
    }
}

#[tokio::test]
async fn request_strings_match_the_kotlin_transcript() {
    let dir = fixture("fake-adb/android-two-devices");
    let kotlin: BTreeSet<String> = std::fs::read_to_string(dir.join("expected/transcript.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|r| r["type"] == "request")
        .map(|r| r["service"].as_str().unwrap().to_string())
        .collect();
    let script = Script::load(&dir.join("script/devices.yaml")).unwrap();
    let server = FakeAdbServer::start(script, ServerOptions::default()).await.unwrap();
    let client = client(&server, 30_000);
    let serial = Some("emulator-5554");

    assert_eq!(client.execute(GetAdbServerVersionRequest, None).await.unwrap(), 41);
    assert_eq!(client.execute(ListDevicesRequest, None).await.unwrap().len(), 2);
    let mut track = client.execute_channel(AsyncDeviceMonitorRequest, None).unwrap();
    assert_eq!(track.next().await.unwrap().unwrap().len(), 2);
    drop(track);
    let features = client
        .execute(FetchDeviceFeaturesRequest::new("emulator-5554"), None)
        .await
        .unwrap();
    assert_eq!(
        features,
        [
            Feature::ShellV2,
            Feature::Cmd,
            Feature::StatV2,
            Feature::LsV2,
            Feature::AbbExec,
            Feature::SendrecvV2
        ]
    );
    let getprop = client
        .execute(ShellCommandRequest::new("getprop"), serial)
        .await
        .unwrap();
    assert_eq!(getprop.exit_code, 0);
    assert!(getprop.output().contains("[ro.build.version.sdk]: [33]"));
    let mut logcat = client
        .execute_channel(ChanneledShellCommandRequest::new("logcat -v long"), serial)
        .unwrap();
    assert!(
        logcat
            .next()
            .await
            .unwrap()
            .unwrap()
            .starts_with("--------- beginning of main")
    );
    drop(logcat);
    let instrument = kotlin
        .iter()
        .find_map(|s| s.strip_prefix("shell,v2,raw:"))
        .unwrap()
        .to_string();
    let mut run = client.execute_compat(CompatShellCommandRequest::new(instrument, features.clone()), serial);
    let mut stdout = Vec::new();
    let mut exit = None;
    while let Some(chunk) = run.next().await {
        let chunk = chunk.unwrap();
        stdout.extend(chunk.stdout.unwrap_or_default());
        exit = exit.or(chunk.exit_code);
    }
    assert_eq!(stdout, std::fs::read(dir.join("script/instrumentation.txt")).unwrap());
    assert_eq!(exit, Some(0));
    let size = 2630923usize;
    let args = ["package", "install", "-r", "-S", &size.to_string()]
        .map(String::from)
        .to_vec();
    let install = AbbInstall {
        args,
        payload: vec![0x5a; size],
    };
    assert_eq!(client.execute(install, serial).await.unwrap(), "Success");

    let ours: Vec<String> = records(&server, "request")
        .iter()
        .map(|r| r["service"].as_str().unwrap().to_string())
        .collect();
    for service in &ours {
        assert!(kotlin.contains(service), "not sent by Kotlin marathon: {service:?}");
    }
    for kind in [
        "host:version",
        "host:devices",
        "host:track-devices",
        "host-serial:emulator-5554:features",
        "host:transport:emulator-5554",
        "shell:getprop;echo x$?",
        "shell:logcat -v long",
        "shell,v2,raw:am instrument",
        "abb_exec:package\0install\0-r\0-S\0",
    ] {
        assert!(ours.iter().any(|s| s.starts_with(kind)), "{kind:?} not sent");
    }
    assert_eq!(server.warnings(), 0, "{:?}", server.transcript());
}

const ONE_DEVICE: &str = r#"
devices:
  - serial: emulator-5554
    features: [shell_v2, cmd, abb_exec]
  - serial: emulator-5556
    connected: false
shell:
  - { exact: "echo box", stdout: "box\n" }
  - { exact: "false", exit_code: 1 }
  - { exact: "exit 255", stdout: "out", stderr: "err", exit_code: 255 }
  - { exact: "exit 128", exit_code: 128 }
  - { exact: "sleep 0.2", delay_ms: 200, stdout: "late\n" }
  - { exact: "sleep 1.5", delay_ms: 1500, stdout: "late\n" }
  - { exact: "split", output: [{ stdout: "ж" }, { sleep_ms: 50 }, { stdout: "x\n" }] }
  - { prefix: "logcat", stdout: "line\n", hang: true }
  - { exact: "cmd package install -S 5", service: exec, stdout: "Success\n" }
  - { exact: "package list", service: abb_exec, stdout: " package:a\n" }
"#;

#[tokio::test]
async fn v1_exit_code_trick() {
    let server = start(ONE_DEVICE).await;
    let client = client(&server, 30_000);
    let serial = Some("emulator-5554");
    // The last x is the delimiter, so an x in the output stays in stdout.
    let r = client
        .execute(ShellCommandRequest::new("echo box"), serial)
        .await
        .unwrap();
    assert_eq!((r.output(), r.exit_code), ("box\n".to_string(), 0));
    let r = client.execute(ShellCommandRequest::new("false"), serial).await.unwrap();
    assert_eq!(r.exit_code, 1);
    // v1 has no stderr: it comes down stdout, and exit codes above 127 are not sign-extended.
    let r = client
        .execute(ShellCommandRequest::new("exit 255"), serial)
        .await
        .unwrap();
    assert_eq!((r.output(), r.exit_code), ("outerr".to_string(), 255));
    assert_eq!(server.warnings(), 0);
}

#[tokio::test]
async fn v2_exit_code_is_sign_extended_and_chunks_keep_their_order() {
    let server = start(ONE_DEVICE).await;
    let client = client(&server, 30_000);
    let serial = Some("emulator-5554");
    let mut chunks = Vec::new();
    let mut stream = client
        .execute_channel(v2::ChanneledShellCommandRequest::new("exit 255", None), serial)
        .unwrap();
    while let Some(chunk) = stream.next().await {
        chunks.push(chunk.unwrap());
    }
    assert_eq!(
        chunks,
        vec![
            ShellCommandResultChunk {
                stdout: Some(b"out".to_vec()),
                ..Default::default()
            },
            ShellCommandResultChunk {
                stderr: Some(b"err".to_vec()),
                ..Default::default()
            },
            // 255 read as a signed byte (known bug N13).
            ShellCommandResultChunk {
                exit_code: Some(-1),
                ..Default::default()
            },
        ]
    );
    let mut stream = client
        .execute_channel(v2::ChanneledShellCommandRequest::new("exit 128", None), serial)
        .unwrap();
    assert_eq!(stream.next().await.unwrap().unwrap().exit_code, Some(-128));
    assert!(stream.next().await.is_none());
    assert_eq!(server.warnings(), 0);
}

#[tokio::test]
async fn v2_stdin_is_written_and_a_closed_input_stops_the_stream() {
    let server = start(ONE_DEVICE).await;
    let client = client(&server, 30_000);
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    tx.send(ShellCommandInputChunk {
        stdin: Some(b"abc".to_vec()),
        close: true,
    })
    .await
    .unwrap();
    // Closing and draining the input stops the stream at its next turn, before the exit packet (N17).
    drop(tx);
    let mut stream = client
        .execute_channel(
            v2::ChanneledShellCommandRequest::new("sleep 0.2", Some(rx)),
            Some("emulator-5554"),
        )
        .unwrap();
    let mut items = Vec::new();
    while let Some(chunk) = stream.next().await {
        items.push(chunk.unwrap());
    }
    assert!(items.iter().all(|c| c.exit_code.is_none()), "{items:?}");
}

#[tokio::test]
async fn compat_shell_falls_back_to_v1_without_shell_v2() {
    let server = start(ONE_DEVICE).await;
    let client = client(&server, 30_000);
    let mut stream = client.execute_compat(CompatShellCommandRequest::new("split", vec![]), Some("emulator-5554"));
    let mut stdout = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        assert!(chunk.stderr.is_none() && chunk.exit_code.is_none());
        stdout.extend(chunk.stdout.unwrap());
    }
    assert_eq!(String::from_utf8(stdout).unwrap(), "жx\n");
    let request = records(&server, "request");
    assert_eq!(request.last().unwrap()["service"], "shell:split");
}

#[tokio::test]
async fn v1_stream_decodes_across_reads() {
    let server = start(ONE_DEVICE).await;
    let client = client(&server, 30_000);
    let mut stream = client
        .execute_channel(ChanneledShellCommandRequest::new("split"), Some("emulator-5554"))
        .unwrap();
    let mut text = String::new();
    while let Some(part) = stream.next().await {
        text.push_str(&part.unwrap());
    }
    assert_eq!(text, "жx\n");
}

#[tokio::test]
async fn track_devices_follows_events_and_ends_with_an_error_when_the_server_dies() {
    let server = start(ONE_DEVICE).await;
    let client = client(&server, 30_000);
    let mut track = client.execute_channel(AsyncDeviceMonitorRequest, None).unwrap();
    let device = |serial: &str| Device {
        serial: serial.into(),
        state: DeviceState::Device,
    };
    let mut next = async |want: Vec<Device>| loop {
        let list = track.next().await.unwrap().unwrap();
        if list == want {
            return;
        }
    };
    next(vec![device("emulator-5554")]).await;
    server.connect_device("emulator-5556").unwrap();
    next(vec![device("emulator-5554"), device("emulator-5556")]).await;
    server.disconnect_device("emulator-5554").unwrap();
    next(vec![device("emulator-5556")]).await;
    server.kill(None);
    match track.next().await {
        Some(Err(AdbError::RequestRejected(m))) => assert_eq!(m, "Unexpected string length: "),
        other => panic!("{other:?}"),
    }
    assert!(track.next().await.is_none());
}

#[tokio::test]
async fn dropping_a_hanging_logcat_stream_closes_it_quietly() {
    // The server notices the client is gone when a write fails, so more output follows the first line.
    let yaml = format!(
        "devices: [{{ serial: emulator-5554 }}]\nshell:\n  - {{ prefix: logcat, hang: true, output: [{{ stdout: \"line\\n\" }}, {{ sleep_ms: 200 }}, {{ stdout: \"{}\" }}] }}\n",
        "a".repeat(8 << 20)
    );
    let server = start(&yaml).await;
    let client = client(&server, 30_000);
    let mut logcat = client
        .execute_channel(
            ChanneledShellCommandRequest::new("logcat -v long"),
            Some("emulator-5554"),
        )
        .unwrap();
    assert_eq!(logcat.next().await.unwrap().unwrap(), "line\n");
    drop(logcat);
    wait_for_event(&server, "client_closed").await;
    assert_eq!(server.warnings(), 0, "{:?}", server.transcript());
}

#[tokio::test]
async fn exec_and_abb_exec_raw_sessions() {
    let server = start(ONE_DEVICE).await;
    let client = client(&server, 30_000);
    let serial = Some("emulator-5554");
    let output = client
        .execute(ExecRequest::new("cmd package install -S 5", b"12345".to_vec()), serial)
        .await
        .unwrap();
    assert_eq!(output, "Success");
    let args = vec!["package".to_string(), "list".to_string()];
    let output = client
        .execute(AbbExecRequest::new(args.clone(), vec![Feature::AbbExec]), serial)
        .await
        .unwrap();
    assert_eq!(output, "package:a");
    let shells = records(&server, "shell");
    assert_eq!(shells[0]["service"], "exec");
    assert_eq!(shells[0]["stdin_bytes"], 5);
    assert_eq!(shells[1]["service"], "abb_exec");
    // ABB_EXEC is checked before connecting.
    let error = client
        .execute(AbbExecRequest::new(args, vec![Feature::Cmd]), serial)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        AdbError::RequestValidation(
            "Request AbbExecRequest did not pass validation: ABB_EXEC is not supported by device".into()
        )
    );
    assert_eq!(server.warnings(), 0);
}

#[tokio::test]
async fn idle_timeout_counts_seconds() {
    let server = start(ONE_DEVICE).await;
    // 1 ms of socketIdleTimeout is one second of idle time (known bug N12).
    let client = client(&server, 1);
    let serial = Some("emulator-5554");
    let r = client
        .execute(ShellCommandRequest::new("sleep 0.2"), serial)
        .await
        .unwrap();
    assert_eq!(r.output(), "late\n");
    let started = Instant::now();
    let error = client
        .execute(ShellCommandRequest::new("sleep 1.5"), serial)
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    assert_eq!(
        error,
        AdbError::RequestRejected("No exit code delimiter found in ".into())
    );
    assert!(
        elapsed >= Duration::from_millis(900) && elapsed < Duration::from_millis(1450),
        "{elapsed:?}"
    );
}

#[tokio::test]
async fn close_ends_live_streams_and_refuses_new_requests() {
    let server = start(ONE_DEVICE).await;
    let client = client(&server, 30_000);
    let mut logcat = client
        .execute_channel(ChanneledShellCommandRequest::new("logcat"), Some("emulator-5554"))
        .unwrap();
    assert_eq!(logcat.next().await.unwrap().unwrap(), "line\n");
    let mut track = client.execute_channel(AsyncDeviceMonitorRequest, None).unwrap();
    assert!(track.next().await.unwrap().is_ok());
    let other = AndroidDebugBridgeClient::new(client.host(), client.port(), client.socket_factory().clone(), None);
    other.close();
    // The shared factory closed. The v1 stream reads its end, then fails on adam's socket close;
    // track-devices fails on the missing message first, which wins.
    assert_eq!(
        logcat.next().await,
        Some(Err(AdbError::IllegalState("Unknown deployment".into())))
    );
    assert!(logcat.next().await.is_none());
    match track.next().await {
        Some(Err(AdbError::RequestRejected(m))) => assert_eq!(m, "Unexpected string length: "),
        other => panic!("{other:?}"),
    }
    let error = client.execute(ListDevicesRequest, None).await.unwrap_err();
    assert_eq!(error, AdbError::VertxClosed);
}

#[tokio::test]
async fn connect_refused_is_a_connect_exception() {
    let server = start(ONE_DEVICE).await;
    let port = server.port();
    drop(server);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let factory = Arc::new(SocketFactory::new(10_000, 30_000));
    let client = AndroidDebugBridgeClient::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port, factory, None);
    let error = client.execute(GetAdbServerVersionRequest, None).await.unwrap_err();
    assert!(error.is_connect_exception(), "{error}");
    assert_eq!(
        error.message(),
        Some(format!("Connection refused: /127.0.0.1:{port}").as_str())
    );
    // Over a stream the same error is the stream's.
    let mut track = client.execute_channel(AsyncDeviceMonitorRequest, None).unwrap();
    assert!(matches!(track.next().await, Some(Err(AdbError::Connect(_)))));
}
