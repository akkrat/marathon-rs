//! Protocol tests: a minimal client speaks each service to an in-process server.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::{FakeAdbServer, Script, ServerOptions};

const TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) async fn start(yaml: &str) -> FakeAdbServer {
    start_in(yaml, Path::new(".")).await
}

async fn start_in(yaml: &str, base: &Path) -> FakeAdbServer {
    let script = Script::from_yaml(yaml).unwrap().compile(base).unwrap();
    FakeAdbServer::start(script, ServerOptions::default()).await.unwrap()
}

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(rel)
}

pub(crate) struct Client {
    s: TcpStream,
}

impl Client {
    pub async fn connect(port: u16) -> Client {
        Client {
            s: TcpStream::connect(("127.0.0.1", port)).await.unwrap(),
        }
    }

    pub async fn send(&mut self, req: &str) {
        self.s
            .write_all(format!("{:04X}{req}", req.len()).as_bytes())
            .await
            .unwrap();
    }

    pub async fn write(&mut self, data: &[u8]) {
        self.s.write_all(data).await.unwrap();
    }

    pub async fn read_n(&mut self, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        tokio::time::timeout(TIMEOUT, self.s.read_exact(&mut buf))
            .await
            .expect("read timed out")
            .unwrap();
        buf
    }

    pub async fn u32(&mut self) -> u32 {
        let b = self.read_n(4).await;
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }

    /// OKAY, or the FAIL message.
    pub async fn status(&mut self) -> Result<(), String> {
        match &self.read_n(4).await[..] {
            b"OKAY" => Ok(()),
            b"FAIL" => Err(self.string().await),
            other => panic!("bad status {other:?}"),
        }
    }

    pub async fn string(&mut self) -> String {
        let len = String::from_utf8(self.read_n(4).await).unwrap();
        let len = usize::from_str_radix(&len, 16).unwrap();
        String::from_utf8(self.read_n(len).await).unwrap()
    }

    pub async fn rest(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        tokio::time::timeout(TIMEOUT, self.s.read_to_end(&mut out))
            .await
            .expect("read timed out")
            .unwrap();
        out
    }

    /// `host:transport:<serial>`, then the device service.
    pub async fn device(port: u16, serial: &str, service: &str) -> Client {
        let mut c = Client::connect(port).await;
        c.send(&format!("host:transport:{serial}")).await;
        c.status().await.unwrap();
        c.send(service).await;
        c
    }

    async fn sync_req(&mut self, id: &[u8; 4], arg: &[u8]) {
        let mut out = id.to_vec();
        out.extend((arg.len() as u32).to_le_bytes());
        out.extend(arg);
        self.write(&out).await;
    }
}

pub(crate) async fn host(port: u16, req: &str) -> Result<String, String> {
    let mut c = Client::connect(port).await;
    c.send(req).await;
    c.status().await?;
    Ok(c.string().await)
}

/// adam's v1 `ShellCommandRequest`: `shell:<cmd>;echo x$?`, the exit code after the last `x`.
pub(crate) async fn shell_v1(port: u16, serial: &str, cmd: &str) -> (String, i32) {
    let mut c = Client::device(port, serial, &format!("shell:{cmd};echo x$?")).await;
    c.status().await.unwrap();
    let out = String::from_utf8(c.rest().await).unwrap();
    let i = out.rfind('x').expect("no exit code delimiter");
    (out[..i].to_string(), out[i + 1..].trim().parse().unwrap())
}

pub(crate) async fn shell_v2(port: u16, serial: &str, cmd: &str) -> (String, String, u8) {
    let mut c = Client::device(port, serial, &format!("shell,v2,raw:{cmd}")).await;
    c.status().await.unwrap();
    // Close stdin, as the real client does when its stdin is not a terminal.
    c.write(&[4, 0, 0, 0, 0]).await;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    loop {
        let id = c.read_n(1).await[0];
        let len = c.u32().await as usize;
        let data = c.read_n(len).await;
        match id {
            1 => out.extend(data),
            2 => err.extend(data),
            3 => {
                return (
                    String::from_utf8(out).unwrap(),
                    String::from_utf8(err).unwrap(),
                    data[0],
                );
            }
            other => panic!("unexpected packet {other}"),
        }
    }
}

fn records(server: &FakeAdbServer, kind: &str) -> Vec<Value> {
    server.transcript().into_iter().filter(|r| r["type"] == kind).collect()
}

const ONE_DEVICE: &str = r#"
devices:
  - serial: emulator-5554
    getprop: { ro.build.version.sdk: 33, ro.product.model: "Pixel 7", ro.product.name: sdk_gphone }
"#;

mod adb_server_extension_test {
    //! Port of adam 0.5.8 `server/server-stub-junit5/src/test/kotlin/com/malinskiy/adam/server/junit5/AdbServerExtensionTest.kt`.

    use super::*;

    #[tokio::test]
    async fn test_x() {
        let server = start("{}").await;
        let version = host(server.port(), "host:version").await.unwrap();
        assert_eq!(u32::from_str_radix(&version, 16).unwrap(), 41);
    }

    #[tokio::test]
    async fn test_multisession() {
        let server = start(
            r#"
devices:
  - serial: emulator-5554
    shell: [ { exact: echo 1, stdout: "1" }, { exact: echo 3, stdout: "3" } ]
  - serial: emulator-5556
    shell: [ { exact: echo 2, stdout: "2" }, { exact: echo 4, stdout: "4" } ]
"#,
        )
        .await;
        let p = server.port();
        assert_eq!(shell_v1(p, "emulator-5554", "echo 1").await.0, "1");
        assert_eq!(shell_v1(p, "emulator-5556", "echo 2").await.0, "2");
        assert_eq!(shell_v1(p, "emulator-5554", "echo 3").await.0, "3");
        assert_eq!(shell_v1(p, "emulator-5556", "echo 4").await.0, "4");
    }
}

mod adb_server_rule_test {
    //! Port of adam 0.5.8 `server/server-stub-junit4/src/test/kotlin/com/malinskiy/adam/server/junit4/AdbServerRuleTest.kt`.

    use super::*;

