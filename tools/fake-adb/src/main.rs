//! `fake-adb`: the scripted ADB server of `marathon-adb` as a standalone process, for runs of the Kotlin
//! marathon (through `adbServers`) and of the real `adb` client. The device script format is documented in
//! `docs/design/fake-adb-script.md`.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use marathon_adb::server::{CompiledScript, FakeAdbServer, Script, ServerOptions};

const USAGE: &str =
    "usage: fake-adb --script <file> [--port <n>] [--host <ip>] [--transcript <file>] [--port-file <file>] [--state <file>] [--strict]

  --script      device script (YAML or JSON)
  --port        port to listen on; 0 picks a free one (default 5037)
  --host        address to listen on (default 127.0.0.1)
  --transcript  write the JSONL request transcript to this file
  --port-file   write the port to this file once listening
  --state       export final device files/reverse/state after joined shutdown; capture shell output
  --strict      unmatched commands get an error, and the exit status is 1 if anything was
                recorded as a warning (also `server: { strict: true }` in the script)

Runs until SIGINT or SIGTERM, or until `adb kill-server` when the script's on_kill is exit. The
transcript is flushed before exiting; a transcript write error also gives exit status 1.";

#[derive(Debug)]
struct Args {
    script: PathBuf,
    options: ServerOptions,
    port_file: Option<PathBuf>,
    strict: bool,
    state_file: Option<PathBuf>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut script = None;
    let mut options = ServerOptions {
        port: 5037,
        ..ServerOptions::default()
    };
    let mut port_file = None;
    let mut strict = false;
    let mut state_file = None;
    while let Some(a) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--script" => script = Some(PathBuf::from(value()?)),
            "--port" => options.port = value()?.parse().map_err(|e| format!("--port: {e}"))?,
            "--host" => options.host = value()?.parse::<IpAddr>().map_err(|e| format!("--host: {e}"))?,
            "--transcript" => options.transcript = Some(PathBuf::from(value()?)),
            "--port-file" => port_file = Some(PathBuf::from(value()?)),
            "--state" => state_file = Some(PathBuf::from(value()?)),
            "--strict" => strict = true,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args {
        script: script.ok_or("--script is required")?,
        options,
        port_file,
        strict,
        state_file,
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("fake-adb: {e}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let mut script = match Script::load(&args.script) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fake-adb: {e}");
            return ExitCode::FAILURE;
        }
    };
    script.strict |= args.strict;
    let shutdown = match shutdown_signal() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fake-adb: cannot handle signals: {e}");
            return ExitCode::FAILURE;
        }
    };
    match serve_with_state(
        script,
        args.options,
        args.port_file.as_deref(),
        args.state_file.as_deref(),
        shutdown,
    )
    .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fake-adb: {e}");
            ExitCode::FAILURE
        }
    }
}

/// SIGINT or SIGTERM; a harness usually stops the server with the latter. The handlers are installed
/// here, before the port file exists: a lazily polled handler left a window in which a harness that
/// had already used the server could kill it with the default action.
fn shutdown_signal() -> std::io::Result<impl Future<Output = std::io::Result<()>>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = interrupt.recv() => Ok(()),
            _ = term.recv() => Ok(()),
        }
    })
}

/// Serves until `adb kill-server` (with `on_kill: exit`) or `shutdown`, then shuts the server down and
/// closes the transcript.
/// Fails on a transcript write error, and in strict mode if any warning was recorded.
#[cfg(test)]
async fn serve(
    script: CompiledScript,
    options: ServerOptions,
    port_file: Option<&Path>,
    shutdown: impl Future<Output = std::io::Result<()>>,
) -> Result<(), String> {
    serve_with_state(script, options, port_file, None, shutdown).await
}

