//! WP 6.2 request-level differential evidence against pinned adam's strict fake-server transcript.
use marathon_adb::{
    client::{
        AndroidDebugBridgeClient, Feature, SocketFactory,
        framebuffer::{BufferedImageScreenCaptureAdapter, ScreenCaptureRequest},
        pkg::{InstallSplitPackageRequest, StreamingPackageInstallRequest},
        reverse::{
            LocalPortSpec, PortForwardingMode, RemotePortSpec, RemoveReversePortForwardRequest,
            ReversePortForwardRequest,
        },
        sync::{
            CompatListFileRequest, CompatStatFileRequest, PullFileRequest, PushFileRequest, directory::PushRequest,
        },
    },
    server::{FakeAdbServer, Script, ServerOptions},
};
use serde_json::Value;
use std::{
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(path)
}
fn typed(rows: Vec<Value>) -> Vec<Value> {
    rows.into_iter()
        .filter(|v| {
            matches!(
                v["type"].as_str(),
                Some("request" | "sync" | "shell" | "framebuffer" | "framebuffer_ack")
            )
        })
        .map(|mut v| {
            let o = v.as_object_mut().unwrap();
            for key in ["conn", "t_ms", "at_ms", "time_ms", "seq"] {
                o.remove(key);
            }
            v
        })
        .collect()
}
fn client(s: &FakeAdbServer) -> AndroidDebugBridgeClient {
    AndroidDebugBridgeClient::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        s.port(),
        Arc::new(SocketFactory::new(10000, 30000)),
        None,
    )
}
#[tokio::test]
async fn kotlin_transcript_matches_production_requests_and_state() {
    let script = Script::load(&fixture("fake-adb/adb-requests/devices.yaml")).unwrap();
    let mut server = FakeAdbServer::start(script, ServerOptions::default()).await.unwrap();
    let client = client(&server);
    let serial = Some("emulator-5554");
    let dir = tempfile::tempdir().unwrap();
    let apk = dir.path().join("sample-fake.apk");
    tokio::fs::copy(fixture("adam/fixture/sample-fake.apk"), &apk)
        .await
        .unwrap();
    let path = apk.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::File::open(path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(0x01020304))
    })
    .await
    .unwrap()
    .unwrap();
    let bytes = tokio::fs::read(&apk).await.unwrap();
    for v2 in [false, true] {
        let features = if v2 {
            vec![Feature::SendrecvV2, Feature::StatV2, Feature::LsV2]
        } else {
            vec![]
        };
        let dest = if v2 { "/sdcard/v2.apk" } else { "/sdcard/v1.apk" };
        let mut request = PushFileRequest::prepare(apk.clone(), dest, "0777").await.unwrap();
        if v2 {
            request = request.with_v2(features.clone(), false);
        }
        let completion = request.completion().unwrap();
        let mut stream = client.execute_channel(request, serial).unwrap();
        let mut progress = vec![];
        while let Some(v) = stream.next().await {
            progress.push(v.unwrap());
        }
        completion.close().await.unwrap();
        assert_eq!(progress, vec![1.0, 1.0]);
        let remote = server.file("emulator-5554", dest).unwrap();
        assert_eq!(remote.content.as_ref().unwrap(), &bytes);
        assert_eq!(remote.mode, 0o777);
        assert_eq!(remote.mtime, 0x04030201);
        let stat = CompatStatFileRequest::new(dest, features.clone())
            .execute(&client, serial)
            .await
            .unwrap();
        assert_eq!(stat.size(), 614);
        assert!(stat.is_regular_file());
        assert!(
            CompatListFileRequest::new("/sdcard", features.clone())
                .execute(&client, serial)
                .await
                .unwrap()
                .iter()
                .any(|e| e.name() == Some(if v2 { "v2.apk" } else { "v1.apk" }))
        );
        let local = dir.path().join("pull.apk");
        let request = if v2 {
            PullFileRequest::v2(dest, local.clone(), features, None)
        } else {
            PullFileRequest::new(dest, local.clone(), None)
        };
        let completion = request.completion();
        let mut stream = client.execute_channel(request, serial).unwrap();
        while let Some(v) = stream.next().await {
            assert_eq!(v.unwrap(), 1.0);
        }
        completion.close().await.unwrap();
        assert_eq!(tokio::fs::read(local).await.unwrap(), bytes);
    }
    let request = StreamingPackageInstallRequest::prepare(apk.clone(), vec![Feature::AbbExec], true, vec![])
        .await
        .unwrap();
    let completion = request.completion().unwrap();
    assert!(client.execute(request, serial).await.unwrap().success);
    completion.close().await.unwrap();
    let request = InstallSplitPackageRequest::prepare(vec![apk.clone(), apk], vec![Feature::Cmd], true, vec![])
        .await
        .unwrap();
    let completion = request.completion();
    assert_eq!(
        request.execute(&client, serial).await.unwrap(),
        "Success [S1]\nSuccess\nSuccess\n\n"
    );
    completion.close().await.unwrap();
    assert_eq!(
        client
            .execute(
                ReversePortForwardRequest {
                    local: RemotePortSpec::Tcp(1234),
                    remote: LocalPortSpec::Tcp(5678),
                    mode: PortForwardingMode::Default
                },
                serial
            )
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        server.reverse_forwards("emulator-5554"),
        vec![("tcp:1234".into(), "tcp:5678".into())]
    );
    client
        .execute(
            RemoveReversePortForwardRequest {
                local: RemotePortSpec::Tcp(1234),
            },
            serial,
        )
        .await
        .unwrap();
    assert!(server.reverse_forwards("emulator-5554").is_empty());
    let raw = client.execute(ScreenCaptureRequest, serial).await.unwrap();
    let image = tokio::task::spawn_blocking(move || BufferedImageScreenCaptureAdapter.decode(&raw))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        i32::from_be_bytes([image.rgba[3], image.rgba[0], image.rgba[1], image.rgba[2]]),
        -2142403666
    );
    client.close();
    server.shutdown().await.unwrap();
    assert_eq!(server.warnings(), 0, "{:?}", server.transcript());
    let expected = tokio::fs::read_to_string(fixture("fake-adb/adb-requests/kotlin-transcript.jsonl"))
        .await
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(typed(server.transcript()), typed(expected));
}
#[tokio::test]
async fn existing_two_devices_scenario_accepts_streaming_request() {
    let mut server = FakeAdbServer::start(
        Script::load(&fixture("fake-adb/android-two-devices/script/devices.yaml")).unwrap(),
        ServerOptions::default(),
    )
    .await
    .unwrap();
    let client = client(&server);
    let r = StreamingPackageInstallRequest::prepare(
        fixture("adam/fixture/sample-fake.apk"),
        vec![Feature::AbbExec],
        true,
        vec![],
    )
    .await
    .unwrap();
    let completion = r.completion().unwrap();
    assert!(client.execute(r, Some("emulator-5554")).await.unwrap().success);
    completion.close().await.unwrap();
    server.shutdown().await.unwrap();
    assert_eq!(server.warnings(), 0);
}
#[tokio::test]
async fn directory_push_creates_directories_before_breadth_first_files() {
    let yaml = "server: {strict: true}\ndevices: [{serial: d}]\nshell:\n  - {prefix: 'mkdir ', stdout: ''}\n  - {exact: mkdir, stdout: ''}\n";
    let mut server = FakeAdbServer::start(
        Script::from_yaml(yaml).unwrap().compile(Path::new(".")).unwrap(),
        ServerOptions::default(),
    )
    .await
    .unwrap();
    let client = client(&server);
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("folder");
    tokio::fs::create_dir_all(source.join("nested")).await.unwrap();
    tokio::fs::write(source.join("root.apk"), b"abc").await.unwrap();
    tokio::fs::write(source.join("nested/inner.apk"), b"def").await.unwrap();
    let r = PushRequest::new(source, "/dest".into(), vec![]);
    let completion = r.completion();
    assert!(r.execute(&client, Some("d")).await.unwrap());
    completion.close().await.unwrap();
    server.shutdown().await.unwrap();
    assert_eq!(server.warnings(), 0);
    let rows = server.transcript();
    let commands: Vec<_> = rows
        .iter()
        .filter(|v| v["type"] == "request")
        .filter_map(|v| v["service"].as_str())
        .filter(|s| s.starts_with("shell:"))
        .collect();
    assert_eq!(
        commands,
        vec!["shell:mkdir -p '/dest';echo x$?", "shell:mkdir '/dest/nested';echo x$?"]
    );
    let paths: Vec<_> = rows
        .iter()
        .filter(|v| v["type"] == "sync" && v["op"] == "SEND")
        .filter_map(|v| v["path"].as_str())
        .collect();
    assert_eq!(paths, vec!["/dest/root.apk", "/dest/nested/inner.apk"]);
    assert_eq!(
        server.file("d", "/dest/root.apk").unwrap().content,
        Some(b"abc".to_vec())
    );
}