    #[tokio::test]
    async fn test() {
        let server = start("{}").await;
        let version = host(server.port(), "host:version").await.unwrap();
        assert_eq!(u32::from_str_radix(&version, 16).unwrap(), 41);
    }
}

#[tokio::test]
async fn host_version_devices_and_features() {
    let server = start(
        r#"
server: { adb_version: 40, host_features: [shell_v2, cmd] }
devices:
  - { serial: emulator-5554, getprop: { ro.product.model: "Pixel 7", ro.product.name: sdk_gphone } }
  - { serial: 127.0.0.1:5555, state: offline, features: [cmd] }
  - { serial: hidden, connected: false }
"#,
    )
    .await;
    let p = server.port();
    assert_eq!(host(p, "host:version").await.unwrap(), "0028");
    assert_eq!(
        host(p, "host:devices").await.unwrap(),
        "emulator-5554\tdevice\n127.0.0.1:5555\toffline\n"
    );
    assert_eq!(
        host(p, "host:devices-l").await.unwrap(),
        "emulator-5554          device product:sdk_gphone model:Pixel_7 transport_id:1\n\
         127.0.0.1:5555         offline transport_id:2\n"
    );
    assert_eq!(host(p, "host:host-features").await.unwrap(), "shell_v2,cmd");
    assert_eq!(host(p, "host-serial:127.0.0.1:5555:features").await.unwrap(), "cmd");
    assert!(
        host(p, "host-serial:emulator-5554:features")
            .await
            .unwrap()
            .contains("shell_v2")
    );
    assert_eq!(
        host(p, "host-serial:127.0.0.1:5555:get-state").await.unwrap(),
        "offline"
    );
    assert_eq!(
        host(p, "host-local:get-serialno").await,
        Err("more than one emulator".into())
    );
    assert_eq!(
        host(p, "host-serial:hidden:features").await,
        Err("device 'hidden' not found".into())
    );
    assert_eq!(host(p, "host:bogus").await, Err("unknown host service".into()));
    assert!(
        records(&server, "warning")[0]["message"]
            .as_str()
            .unwrap()
            .contains("host:bogus")
    );
}

#[tokio::test]
async fn transport_switches() {
    let server = start(
        r#"
devices:
  - { serial: a1, shell: [ { exact: id, stdout: "a1\n" } ] }
  - { serial: b2, state: unauthorized }
"#,
    )
    .await;
    let p = server.port();
    let mut c = Client::connect(p).await;
    c.send("host:transport:nope").await;
    assert_eq!(c.status().await, Err("device 'nope' not found".into()));
    let mut c = Client::connect(p).await;
    c.send("host:transport:b2").await;
    assert!(c.status().await.unwrap_err().starts_with("device unauthorized."));
    let mut c = Client::connect(p).await;
    c.send("host:transport-any").await;
    assert_eq!(c.status().await, Err("more than one device/emulator".into()));

    // tport answers with the transport id after OKAY.
    let mut c = Client::connect(p).await;
    c.send("host:tport:serial:a1").await;
    c.status().await.unwrap();
    assert_eq!(c.read_n(8).await, 1u64.to_le_bytes());
    c.send("shell:id").await;
    c.status().await.unwrap();
    assert_eq!(c.rest().await, b"a1\n");

    server.disconnect_device("b2").unwrap();
    let mut c = Client::connect(p).await;
    c.send("host:transport-any").await;
    c.status().await.unwrap();
    c.send("shell:id").await;
    c.status().await.unwrap();
    assert_eq!(c.rest().await, b"a1\n");
    let mut c = Client::connect(p).await;
    c.send("host:transport-id:1").await;
    c.status().await.unwrap();
}

#[tokio::test]
async fn track_devices_follows_api_changes_and_timeline() {
    let server = start(
        r#"
devices:
  - { serial: emulator-5554 }
  - { serial: emulator-5556, connected: false }
events:
  - { at_ms: 50, connect: emulator-5556 }
  - { at_ms: 100, state: { serial: emulator-5556, state: offline } }
  - { at_ms: 150, disconnect: emulator-5554 }
"#,
    )
    .await;
    let mut c = Client::connect(server.port()).await;
    c.send("host:track-devices").await;
    c.status().await.unwrap();
    assert_eq!(c.string().await, "emulator-5554\tdevice\n");
    assert_eq!(c.string().await, "emulator-5554\tdevice\nemulator-5556\tdevice\n");
    assert_eq!(c.string().await, "emulator-5554\tdevice\nemulator-5556\toffline\n");
    assert_eq!(c.string().await, "emulator-5556\toffline\n");
    server.set_device_state("emulator-5556", "device").unwrap();
    assert_eq!(c.string().await, "emulator-5556\tdevice\n");
    let events: Vec<Value> = records(&server, "event");
    assert_eq!(
        events.iter().map(|e| e["event"].as_str().unwrap()).collect::<Vec<_>>(),
        ["connect", "state", "disconnect", "state"]
    );
}