async fn serve_with_state(
    script: CompiledScript,
    options: ServerOptions,
    port_file: Option<&Path>,
    state_file: Option<&Path>,
    shutdown: impl Future<Output = std::io::Result<()>>,
) -> Result<(), String> {
    let strict = script.strict;
    let mut server = FakeAdbServer::start(script, options)
        .await
        .map_err(|e| format!("cannot start: {e}"))?;
    if state_file.is_some() {
        server.capture_output();
    }
    if let Some(p) = port_file {
        tokio::fs::write(p, server.port().to_string())
            .await
            .map_err(|e| format!("cannot write {}: {e}", p.display()))?;
    }
    println!("fake-adb listening on {}", server.addr());
    tokio::select! {
        _ = server.stopped() => println!("fake-adb stopped by kill-server"),
        r = shutdown => r.map_err(|e| format!("waiting for a signal: {e}"))?,
    }
    // Stops the timeline and the connections first, so that the transcript is complete when closed.
    server.shutdown().await.map_err(|e| e.to_string())?;
    if let Some(path) = state_file {
        let snapshot = serde_json::to_vec_pretty(&server.snapshot()).map_err(|e| e.to_string())?;
        tokio::fs::write(path, snapshot)
            .await
            .map_err(|e| format!("cannot write state: {e}"))?;
    }
    let warnings = server.warnings();
    if strict && warnings > 0 {
        return Err(format!(
            "strict mode: {warnings} warning(s) recorded; see the transcript"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn arguments() {
        let a = parse(&[
            "--script",
            "s.yaml",
            "--port",
            "0",
            "--transcript",
            "t.jsonl",
            "--port-file",
            "p",
        ])
        .unwrap();
        assert_eq!(a.script, PathBuf::from("s.yaml"));
        assert_eq!(a.options.port, 0);
        assert_eq!(a.options.transcript, Some(PathBuf::from("t.jsonl")));
        assert_eq!(a.port_file, Some(PathBuf::from("p")));
        assert!(!a.strict);
        assert!(parse(&["--script", "s", "--strict"]).unwrap().strict);
        assert_eq!(parse(&["--script", "s"]).unwrap().options.port, 5037);
        assert!(parse(&[]).unwrap_err().contains("--script is required"));
        assert!(parse(&["--port"]).unwrap_err().contains("needs a value"));
        assert!(
            parse(&["--script", "s", "--bogus"])
                .unwrap_err()
                .contains("unknown argument")
        );
    }

    /// Waits for the port file, sends each request on its own connection (`shell:` ones to device d1),
    /// then `host:kill`.
    async fn drive(port_file: &Path, commands: &[&str]) {
        let mut port = String::new();
        for _ in 0..500 {
            port = std::fs::read_to_string(port_file).unwrap_or_default();
            if !port.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let port: u16 = port.parse().unwrap();
        for cmd in commands.iter().chain(&["host:kill"]) {
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            if cmd.starts_with("shell:") {
                s.write_all(b"0011host:transport:d1").await.unwrap();
                let mut okay = [0u8; 4];
                s.read_exact(&mut okay).await.unwrap();
            }
            s.write_all(format!("{:04x}{cmd}", cmd.len()).as_bytes()).await.unwrap();
            let mut out = Vec::new();
            let _ = s.read_to_end(&mut out).await;
        }
    }

    async fn run(yaml: &str, commands: &[&str]) -> Result<(), String> {
        let dir = tempfile::tempdir().unwrap();
        let port_file = dir.path().join("port");
        let transcript = dir.path().join("t.jsonl");
        let script = Script::from_yaml(yaml).unwrap().compile(dir.path()).unwrap();
        let options = ServerOptions {
            port: 0,
            transcript: Some(transcript.clone()),
            ..ServerOptions::default()
        };
        let never = std::future::pending::<std::io::Result<()>>();
        let both = async {
            tokio::join!(
                serve(script, options, Some(&port_file), never),
                drive(&port_file, commands)
            )
        };
        let (r, ()) = tokio::time::timeout(std::time::Duration::from_secs(30), both)
            .await
            .expect("timed out");
        // serve closed the transcript before returning.
        assert!(std::fs::read_to_string(&transcript).unwrap().contains("server_stopped"));
        r
    }

    #[tokio::test]
    async fn strict_mode_fails_on_warnings() {
        let devices = "devices: [{ serial: d1, shell: [ { exact: ok } ] }]";
        assert_eq!(run(devices, &["shell:ok", "shell:typo"]).await, Ok(()));
        let strict = format!("server: {{ strict: true }}\n{devices}");
        assert_eq!(run(&strict, &["shell:ok"]).await, Ok(()));
        let e = run(&strict, &["shell:ok", "shell:typo"]).await.unwrap_err();
        assert!(e.contains("1 warning(s)"), "{e}");
    }
}
