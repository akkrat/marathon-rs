//! Drives the scripted server with the real `adb` client (`$ADB`, else `/opt/homebrew/bin/adb`) through
//! `-H 127.0.0.1 -P <port>`. Skipped, with a message, when there is no adb.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use marathon_adb::server::{FakeAdbServer, Script, ServerOptions};

const ADB_TIMEOUT: Duration = Duration::from_secs(60);

fn adb_path() -> Option<PathBuf> {
    std::env::var_os("ADB")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from("/opt/homebrew/bin/adb")))
        .filter(|p| p.is_file())
}

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(rel)
}

struct Adb {
    exe: PathBuf,
    port: u16,
    dir: PathBuf,
}

struct Out {
    code: i32,
    stdout: Vec<u8>,
    stderr: String,
}

impl Out {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

impl Adb {
    /// Runs adb with stdin from /dev/null and output in files (no pipe can fill up), killing it after
    /// [`ADB_TIMEOUT`].
    fn run(&self, args: &[&str]) -> Out {
        let (out_path, err_path) = (self.dir.join("stdout"), self.dir.join("stderr"));
        let port = self.port.to_string();
        let mut child = Command::new(&self.exe)
            .args(["-H", "127.0.0.1", "-P", &port])
            .args(args)
            .env_remove("ANDROID_ADB_SERVER_PORT")
            .env_remove("ADB_SERVER_SOCKET")
            .env_remove("ANDROID_SERIAL")
            .stdin(Stdio::null())
            .stdout(File::create(&out_path).unwrap())
            .stderr(File::create(&err_path).unwrap())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + ADB_TIMEOUT;
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("adb {args:?} timed out");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        Out {
            code: status.code().unwrap_or(-1),
            stdout: std::fs::read(&out_path).unwrap(),
            stderr: std::fs::read_to_string(&err_path).unwrap(),
        }
    }