#[tokio::test]
async fn shell_v1_v2_exec_and_abb_exec() {
    let server = start(
        r#"
devices:
  - serial: emulator-5554
    getprop: { ro.build.version.sdk: 33, sys.boot_completed: 1 }
    shell:
      - { exact: "false", exit_code: 1 }
      - { exact: "both", output: [ { stdout: "out1 " }, { stderr: "err " }, { stdout: "out2" } ], exit_code: 3 }
      - { regex: "^ls( .*)?$", stdout: "a\nb\n" }
      - { prefix: "screencap", service: exec, stdout: "PNG" }
      - { exact: "package list packages", service: abb_exec, stdout: "package:x\n" }
default: { stdout: "not found\n", exit_code: 127 }
"#,
    )
    .await;
    let p = server.port();
    let s = "emulator-5554";
    assert_eq!(shell_v1(p, s, "false").await, (String::new(), 1));
    assert_eq!(shell_v1(p, s, "ls /sdcard").await, ("a\nb\n".into(), 0));
    // v1 merges stderr into stdout, in step order.
    assert_eq!(shell_v1(p, s, "both").await, ("out1 err out2".into(), 3));
    assert_eq!(shell_v2(p, s, "both").await, ("out1 out2".into(), "err ".into(), 3));
    // Built-ins.
    assert_eq!(
        shell_v2(p, s, "getprop").await.0,
        "[ro.build.version.sdk]: [33]\n[sys.boot_completed]: [1]\n"
    );
    assert_eq!(shell_v1(p, s, "getprop ro.build.version.sdk").await.0, "33\n");
    assert_eq!(
        shell_v2(p, s, "echo hello world").await,
        ("hello world\n".into(), String::new(), 0)
    );
    // Plain v1 without adam's suffix: the output only, then EOF.
    let mut c = Client::device(p, s, "shell:ls").await;
    c.status().await.unwrap();
    assert_eq!(c.rest().await, b"a\nb\n");
    // The service filter: screencap only answers exec.
    let mut c = Client::device(p, s, "exec:screencap -p").await;
    c.status().await.unwrap();
    assert_eq!(c.rest().await, b"PNG");
    assert_eq!(shell_v1(p, s, "screencap -p").await, ("not found\n".into(), 127));
    let mut c = Client::device(p, s, "abb_exec:package\0list\0packages").await;
    c.status().await.unwrap();
    assert_eq!(c.rest().await, b"package:x\n");

    let shells = records(&server, "shell");
    assert_eq!(shells[0]["command"], "false");
    assert_eq!(shells[0]["exit_code_trick"], true);
    assert_eq!(shells[0]["handler"], "script");
    assert_eq!(shells[0]["rule"], "devices[emulator-5554].shell[0]");
    assert_eq!(shells[3]["protocol"], "v2");
    assert_eq!(shells[4]["handler"], "builtin");
    let defaults: Vec<&Value> = shells.iter().filter(|r| r["handler"] == "default").collect();
    assert_eq!(defaults.len(), 1);
    assert_eq!(defaults[0]["command"], "screencap -p");
    let warnings = records(&server, "warning");
    assert!(warnings.iter().any(|w| {
        w["message"]
            .as_str()
            .unwrap()
            .contains("unmatched shell command \"screencap -p\"")
    }));
}

#[tokio::test]
async fn shared_expectations_times_and_builtins_off() {
    let server = start(
        r#"
server: { builtins: false }
devices:
  - serial: d1
    shell: [ { exact: "pm path x", times: 1, stdout: "first\n" } ]
  - serial: d2
shell:
  - { exact: "pm path x", stdout: "shared\n" }
"#,
    )
    .await;
    let p = server.port();
    assert_eq!(shell_v1(p, "d1", "pm path x").await.0, "first\n");
    assert_eq!(shell_v1(p, "d1", "pm path x").await.0, "shared\n");
    assert_eq!(shell_v1(p, "d2", "pm path x").await.0, "shared\n");
    assert_eq!(shell_v1(p, "d2", "getprop").await, (String::new(), 0));
    assert_eq!(records(&server, "shell")[3]["handler"], "default");
}

#[tokio::test]
async fn delay_stream_and_hang() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("instrumentation.txt"),
        "INSTRUMENTATION_STATUS: one\nINSTRUMENTATION_CODE: -1\n",
    )
    .unwrap();
    let server = start_in(
        r#"
devices:
  - serial: d1
    shell:
      - { exact: slow, delay_ms: 200, stdout: "done" }
      - { prefix: "am instrument", stdout_file: instrumentation.txt, chunk_size: 5, chunk_interval_ms: 1 }
      - { prefix: logcat, stdout: "line1\n", hang: true }
"#,
        dir.path(),
    )
    .await;
    let p = server.port();
    let t = std::time::Instant::now();
    assert_eq!(shell_v1(p, "d1", "slow").await.0, "done");
    assert!(t.elapsed() >= Duration::from_millis(200));
    assert_eq!(
        shell_v2(p, "d1", "am instrument -w -r x").await.0,
        "INSTRUMENTATION_STATUS: one\nINSTRUMENTATION_CODE: -1\n"
    );

    // A hanging command keeps the connection open until the client closes it.
    let mut c = Client::device(p, "d1", "shell:logcat -v long").await;
    c.status().await.unwrap();
    assert_eq!(c.read_n(6).await, b"line1\n");
    let mut probe = [0u8; 1];
    assert!(
        tokio::time::timeout(Duration::from_millis(200), c.s.read(&mut probe))
            .await
            .is_err(),
        "hang ended early"
    );
    drop(c);
}

#[tokio::test]
async fn disconnect_ends_open_sessions() {
    let server = start("devices: [{ serial: d1, shell: [ { prefix: logcat, hang: true } ] }]").await;
    let mut c = Client::device(server.port(), "d1", "shell:logcat").await;
    c.status().await.unwrap();
    server.disconnect_device("d1").unwrap();
    assert_eq!(c.rest().await, b"");
    assert!(
        records(&server, "event")
            .iter()
            .any(|e| e["event"] == "session_dropped")
    );
}

