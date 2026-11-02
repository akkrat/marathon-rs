//! SIGTERM shuts the binary down cleanly: open sessions and the timeline stop, and the transcript file
//! is complete, ending with `server_stopped`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

fn request(s: &mut TcpStream, req: &str) {
    s.write_all(format!("{:04x}{req}", req.len()).as_bytes()).unwrap();
}

fn okay(s: &mut TcpStream) {
    let mut status = [0u8; 4];
    s.read_exact(&mut status).unwrap();
    assert_eq!(&status, b"OKAY");
}

fn device(port: u16, service: &str) -> TcpStream {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    request(&mut s, "host:transport:d1");
    okay(&mut s);
    request(&mut s, service);
    okay(&mut s);
    s
}

/// Starts the binary on a free port; returns it, the port and the transcript path.
fn spawn(dir: &Path, script_text: &str) -> (Child, u16, PathBuf) {
    let script = dir.join("script.yaml");
    let port_file = dir.join("port");
    let transcript = dir.join("transcript.jsonl");
    std::fs::write(&script, script_text).unwrap();
    let _ = std::fs::remove_file(&port_file);
    let child = Command::new(env!("CARGO_BIN_EXE_fake-adb"))
        .args(["--port", "0", "--script"])
        .arg(&script)
        .arg("--port-file")
        .arg(&port_file)
        .arg("--transcript")
        .arg(&transcript)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let port: u16 = loop {
        if let Ok(p) = std::fs::read_to_string(&port_file)
            && let Ok(p) = p.parse()
        {
            break p;
        }
        assert!(Instant::now() < deadline, "no port file");
        std::thread::sleep(Duration::from_millis(10));
    };
    (child, port, transcript)
}

/// Sends SIGTERM, requires exit status 0 and a transcript ending with `server_stopped`; returns it.
fn terminate(mut child: Child, transcript: &Path) -> Vec<Value> {
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("fake-adb did not exit after SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "{status}");
    let text = std::fs::read_to_string(transcript).unwrap();
    let records: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let last = records.last().unwrap();
    assert_eq!(
        (&last["type"], &last["event"]),
        (&"event".into(), &"server_stopped".into()),
        "{text}"
    );
    assert_eq!(
        records.iter().filter(|r| r["event"] == "server_stopped").count(),
        1,
        "{text}"
    );
    records
}

const OPEN_SESSIONS: &str = r#"
devices:
  - serial: d1
    shell:
      - { prefix: logcat, stdout: "line\n", hang: true }
events:
  # Far in the future: the timeline task is still waiting when the signal arrives.
  - { at_ms: 600000, disconnect: d1 }
"#;

#[test]
fn sigterm_completes_the_transcript() {
    let dir = tempfile::tempdir().unwrap();
    // The missing record was a race, so try a few times.
    for _ in 0..5 {
        let (child, port, transcript) = spawn(dir.path(), OPEN_SESSIONS);
        // Sessions still open at the signal: a track-devices client and a hanging logcat.
        let mut track = TcpStream::connect(("127.0.0.1", port)).unwrap();
        request(&mut track, "host:track-devices");
        okay(&mut track);
        let mut logcat = device(port, "shell:logcat");
        let mut line = [0u8; 5];
        logcat.read_exact(&mut line).unwrap();
        let mut echo = device(port, "shell:echo done");
        let mut out = Vec::new();
        echo.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"done\n");

        let records = terminate(child, &transcript);
        let commands: Vec<&str> = records
            .iter()
            .filter(|r| r["type"] == "shell")
            .filter_map(|r| r["command"].as_str())
            .collect();
        assert_eq!(commands, ["logcat", "echo done"]);
        drop((track, logcat));
    }
}

const KILLED_THEN_DISCONNECTED: &str = r#"
devices: [{ serial: d1 }]
events:
  - { at_ms: 20, kill_server: { restart_after_ms: 600000 } }
  - { at_ms: 60, disconnect: d1 }
"#;

#[test]
fn sigterm_during_a_restart_wait_completes_the_transcript() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..5 {
        let (child, _port, transcript) = spawn(dir.path(), KILLED_THEN_DISCONNECTED);
        std::thread::sleep(Duration::from_millis(150));
        let records = terminate(child, &transcript);
        let events: Vec<&str> = records.iter().filter_map(|r| r["event"].as_str()).collect();
        assert_eq!(events, ["server_killed", "disconnect", "server_stopped"]);
    }
}