    fn ok(&self, args: &[&str]) -> Out {
        let out = self.run(args);
        assert_eq!(out.code, 0, "adb {args:?} failed: {}{}", out.text(), out.stderr);
        out
    }
}

const SCRIPT: &str = r#"
devices:
  # Everything a recent emulator reports: shell v2, sync v2, abb_exec.
  - serial: emulator-5554
    getprop: { ro.product.model: "Pixel 7", ro.product.name: sdk_gphone64_arm64 }
    files:
      - { path: /sdcard/remote.txt, content: "from the device\n", mtime: 1700000000 }
    shell:
      - { exact: "exit 3", exit_code: 3 }
      - { exact: "both", stdout: "out\n", stderr: "err\n", exit_code: 1 }
      - { prefix: "screencap", service: exec, stdout_file: screen.png }
      - { prefix: "package install -S", service: abb_exec, stdout: "Success\n" }
      - { prefix: "settings get global enable_adb_incremental_install_default", stdout: "null\n" }
  # An old device: shell v1, sync v1, `cmd` but no abb_exec.
  - serial: v1-device
    features: [cmd]
    shell:
      - { prefix: "cmd package 'install' -S", service: exec, stdout: "Success\n" }
  # No `cmd` at all: adb falls back to push and `pm install`.
  - serial: legacy-device
    features: []
    shell:
      # adb quotes each argument: pm 'install' '/data/local/tmp/app-debug.apk'.
      - { regex: "^pm '?install'? ", stdout: "Success\n" }
      - { prefix: "rm '/data/local/tmp/", stdout: "" }
"#;

#[test]
fn real_adb_client() {
    let Some(exe) = adb_path() else {
        eprintln!("skipping real_adb_client: no adb at $ADB or /opt/homebrew/bin/adb");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let png = b"\x89PNG\r\n\x1a\nfake screen".to_vec();
    std::fs::write(dir.path().join("screen.png"), &png).unwrap();
    let script = Script::from_yaml(SCRIPT).unwrap().compile(dir.path()).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt
        .block_on(FakeAdbServer::start(script, ServerOptions::default()))
        .unwrap();
    let adb = Adb {
        exe,
        port: server.port(),
        dir: dir.path().to_path_buf(),
    };

    // devices
    assert_eq!(
        adb.ok(&["devices"]).text(),
        "List of devices attached\nemulator-5554\tdevice\nv1-device\tdevice\nlegacy-device\tdevice\n\n"
    );
    assert!(adb.ok(&["devices", "-l"]).text().contains("model:Pixel_7"));

    // shell: v2 carries stdout, stderr and the exit code; v1 only the output.
    let out = adb.ok(&["-s", "emulator-5554", "shell", "echo", "hello"]);
    assert_eq!(out.text(), "hello\n");
    assert_eq!(adb.run(&["-s", "emulator-5554", "shell", "exit 3"]).code, 3);
    let out = adb.run(&["-s", "emulator-5554", "shell", "both"]);
    assert_eq!(
        (out.code, out.text(), out.stderr.as_str()),
        (1, "out\n".into(), "err\n")
    );
    let out = adb.ok(&["-s", "v1-device", "shell", "echo", "v1"]);
    assert_eq!(out.text(), "v1\n");

    // push, ls, pull with sync v2 and v1.
    let local = dir.path().join("local.txt");
    std::fs::write(&local, "pushed\n").unwrap();
    for serial in ["emulator-5554", "v1-device"] {
        adb.ok(&["-s", serial, "push", local.to_str().unwrap(), "/sdcard/dir/pushed.txt"]);
        let node = server
            .file(serial, "/sdcard/dir/pushed.txt")
            .expect("pushed file missing");
        assert_eq!(node.content.as_deref(), Some(&b"pushed\n"[..]));
        let ls = adb.ok(&["-s", serial, "ls", "/sdcard/dir"]).text();
        assert!(
            ls.lines()
                .any(|l| l.ends_with(" pushed.txt") && l.starts_with("000081")),
            "{serial}: {ls}"
        );
    }
    let pulled = dir.path().join("pulled.txt");
    adb.ok(&[
        "-s",
        "emulator-5554",
        "pull",
        "/sdcard/remote.txt",
        pulled.to_str().unwrap(),
    ]);
    assert_eq!(std::fs::read_to_string(&pulled).unwrap(), "from the device\n");
    std::fs::remove_file(&pulled).unwrap();
    adb.ok(&[
        "-s",
        "v1-device",
        "pull",
        "/sdcard/dir/pushed.txt",
        pulled.to_str().unwrap(),
    ]);
    assert_eq!(std::fs::read_to_string(&pulled).unwrap(), "pushed\n");
    let out = adb.run(&[
        "-s",
        "emulator-5554",
        "pull",
        "/sdcard/missing",
        pulled.to_str().unwrap(),
    ]);
    assert_ne!(out.code, 0);
    assert!(out.stderr.contains("No such file or directory"), "{}", out.stderr);

    // install: streamed through abb_exec, streamed through exec, and legacy push + pm install.
    let apk = fixture("vendor-android/apk/app-debug.apk");
    let apk_size = std::fs::metadata(&apk).unwrap().len();
    for serial in ["emulator-5554", "v1-device", "legacy-device"] {
        let out = adb.ok(&["-s", serial, "install", apk.to_str().unwrap()]);
        assert!(
            out.text().contains("Success"),
            "{serial}: {}\n{:#?}",
            out.text(),
            server.transcript()
        );
    }

    // reverse
    adb.ok(&["-s", "emulator-5554", "reverse", "tcp:8080", "tcp:9090"]);
    assert_eq!(
        adb.ok(&["-s", "emulator-5554", "reverse", "--list"]).text().trim_end(),
        "host-1 tcp:8080 tcp:9090"
    );
    adb.ok(&["-s", "emulator-5554", "reverse", "--remove", "tcp:8080"]);
    assert!(server.reverse_forwards("emulator-5554").is_empty());
    let out = adb.run(&["-s", "emulator-5554", "reverse", "--remove", "tcp:8080"]);
    assert_ne!(out.code, 0);

    // exec-out screencap
    assert_eq!(
        adb.ok(&["-s", "emulator-5554", "exec-out", "screencap", "-p"]).stdout,
        png
    );

    let transcript = server.transcript();
    let shell = |service: &str| -> Vec<serde_json::Value> {
        transcript
            .iter()
            .filter(|r| r["type"] == "shell" && r["service"] == service)
            .cloned()
            .collect()
    };
    let installs: Vec<_> = [shell("abb_exec"), shell("exec")]
        .concat()
        .into_iter()
        .filter(|r| r["command"].as_str().unwrap().contains(" -S "))
        .collect();
    assert!(installs.iter().all(|r| r["stdin_bytes"] == apk_size), "{installs:?}");
    assert_eq!(installs.len(), 2, "{installs:?}");
    let legacy_push = transcript
        .iter()
        .find(|r| r["type"] == "sync" && r["serial"] == "legacy-device" && r["op"] == "SEND" && r["size"] == apk_size);
    assert!(legacy_push.is_some(), "no legacy push in the transcript");
    let warnings: Vec<_> = transcript.iter().filter(|r| r["type"] == "warning").collect();
    assert!(warnings.is_empty(), "{warnings:?}");

    // kill-server stops the fake like the real one.
    adb.ok(&["kill-server"]);
    rt.block_on(async { tokio::time::timeout(Duration::from_secs(10), server.stopped()).await })
        .unwrap();
}