#[tokio::test]
async fn sync_v1_push_stat_list_pull() {
    let server = start(
        r#"
devices:
  - serial: d1
    files:
      - { path: /sdcard/existing.txt, content: "hello", mode: "0640", mtime: 1000 }
"#,
    )
    .await;
    let p = server.port();
    let mut c = Client::device(p, "d1", "sync:").await;
    c.status().await.unwrap();

    // STAT of a file and of nothing.
    c.sync_req(b"STAT", b"/sdcard/existing.txt").await;
    assert_eq!(c.read_n(4).await, b"STAT");
    assert_eq!((c.u32().await, c.u32().await, c.u32().await), (0o100640, 5, 1000));
    c.sync_req(b"STAT", b"/nope").await;
    assert_eq!(c.read_n(16).await, b"STAT\0\0\0\0\0\0\0\0\0\0\0\0");

    // SEND "<path>,<mode>" with two DATA chunks, then DONE with the mtime.
    c.sync_req(b"SEND", format!("/data/local/tmp/new/f.bin,{}", 0o644).as_bytes())
        .await;
    c.sync_req(b"DATA", b"abc").await;
    c.sync_req(b"DATA", b"def").await;
    let mut done = b"DONE".to_vec();
    done.extend(1234u32.to_le_bytes());
    c.write(&done).await;
    assert_eq!(c.read_n(8).await, b"OKAY\0\0\0\0");
    let node = server.file("d1", "/data/local/tmp/new/f.bin").unwrap();
    assert_eq!(
        (node.content.as_deref(), node.mode, node.mtime),
        (Some(&b"abcdef"[..]), 0o644, 1234)
    );

    // LIST: ".", "..", then the children.
    c.sync_req(b"LIST", b"/data/local/tmp/new").await;
    let mut names = Vec::new();
    loop {
        let id = c.read_n(4).await;
        let (mode, size, _mtime, namelen) = (c.u32().await, c.u32().await, c.u32().await, c.u32().await);
        if id == b"DONE" {
            break;
        }
        let name = String::from_utf8(c.read_n(namelen as usize).await).unwrap();
        names.push((name, mode, size));
    }
    assert_eq!(names[0].0, ".");
    assert_eq!(names[1].0, "..");
    assert_eq!(names[2], ("f.bin".to_string(), 0o100644, 6));

    // RECV: DATA chunks, then DONE.
    c.sync_req(b"RECV", b"/sdcard/existing.txt").await;
    assert_eq!(c.read_n(4).await, b"DATA");
    let n = c.u32().await;
    assert_eq!(c.read_n(n as usize).await, b"hello");
    assert_eq!(c.read_n(8).await, b"DONE\0\0\0\0");
    c.sync_req(b"QUIT", b"").await;
    assert_eq!(c.rest().await, b"");

    // A failed RECV ends the session with FAIL.
    let mut c = Client::device(p, "d1", "sync:").await;
    c.status().await.unwrap();
    c.sync_req(b"RECV", b"/missing").await;
    assert_eq!(c.read_n(4).await, b"FAIL");
    let n = c.u32().await;
    assert_eq!(c.read_n(n as usize).await, b"open failed: No such file or directory");
    assert_eq!(c.rest().await, b"");

    let ops: Vec<String> = records(&server, "sync")
        .iter()
        .map(|r| r["op"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ops, ["STAT", "STAT", "SEND", "LIST", "RECV", "QUIT", "RECV"]);
    let send = &records(&server, "sync")[2];
    assert_eq!(
        (send["path"].as_str(), send["size"].as_u64(), send["mode"].as_str()),
        (Some("/data/local/tmp/new/f.bin"), Some(6), Some("644"))
    );
}

#[tokio::test]
async fn sync_v2_stat_list_send_recv() {
    let server = start("devices: [{ serial: d1, files: [ { path: /sdcard/a.txt, content: abc, mtime: 77 } ] }]").await;
    let mut c = Client::device(server.port(), "d1", "sync:").await;
    c.status().await.unwrap();

    for id in [b"LST2", b"STA2"] {
        c.sync_req(id, b"/sdcard/a.txt").await;
        assert_eq!(&c.read_n(4).await, id);
        let body = c.read_n(68).await;
        assert_eq!(&body[0..4], &0u32.to_le_bytes()); // error
        assert_eq!(&body[20..24], &0o100644u32.to_le_bytes()); // mode
        assert_eq!(&body[36..44], &3u64.to_le_bytes()); // size
        assert_eq!(&body[52..60], &77i64.to_le_bytes()); // mtime
    }
    c.sync_req(b"LST2", b"/missing").await;
    assert_eq!(c.read_n(4).await, b"LST2");
    assert_eq!(&c.read_n(68).await[0..4], &2u32.to_le_bytes()); // ENOENT

    // SND2: path, then "SND2" mode flags, then DATA/DONE.
    c.sync_req(b"SND2", b"/sdcard/b.txt").await;
    let mut setup = b"SND2".to_vec();
    setup.extend(0o600u32.to_le_bytes());
    setup.extend(0u32.to_le_bytes());
    c.write(&setup).await;
    c.sync_req(b"DATA", b"xyz").await;
    c.write(b"DONE\x05\0\0\0").await;
    assert_eq!(c.read_n(8).await, b"OKAY\0\0\0\0");
    assert_eq!(server.file("d1", "/sdcard/b.txt").unwrap().content.unwrap(), b"xyz");

    c.sync_req(b"LIS2", b"/sdcard").await;
    let mut names = Vec::new();
    loop {
        let id = c.read_n(4).await;
        let body = c.read_n(72).await;
        if id == b"DONE" {
            break;
        }
        assert_eq!(id, b"DNT2");
        let namelen = u32::from_le_bytes([body[68], body[69], body[70], body[71]]);
        names.push(String::from_utf8(c.read_n(namelen as usize).await).unwrap());
    }
    assert_eq!(names, [".", "..", "a.txt", "b.txt"]);

    // RCV2: path, then "RCV2" flags.
    c.sync_req(b"RCV2", b"/sdcard/b.txt").await;
    c.write(b"RCV2\0\0\0\0").await;
    assert_eq!(c.read_n(8).await, b"DATA\x03\0\0\0");
    assert_eq!(c.read_n(3).await, b"xyz");
    assert_eq!(c.read_n(8).await, b"DONE\0\0\0\0");

    // Compression isn't implemented and says so.
    c.sync_req(b"RCV2", b"/sdcard/b.txt").await;
    c.write(b"RCV2\x01\0\0\0").await;
    assert_eq!(c.read_n(4).await, b"FAIL");
}

#[tokio::test]
async fn framebuffer_generated_and_replayed() {
    let replay = fixture("vendor-android/framebuffer/screencap_1.bin");
    let yaml = format!(
        r#"
devices:
  - {{ serial: v1, framebuffer: {{ version: 1, width: 2, height: 1, fill: [10, 20, 30, 255] }} }}
  - {{ serial: v2, framebuffer: {{ version: 2, width: 1, height: 1, fill: [1, 2, 3, 4], color_space: 1 }} }}
  - {{ serial: replay, framebuffer: {{ version: 1, file: {} }} }}
  - {{ serial: none }}
"#,
        replay.display()
    );
    let server = start(&yaml).await;
    let p = server.port();

    let mut c = Client::device(p, "v1", "framebuffer:").await;
    c.status().await.unwrap();
    let mut words = Vec::new();
    for _ in 0..13 {
        words.push(c.u32().await);
    }
    assert_eq!(words, [1, 32, 8, 2, 1, 0, 8, 16, 8, 8, 8, 24, 8]);
    c.write(&[0]).await; // adam's nudge byte
    assert_eq!(c.rest().await, [10, 20, 30, 255, 10, 20, 30, 255]);

    let mut c = Client::device(p, "v2", "framebuffer:").await;
    c.status().await.unwrap();
    let mut words = Vec::new();
    for _ in 0..14 {
        words.push(c.u32().await);
    }
    assert_eq!(words, [2, 32, 1, 4, 1, 1, 0, 8, 16, 8, 8, 8, 24, 8]);
    c.write(&[0]).await;
    assert_eq!(c.rest().await, [1, 2, 3, 4]);

    let mut c = Client::device(p, "replay", "framebuffer:").await;
    c.status().await.unwrap();
    let mut data = c.read_n(52).await;
    c.write(&[0]).await;
    data.extend(c.rest().await);
    assert_eq!(&data[..4], &1u32.to_le_bytes());
    assert_eq!(data[4..], std::fs::read(&replay).unwrap()[..]);

    let mut c = Client::device(p, "none", "framebuffer:").await;
    c.status().await.unwrap();
    assert_eq!(c.rest().await, b"");
}

// adam sends no nudge for an unknown version, so the server must not wait for one.
#[tokio::test]
async fn framebuffer_unknown_version_closes_without_ack() {
    let replay = fixture("vendor-android/framebuffer/screencap_1.bin");
    let yaml = format!(
        "devices: [{{ serial: d1, framebuffer: {{ version: 99, file: {} }} }}]",
        replay.display()
    );
    let server = start(&yaml).await;
    let mut c = Client::device(server.port(), "d1", "framebuffer:").await;
    c.status().await.unwrap();
    let data = c.rest().await;
    assert_eq!(&data[..4], &99u32.to_le_bytes());
    assert_eq!(data[4..], std::fs::read(&replay).unwrap()[..]);
}

#[tokio::test]
async fn reverse_forward_list_and_kill() {
    let server = start("devices: [{ serial: d1 }]").await;
    let p = server.port();
    let reverse = |req: &'static str| async move {
        let mut c = Client::device(p, "d1", req).await;
        c.status().await.unwrap();
        c
    };
    let mut c = reverse("reverse:forward:tcp:8080;tcp:9090").await;
    c.status().await.unwrap();
    assert_eq!(c.rest().await, b"");
    let mut c = reverse("reverse:forward:tcp:0;tcp:7000").await;
    c.status().await.unwrap();
    let port: u16 = c.string().await.parse().unwrap();
    let mut c = reverse("reverse:forward:norebind:tcp:8080;tcp:1").await;
    assert_eq!(c.status().await, Err("cannot rebind existing socket".into()));
    let mut c = reverse("reverse:list-forward").await;
    assert_eq!(
        c.string().await,
        format!("host-1 tcp:8080 tcp:9090\nhost-1 tcp:{port} tcp:7000\n")
    );
    let mut c = reverse("reverse:killforward:tcp:8080").await;
    c.status().await.unwrap();
    let mut c = reverse("reverse:killforward:tcp:8080").await;
    assert_eq!(c.status().await, Err("listener 'tcp:8080' not found".into()));
    assert_eq!(
        server.reverse_forwards("d1"),
        [(format!("tcp:{port}"), "tcp:7000".to_string())]
    );
    let mut c = reverse("reverse:killforward-all").await;
    c.status().await.unwrap();
    assert!(server.reverse_forwards("d1").is_empty());
}

#[tokio::test]
async fn installs_streaming_split_and_legacy() {
    let server = start(
        r#"
devices:
  - serial: d1
    shell:
      - { prefix: "cmd package install -S", stdout: "Success\n" }
      - { prefix: "package install -S", service: abb_exec, stdout: "Success\n" }
      - { prefix: "cmd package install-create", stdout: "Success: created install session [77]\n" }
      - { regex: "^cmd package install-write -S \\d+ 77 ", stdout: "Success: streamed\n" }
      - { exact: "cmd package install-commit 77", stdout: "Success\n" }
      - { prefix: "pm install -r /data/local/tmp/", stdout: "Success\n" }
"#,
    )
    .await;
    let p = server.port();
    let apk = vec![7u8; 100_000];

    let mut c = Client::device(p, "d1", &format!("exec:cmd package install -S {}", apk.len())).await;
    c.status().await.unwrap();
    c.write(&apk).await;
    assert_eq!(c.rest().await, b"Success\n");

    let mut c = Client::device(p, "d1", &format!("abb_exec:package\0install\0-S\0{}", apk.len())).await;
    c.status().await.unwrap();
    c.write(&apk).await;
    assert_eq!(c.rest().await, b"Success\n");

    let mut c = Client::device(
        p,
        "d1",
        &format!("exec:cmd package install-create '-S{}' -r", apk.len()),
    )
    .await;
    c.status().await.unwrap();
    assert_eq!(c.rest().await, b"Success: created install session [77]\n");
    let mut c = Client::device(
        p,
        "d1",
        &format!("exec:cmd package install-write -S {} 77 base.apk -", apk.len()),
    )
    .await;
    c.status().await.unwrap();
    c.write(&apk).await;
    assert_eq!(c.rest().await, b"Success: streamed\n");
    let mut c = Client::device(p, "d1", "exec:cmd package install-commit 77").await;
    c.status().await.unwrap();
    assert_eq!(c.rest().await, b"Success\n");

    // Legacy: push through sync, then pm install over shell.
    let mut c = Client::device(p, "d1", "sync:").await;
    c.status().await.unwrap();
    c.sync_req(b"SEND", b"/data/local/tmp/app.apk,420").await;
    c.sync_req(b"DATA", &apk[..60_000]).await;
    c.sync_req(b"DATA", &apk[60_000..]).await;
    c.write(b"DONE\0\0\0\0").await;
    assert_eq!(c.read_n(8).await, b"OKAY\0\0\0\0");
    c.sync_req(b"QUIT", b"").await;
    assert_eq!(server.file("d1", "/data/local/tmp/app.apk").unwrap().size(), 100_000);
    assert_eq!(
        shell_v1(p, "d1", "pm install -r /data/local/tmp/app.apk").await,
        ("Success\n".into(), 0)
    );

    // Digest the actual upload bytes, independent of socket/chunk boundaries.
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(&apk));
    for row in records(&server, "shell").iter().filter(|r| r["stdin_bytes"] == 100_000) {
        assert_eq!(row["stdin_sha256"], digest);
    }
    let sync_rows = records(&server, "sync");
    let push = sync_rows.iter().find(|r| r["op"] == "SEND").unwrap();
    assert_eq!(push["payload_sha256"], digest);

    let stdin: Vec<Value> = records(&server, "shell")
        .iter()
        .map(|r| r["stdin_bytes"].clone())
        .collect();
    assert_eq!(
        stdin,
        [
            Value::from(100_000),
            Value::from(100_000),
            Value::Null,
            Value::from(100_000),
            Value::Null,
            Value::Null
        ]
    );
    assert!(
        records(&server, "warning").is_empty(),
        "{:?}",
        records(&server, "warning")
    );
}

#[tokio::test]
async fn host_kill_exit_and_ignore() {
    let server = start("{}").await;
    let mut c = Client::connect(server.port()).await;
    c.send("host:kill").await;
    c.status().await.unwrap();
    tokio::time::timeout(TIMEOUT, server.stopped()).await.unwrap();
    assert!(TcpStream::connect(("127.0.0.1", server.port())).await.is_err());

    let server = start("server: { on_kill: ignore }").await;
    let mut c = Client::connect(server.port()).await;
    c.send("host:kill").await;
    c.status().await.unwrap();
    assert_eq!(host(server.port(), "host:version").await.unwrap(), "0029");
}

#[tokio::test]
async fn kill_server_event_drops_connections_and_restarts() {
    let server = start(
        r#"
devices: [{ serial: d1, shell: [ { prefix: logcat, hang: true } ] }]
events: [{ at_ms: 100, kill_server: { restart_after_ms: 300 } }]
"#,
    )
    .await;
    let p = server.port();
    let mut c = Client::device(p, "d1", "shell:logcat").await;
    c.status().await.unwrap();
    assert_eq!(c.rest().await, b"");
    // Down for a while, then back on the same port with the same devices.
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        if let Ok(mut c) = TcpStream::connect(("127.0.0.1", p)).await {
            c.write_all(b"000Chost:devices").await.unwrap();
            let mut out = Vec::new();
            c.read_to_end(&mut out).await.unwrap();
            assert_eq!(out, b"OKAY000ad1\tdevice\n");
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let events: Vec<String> = records(&server, "event")
        .iter()
        .map(|e| e["event"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(events, ["server_killed", "server_restarted"]);
}

#[tokio::test]
async fn unknown_device_service_fails() {
    let server = start("devices: [{ serial: d1 }]").await;
    let mut c = Client::device(server.port(), "d1", "jdwp:1234").await;
    assert_eq!(c.status().await, Err("closed".into()));
    // Without a transport switch, a device service goes to the only device.
    let mut c = Client::connect(server.port()).await;
    c.send("shell:echo hi").await;
    c.status().await.unwrap();
    assert_eq!(c.rest().await, b"hi\n");
}

#[tokio::test]
async fn transcript_file_is_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    let script = Script::from_yaml(ONE_DEVICE).unwrap().compile(Path::new(".")).unwrap();
    let server = FakeAdbServer::start(
        script,
        ServerOptions {
            transcript: Some(path.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    shell_v1(server.port(), "emulator-5554", "getprop ro.product.model").await;
    let want = server.transcript().len();
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        if lines.len() == want {
            assert_eq!(lines[0]["service"], "host:transport:emulator-5554");
            assert_eq!(lines[1]["service"], "shell:getprop ro.product.model;echo x$?");
            assert_eq!(lines[1]["serial"], "emulator-5554");
            assert_eq!(lines[2]["command"], "getprop ro.product.model");
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{text}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn short_install_payloads_fail_without_using_the_expectation() {
    let server = start(
        r#"
devices:
  - serial: d1
    shell:
      - { prefix: "cmd package install -S", times: 1, stdout: "Success\n" }
      - { prefix: "package install -S", service: abb_exec, times: 1, stdout: "Success\n" }
"#,
    )
    .await;
    let p = server.port();
    for service in [
        "exec:cmd package install -S 100",
        "abb_exec:package\0install\0-S\x00100",
    ] {
        let mut c = Client::device(p, "d1", service).await;
        c.status().await.unwrap();
        c.write(&[1u8; 10]).await;
        c.s.shutdown().await.unwrap();
        assert_eq!(
            String::from_utf8(c.rest().await).unwrap(),
            "Failure [INSTALL_FAILED_INVALID_APK: fake adb received 10 of 100 bytes]\n"
        );
        // The success expectation is still there for a complete upload.
        let mut c = Client::device(p, "d1", service).await;
        c.status().await.unwrap();
        c.write(&[1u8; 100]).await;
        assert_eq!(c.rest().await, b"Success\n");
    }
    let shells = records(&server, "shell");
    let short: Vec<&Value> = shells.iter().filter(|r| r["handler"] == "short_payload").collect();
    assert_eq!(short.len(), 2);
    for r in short {
        assert_eq!(
            (r["stdin_bytes"].as_u64(), r["stdin_expected"].as_u64()),
            (Some(10), Some(100))
        );
        assert_eq!(r["exit_code"], 1);
        assert_eq!(r["rule"], Value::Null);
    }
    assert_eq!(
        shells
            .iter()
            .filter(|r| r["handler"] == "script" && r["stdin_bytes"] == 100)
            .count(),
        2
    );
    assert_eq!(server.warnings(), 2);
}

/// After the server has closed both halves, the client's writes are refused (RST), which they aren't
/// while a leaked task still holds the read half.
async fn assert_fully_closed(c: &mut Client) {
    assert_eq!(c.rest().await, b"");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if c.s.write_all(b"x").await.is_err() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the server still holds the read side"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn cancelled_sessions_release_the_socket() {
    let server = start("devices: [{ serial: d1, shell: [ { prefix: logcat, stdout: \"x\\n\", hang: true } ] }]").await;
    // The client keeps its write side open in both cases.
    let mut c = Client::device(server.port(), "d1", "shell,v2,raw:logcat").await;
    c.status().await.unwrap();
    c.read_n(7).await;
    server.disconnect_device("d1").unwrap();
    assert_fully_closed(&mut c).await;

    server.connect_device("d1").unwrap();
    let mut c = Client::device(server.port(), "d1", "shell:logcat").await;
    c.status().await.unwrap();
    c.read_n(2).await;
    server.kill(None);
    assert_fully_closed(&mut c).await;
}

#[tokio::test]
async fn track_devices_ends_when_the_client_leaves() {
    let server = start("devices: [{ serial: d1 }]").await;
    let mut c = Client::connect(server.port()).await;
    c.send("host:track-devices").await;
    c.status().await.unwrap();
    assert_eq!(c.string().await, "d1\tdevice\n");
    assert_eq!(server.track_sessions(), 1);
    drop(c);
    // No device event follows; the session must notice the EOF by itself.
    let deadline = std::time::Instant::now() + TIMEOUT;
    while server.track_sessions() > 0 {
        assert!(std::time::Instant::now() < deadline, "track-devices session lingers");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn scripted_directory_modes_are_kept() {
    let server = start(
        r#"devices: [{ serial: d1, files: [ { path: /sdcard/private/deep, dir: true, mode: "0700", mtime: 5 } ] }]"#,
    )
    .await;
    let node = server.file("d1", "/sdcard/private/deep").unwrap();
    assert_eq!((node.st_mode(), node.mtime), (0o040700, 5));
    assert_eq!(server.file("d1", "/sdcard/private").unwrap().st_mode(), 0o040755);
    let mut c = Client::device(server.port(), "d1", "sync:").await;
    c.status().await.unwrap();
    c.sync_req(b"STAT", b"/sdcard/private/deep").await;
    assert_eq!(c.read_n(4).await, b"STAT");
    assert_eq!(c.u32().await, 0o040700);
}

#[tokio::test]
async fn strict_mode_rejects_unmatched_commands() {
    let server = start(
        r#"
server: { strict: true }
devices: [{ serial: d1, getprop: { a: b }, shell: [ { exact: once, times: 1, stdout: "1\n" } ] }]
"#,
    )
    .await;
    let p = server.port();
    assert_eq!(shell_v1(p, "d1", "once").await.0, "1\n");
    assert_eq!(shell_v1(p, "d1", "getprop a").await.0, "b\n");
    assert_eq!(server.warnings(), 0);
    for (service, cmd) in [
        ("shell:once", "shell command 'once'"),
        ("exec:typo -x", "exec command 'typo -x'"),
    ] {
        let mut c = Client::device(p, "d1", service).await;
        assert_eq!(c.status().await, Err(format!("fake adb: no expectation for {cmd}")));
    }
    assert_eq!(server.warnings(), 2);
    let rejected = records(&server, "shell")
        .into_iter()
        .filter(|r| r["handler"] == "rejected")
        .count();
    assert_eq!(rejected, 2);
}

#[tokio::test]
async fn concurrent_uploads_cannot_share_a_limited_expectation() {
    let server = start(
        r#"
server: { strict: true }
devices: [{ serial: d1, shell: [ { prefix: "cmd package install -S", times: 1, stdout: "Success\n" } ] }]
"#,
    )
    .await;
    let p = server.port();
    let service = "exec:cmd package install -S 100";
    let rejected = Err("fake adb: no expectation for exec command 'cmd package install -S 100'".to_string());

    // A holds the only use while its upload is in flight, so B is refused.
    let mut a = Client::device(p, "d1", service).await;
    a.status().await.unwrap();
    a.write(&[1u8; 50]).await;
    let mut b = Client::device(p, "d1", service).await;
    assert_eq!(b.status().await, rejected);

    // A's upload ends early: the use goes back.
    a.s.shutdown().await.unwrap();
    assert!(
        String::from_utf8(a.rest().await)
            .unwrap()
            .starts_with("Failure [INSTALL_FAILED_INVALID_APK")
    );
    let mut c = Client::device(p, "d1", service).await;
    c.status().await.unwrap();
    c.write(&[1u8; 100]).await;
    assert_eq!(c.rest().await, b"Success\n");

    // Now it is used up.
    let mut d = Client::device(p, "d1", service).await;
    assert_eq!(d.status().await, rejected);
}

#[tokio::test]
async fn shutdown_completes_the_transcript_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let script = Script::from_yaml(
        "devices: [{ serial: d1, shell: [ { prefix: logcat, hang: true } ] }]\nevents: [{ at_ms: 600000, disconnect: d1 }]",
    )
    .unwrap()
    .compile(Path::new("."))
    .unwrap();
    let mut server = FakeAdbServer::start(
        script,
        ServerOptions {
            transcript: Some(path.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut c = Client::device(server.port(), "d1", "shell:logcat").await;
    c.status().await.unwrap();
    server.shutdown().await.unwrap();
    assert_eq!(c.rest().await, b"");
    let text = std::fs::read_to_string(&path).unwrap();
    let last: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert_eq!(last["event"], "server_stopped", "{text}");
    assert_eq!(text.lines().count(), server.transcript().len());
}

/// Retries the upload until the server accepts it (the reservation comes back asynchronously, when the
/// server notices the failed session), then sends it whole.
async fn upload_when_released(p: u16, service: &str) -> Vec<u8> {
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        let mut c = Client::device(p, "d1", service).await;
        if c.status().await.is_ok() {
            c.write(&[1u8; 100]).await;
            return c.rest().await;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the reservation was never released"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

const ONE_UPLOAD: &str = r#"
server: { strict: true }
devices: [{ serial: d1, shell: [ { prefix: "cmd package install -S", times: 1, stdout: "Success\n" } ] }]
"#;

#[tokio::test]
async fn reservation_is_released_after_a_connection_reset() {
    let server = start(ONE_UPLOAD).await;
    let p = server.port();
    let service = "exec:cmd package install -S 100";
    let mut a = Client::device(p, "d1", service).await;
    a.status().await.unwrap();
    a.write(&[1u8; 50]).await;
    // Linger 0 turns the close into a reset, so the server's read fails instead of seeing EOF.
    #[allow(deprecated)]
    a.s.set_linger(Some(Duration::ZERO)).unwrap();
    drop(a);
    assert_eq!(upload_when_released(p, service).await, b"Success\n");
}

#[tokio::test]
async fn reservation_is_released_when_the_device_reconnects_mid_upload() {
    let server = start(ONE_UPLOAD).await;
    let p = server.port();
    let service = "exec:cmd package install -S 100";
    let mut a = Client::device(p, "d1", service).await;
    a.status().await.unwrap();
    a.write(&[1u8; 50]).await;
    server.disconnect_device("d1").unwrap();
    // The session is dropped with our unread bytes in its buffer, so this ends in EOF or a reset.
    let mut rest = Vec::new();
    let _ = a.s.read_to_end(&mut rest).await;
    server.connect_device("d1").unwrap();
    assert_eq!(upload_when_released(p, service).await, b"Success\n");
}

#[tokio::test]
async fn a_client_closing_mid_stream_is_not_a_warning() {
    // Found running the Kotlin marathon: it abandons instrumentation and logcat streams, and the writes that
    // follow fail with a broken pipe or a reset. That must not fail a strict run.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("big.txt"), vec![b'x'; 4 * 1024 * 1024]).unwrap();
    let server = start_in(
        r#"
server: { strict: true }
devices: [{ serial: d1, shell: [ { prefix: "am instrument", stdout_file: big.txt, chunk_size: 1024, chunk_interval_ms: 1 } ] }]
"#,
        dir.path(),
    )
    .await;
    let mut c = Client::device(server.port(), "d1", "shell,v2,raw:am instrument -w -r x").await;
    c.status().await.unwrap();
    c.read_n(100).await;
    #[allow(deprecated)]
    c.s.set_linger(Some(Duration::ZERO)).unwrap();
    drop(c);
    let deadline = std::time::Instant::now() + TIMEOUT;
    while !records(&server, "event").iter().any(|e| e["event"] == "client_closed") {
        assert!(std::time::Instant::now() < deadline, "{:?}", server.transcript());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(server.warnings(), 0, "{:?}", records(&server, "warning"));
}

#[tokio::test]
async fn a_reset_during_an_upload_warns_in_strict_mode() {
    let server = start(
        r#"
server: { strict: true }
devices: [{ serial: d1, shell: [ { prefix: "package install -S", service: abb_exec, stdout: "Success\n" } ] }]
"#,
    )
    .await;
    let mut c = Client::device(server.port(), "d1", "abb_exec:package\0install\0-S\x00100").await;
    c.status().await.unwrap();
    c.write(&[1u8; 50]).await;
    #[allow(deprecated)]
    c.s.set_linger(Some(Duration::ZERO)).unwrap();
    drop(c);
    let deadline = std::time::Instant::now() + TIMEOUT;
    let short = loop {
        if let Some(r) = records(&server, "shell")
            .into_iter()
            .find(|r| r["handler"] == "short_payload")
        {
            break r;
        }
        assert!(std::time::Instant::now() < deadline, "{:?}", server.transcript());
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(
        (short["stdin_bytes"].as_u64(), short["stdin_expected"].as_u64()),
        (Some(50), Some(100))
    );
    assert!(short["error"].is_string(), "{short}");
    assert_eq!(server.warnings(), 1, "{:?}", records(&server, "warning"));
    assert!(!records(&server, "event").iter().any(|e| e["event"] == "client_closed"));
}

#[tokio::test]
async fn captured_output_and_complete_device_state_survive_shutdown() -> Result<(), Box<dyn std::error::Error>> {
    let script = Script::from_yaml(
        "devices: [{serial: d1, files: [{path: /sdcard/kept, content: literal, mode: '0600', mtime: 7}], shell: [{exact: echo, stdout: 'out', stderr: 'err'}]}]",
    )?.compile(Path::new("."))?;
    let mut server = FakeAdbServer::start(script, ServerOptions::default()).await?;
    server.capture_output();
    let mut forward = Client::device(server.port(), "d1", "reverse:forward:tcp:8080;tcp:9090").await;
    forward.status().await.map_err(std::io::Error::other)?;
    forward.status().await.map_err(std::io::Error::other)?;
    assert!(forward.rest().await.is_empty());
    let mut shell = Client::device(server.port(), "d1", "shell:echo").await;
    shell.status().await.map_err(std::io::Error::other)?;
    assert_eq!(shell.rest().await, b"outerr");
    server.shutdown().await?;
    let snapshot = server.snapshot();
    let device = &snapshot["devices"][0];
    assert_eq!(device["serial"], "d1");
    assert_eq!(device["connected"], true);
    assert_eq!(device["reverse"], serde_json::json!([["tcp:8080", "tcp:9090"]]));
    let files = device["files"].as_array().ok_or("missing filesystem")?;
    let file = files
        .iter()
        .find(|f| f["path"] == "/sdcard/kept")
        .ok_or("missing kept file")?;
    assert_eq!(file["mode"], 0o600);
    assert_eq!(file["mtime"], 7);
    assert_eq!(file["content"], serde_json::json!(b"literal".to_vec()));
    let rows = server.transcript();
    let outputs: Vec<_> = rows.iter().filter(|r| r["type"] == "output").collect();
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0]["stream"], "stdout");
    assert_eq!(outputs[0]["bytes"], serde_json::json!(b"out".to_vec()));
    assert_eq!(outputs[1]["stream"], "stderr");
    assert_eq!(outputs[1]["bytes"], serde_json::json!(b"err".to_vec()));
    assert_eq!(rows.last().ok_or("missing final event")?["event"], "server_stopped");
    Ok(())
}
