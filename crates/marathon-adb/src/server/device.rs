//! Device services (`SERVICES.TXT`, "local services"): shell v1 and v2, exec, abb_exec, framebuffer and
//! reverse forwarding. Sync lives in `sync.rs`.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::task::JoinHandle;

use super::protocol::{protocol_string, shell_v2, shell_v2_packet, write_fail, write_okay};
use super::script::{Response, ServiceKind, Step};
use super::world::{Transport, World};

/// How long a finished session waits for the client to close its side. Closing a socket with unread
/// input makes the kernel send RST, and the client may then lose the tail of our output.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// The suffix adam's v1 `ShellCommandRequest` appends to get the exit code: `<cmd>;echo x$?`.
const EXIT_CODE_TRICK: &str = ";echo x$?";

/// Shell protocol v2 data packets are split at this size, like adbd's buffer.
const SHELL_V2_CHUNK: usize = 32 * 1024;

pub(crate) async fn handle(
    world: Arc<World>,
    mut t: Transport,
    req: String,
    rd: OwnedReadHalf,
    wr: OwnedWriteHalf,
    conn: u64,
) -> io::Result<()> {
    let serial = t.serial.clone();
    tokio::select! {
        r = serve(&world, &serial, &req, rd, wr, conn) => r,
        // The device disconnected or went offline: drop the connection, as adb does.
        _ = t.life.changed() => {
            world.transcript.record(json!({"type": "event", "event": "session_dropped", "conn": conn, "serial": serial}));
            Ok(())
        }
    }
}

async fn serve(
    world: &Arc<World>,
    serial: &str,
    req: &str,
    mut rd: OwnedReadHalf,
    mut wr: OwnedWriteHalf,
    conn: u64,
) -> io::Result<()> {
    if let Some(rest) = req.strip_prefix("shell") {
        let (args, cmd) = match rest.strip_prefix(':') {
            Some(cmd) => ("", cmd),
            None => match rest.strip_prefix(',').and_then(|r| r.split_once(':')) {
                Some((args, cmd)) => (args, cmd),
                None => return unknown(world, req, &mut wr).await,
            },
        };
        let v2 = args.split(',').any(|a| a == "v2");
        let (cmd, trick) = match cmd.strip_suffix(EXIT_CODE_TRICK) {
            Some(c) if !v2 => (c, true),
            _ => (cmd, false),
        };
        let cmd = Command {
            kind: ServiceKind::Shell,
            text: cmd.to_string(),
            v2,
            exit_code_trick: trick,
        };
        return run(world, serial, conn, cmd, rd, wr).await;
    }
    if let Some(cmd) = req.strip_prefix("exec:") {
        let cmd = Command {
            kind: ServiceKind::Exec,
            text: cmd.to_string(),
            v2: false,
            exit_code_trick: false,
        };
        return run(world, serial, conn, cmd, rd, wr).await;
    }
    if let Some(args) = req.strip_prefix("abb_exec:") {
        // abb_exec separates arguments with NUL; expectations see them joined by spaces.
        let text = args.split('\0').collect::<Vec<_>>().join(" ");
        let cmd = Command {
            kind: ServiceKind::AbbExec,
            text,
            v2: false,
            exit_code_trick: false,
        };
        return run(world, serial, conn, cmd, rd, wr).await;
    }
    if req == "sync:" {
        return super::sync::serve(world, serial, conn, rd, wr).await;
    }
    if req == "framebuffer:" {
        let data = world.with_device(serial, |d| d.framebuffer.clone()).flatten();
        write_okay(&mut wr).await?;
        world.transcript.record(
            json!({"type": "framebuffer", "conn": conn, "serial": serial, "size": data.as_ref().map(|d| d.len())}),
        );
        // Without a scripted framebuffer, adbd's behaviour when screencap fails: close without a header.
        if let Some(data) = data {
            let version = data
                .get(..4)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .map(u32::from_le_bytes);
            let header_size = match version {
                Some(1) => Some(52),
                Some(2) => Some(56),
                Some(16) => Some(16),
                _ => None,
            };
            let Some(header_size) = header_size.map(|h| h.min(data.len())) else {
                // adam rejects an unknown version without sending the nudge, so there is no ack to await.
                wr.write_all(&data).await?;
                return close(rd, wr).await;
            };
            wr.write_all(&data[..header_size]).await?;
            // adam writes its nudge only after the header. Closing here races that write.
            let ack = rd.read_u8().await?;
            world
                .transcript
                .record(json!({"type":"framebuffer_ack", "conn":conn, "serial":serial, "value":ack}));
            if ack != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "framebuffer acknowledgement is not zero",
                ));
            }
            wr.write_all(&data[header_size..]).await?;
        }
        return close(rd, wr).await;
    }
    if let Some(r) = req.strip_prefix("reverse:") {
        return reverse(world, serial, r, rd, wr).await;
    }
    unknown(world, req, &mut wr).await
}

async fn unknown(world: &World, req: &str, wr: &mut OwnedWriteHalf) -> io::Result<()> {
    world.transcript.warning(format!("unsupported device service {req:?}"));
    // adbd closes the stream on an unknown service, which the server reports as "closed".
    write_fail(wr, "closed").await
}

/// Shuts down our side, then waits (bounded) for the client to close its side.
pub(crate) async fn close(mut rd: OwnedReadHalf, mut wr: OwnedWriteHalf) -> io::Result<()> {
    wr.shutdown().await?;
    let _ = tokio::time::timeout(CLOSE_GRACE, async {
        let mut buf = [0u8; 4096];
        while let Ok(n) = rd.read(&mut buf).await {
            if n == 0 {
                break;
            }
        }
    })
    .await;
    Ok(())
}

struct Command {
    kind: ServiceKind,
    /// What expectations match against: the command, without adam's exit-code suffix.
    text: String,
    v2: bool,
    exit_code_trick: bool,
}

/// Splits on whitespace and NUL and drops quotes, enough to find `-S <size>`.
fn tokens(cmd: &str) -> Vec<String> {
    cmd.split(|c: char| c.is_whitespace() || c == '\0')
        .filter(|t| !t.is_empty())
        .map(|t| t.replace(['\'', '"'], ""))
        .collect()
}

/// The stdin payload size of a streamed install: `cmd package install -S <n>`, `pm install-write -S <n> …`,
/// and the abb_exec forms. `install-create -S` gives a total size but no payload, so it doesn't count.
fn payload_size(kind: ServiceKind, cmd: &str) -> Option<u64> {
    if kind == ServiceKind::Shell {
        return None;
    }
    let t = tokens(cmd);
    let mut it = t.iter().map(String::as_str).peekable();
    match it.next()? {
        "cmd" => {
            if it.next()? != "package" {
                return None;
            }
        }
        "pm" | "package" => {}
        _ => return None,
    }
    if !matches!(it.next()?, "install" | "install-write") {
        return None;
    }
    while let Some(tok) = it.next() {
        if tok == "-S" {
            return it.peek()?.parse().ok();
        }
        if let Some(n) = tok.strip_prefix("-S") {
            return n.parse().ok();
        }
    }
    None
}

fn builtin(world: &World, serial: &str, cmd: &str) -> Option<Response> {
    let out = |s: String| Response {
        steps: vec![Step::Stdout(s.into_bytes())],
        exit_code: 0,
        hang: false,
    };
    let t: Vec<&str> = cmd.split_whitespace().collect();
    match t.as_slice() {
        ["getprop"] => world.with_device(serial, |d| {
            out(d.getprop.iter().map(|(k, v)| format!("[{k}]: [{v}]\n")).collect())
        }),
        ["getprop", key] => world.with_device(serial, |d| {
            out(format!("{}\n", d.getprop.get(*key).cloned().unwrap_or_default()))
        }),
        ["echo", ..] => Some(out(format!("{}\n", cmd.trim_start()[4..].trim_start()))),
        _ => None,
    }
}

/// Aborts the task when dropped, so that a session cancelled from outside (device gone, server killed)
/// doesn't leave its reader running with the socket's read half.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A use of an expectation taken by [`World::take_rule`], given back on drop unless kept.
struct Reservation<'a> {
    world: &'a World,
    serial: &'a str,
    id: Option<String>,
}

impl Reservation<'_> {
    fn keep(mut self) {
        self.id = None;
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if let Some(id) = &self.id {
            self.world.release_rule(self.serial, id);
        }
    }
}

async fn run(
    world: &Arc<World>,
    serial: &str,
    conn: u64,
    cmd: Command,
    mut rd: OwnedReadHalf,
    mut wr: OwnedWriteHalf,
) -> io::Result<()> {
    let record =
        |handler: &str, rule: Option<&str>, stdin: Option<u64>, extra: serde_json::Value, r: Option<&Response>| {
            let mut v = json!({
                "type": "shell",
                "conn": conn,
                "serial": serial,
                "service": cmd.kind.name(),
                "protocol": if cmd.v2 { "v2" } else { "raw" },
                "command": cmd.text,
                "exit_code_trick": cmd.exit_code_trick,
                "stdin_bytes": stdin,
                "handler": handler,
                "rule": rule,
                "exit_code": r.map(|r| r.exit_code),
                "hang": r.is_some_and(|r| r.hang),
            });
            if let (Some(m), serde_json::Value::Object(e)) = (v.as_object_mut(), extra) {
                m.extend(e);
            }
            world.transcript.record(v);
        };
    let builtin_response =
        builtin(world, serial, &cmd.text).filter(|_| world.builtins && cmd.kind == ServiceKind::Shell);
    // The expectation is reserved before OKAY, so that concurrent sessions can't both use one limited by
    // `times`, and so that strict mode can refuse the stream itself.
    let (handler, rule, response) = match world.take_rule(serial, cmd.kind, &cmd.text) {
        Some((id, r)) => ("script", Some(id), r),
        None => match builtin_response {
            Some(r) => ("builtin", None, r),
            None if world.strict => {
                world.transcript.warning(format!(
                    "{serial}: unmatched {} command {:?} (strict)",
                    cmd.kind.name(),
                    cmd.text
                ));
                record("rejected", None, None, json!({}), None);
                let msg = format!(
                    "fake adb: no expectation for {} command '{}'",
                    cmd.kind.name(),
                    cmd.text
                );
                write_fail(&mut wr, &msg).await?;
                return close(rd, wr).await;
            }
            None => {
                world.transcript.warning(format!(
                    "{serial}: unmatched {} command {:?}",
                    cmd.kind.name(),
                    cmd.text
                ));
                ("default", None, world.default.clone())
            }
        },
    };
    // Until the upload is complete, any way out of here (an error, a short upload, the session being
    // dropped on a disconnect or kill) gives the reserved use back.
    let reservation = Reservation {
        world,
        serial,
        id: rule.clone(),
    };
    write_okay(&mut wr).await?;
    let payload = payload_size(cmd.kind, &cmd.text);
    let mut received = None;
    let mut payload_digest = None;
    if let Some(n) = payload {
        let mut got_total = 0u64;
        let mut digest = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        while got_total < n {
            let want = buf.len().min(usize::try_from(n - got_total).unwrap_or(usize::MAX));
            let got = match rd.read(&mut buf[..want]).await {
                Ok(got) => got,
                Err(e) => {
                    // A reset or other failure mid-upload is an unfinished exchange, not a client
                    // walking away from output: warn (failing strict mode), with the byte counts.
                    world.transcript.warning(format!(
                        "{serial}: stdin failed after {got_total} of {n} bytes for {:?}: {e}",
                        cmd.text
                    ));
                    let failure = Response {
                        steps: Vec::new(),
                        exit_code: 1,
                        hang: false,
                    };
                    let extra = json!({"stdin_expected": n, "error": e.to_string()});
                    record("short_payload", None, Some(got_total), extra, Some(&failure));
                    return Ok(());
                }
            };
            if got == 0 {
                break;
            }
            digest.update(&buf[..got]);
            got_total += got as u64;
        }
        received = Some(got_total);
        payload_digest = Some(format!("{:x}", digest.finalize()));
        if got_total < n {
            // The upload ended early: answer as pm does for a broken APK, and give the reserved
            // expectation back now, since the install it scripts never happened.
            drop(reservation);
            world.transcript.warning(format!(
                "{serial}: stdin ended after {got_total} of {n} bytes for {:?}",
                cmd.text
            ));
            let text = format!("Failure [INSTALL_FAILED_INVALID_APK: fake adb received {got_total} of {n} bytes]\n");
            let failure = Response {
                steps: Vec::new(),
                exit_code: 1,
                hang: false,
            };
            record(
                "short_payload",
                None,
                received,
                json!({"stdin_expected": n}),
                Some(&failure),
            );
            wr.write_all(text.as_bytes()).await?;
            return close(rd, wr).await;
        }
    }
    reservation.keep();
    record(
        handler,
        rule.as_deref(),
        received,
        json!({"stdin_sha256": payload_digest}),
        Some(&response),
    );
    // Whatever else the client sends (shell v2 stdin and close-stdin packets, raw stdin) is read and
    // dropped; the read side reaching EOF means the client has gone.
    let mut client_gone = AbortOnDrop(tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        while let Ok(n) = rd.read(&mut buf).await {
            if n == 0 {
                break;
            }
        }
    }));
    let output = async {
        for step in &response.steps {
            match step {
                Step::Stdout(b) => {
                    write_out(&mut wr, cmd.v2, shell_v2::STDOUT, b).await?;
                    capture_output(world, serial, conn, "stdout", b);
                }
                Step::Stderr(b) => {
                    write_out(&mut wr, cmd.v2, shell_v2::STDERR, b).await?;
                    capture_output(world, serial, conn, "stderr", b);
                }
                Step::Sleep(ms) => tokio::time::sleep(Duration::from_millis(*ms)).await,
                Step::Gate(name) => {
                    // The world holds the sender, so the wait ends only when the gate opens.
                    let _ = world.gates.subscribe().wait_for(|open| open.contains(name)).await;
                }
                Step::StdoutStream {
                    data,
                    chunk_size,
                    interval_ms,
                } => {
                    for (i, chunk) in data.chunks(*chunk_size).enumerate() {
                        if i > 0 && *interval_ms > 0 {
                            tokio::time::sleep(Duration::from_millis(*interval_ms)).await;
                        }
                        write_out(&mut wr, cmd.v2, shell_v2::STDOUT, chunk).await?;
                        capture_output(world, serial, conn, "stdout", chunk);
                    }
                }
            }
        }
        if response.hang {
            let _ = (&mut client_gone.0).await;
            return Ok(true);
        }
        if cmd.v2 {
            wr.write_all(&shell_v2_packet(shell_v2::EXIT, &[response.exit_code]))
                .await?;
        } else if cmd.exit_code_trick {
            wr.write_all(format!("x{}\n", response.exit_code).as_bytes()).await?;
        }
        wr.shutdown().await?;
        Ok(false)
    };
    // The command's output is the one place where the client may simply stop listening (marathon closes
    // logcat and abandoned instrumentation streams); such a hang-up is marked as a cancelled output.
    if output.await.map_err(output_cancelled)? {
        return Ok(());
    }
    let _ = tokio::time::timeout(CLOSE_GRACE, &mut client_gone.0).await;
    Ok(())
}

fn capture_output(world: &World, serial: &str, conn: u64, stream: &str, bytes: &[u8]) {
    if world.capture_output.load(std::sync::atomic::Ordering::Relaxed) {
        world
            .transcript
            .record(json!({"type":"output", "conn":conn, "serial":serial,
            "stream":stream, "bytes":bytes}));
    }
}

/// Marks an error as the client hanging up on an output stream; the server records `client_closed` for
/// these instead of a warning.
#[derive(Debug)]
pub(crate) struct OutputCancelled(io::Error);

impl std::fmt::Display for OutputCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "client closed the output stream: {}", self.0)
    }
}

impl std::error::Error for OutputCancelled {}

/// Wraps hang-up errors (broken pipe, reset, abort) in [`OutputCancelled`]; other errors pass unchanged.
pub(crate) fn output_cancelled(e: io::Error) -> io::Error {
    match e.kind() {
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted => {
            io::Error::new(e.kind(), OutputCancelled(e))
        }
        _ => e,
    }
}

pub(crate) fn is_output_cancelled(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<OutputCancelled>())
}

async fn write_out(wr: &mut OwnedWriteHalf, v2: bool, id: u8, data: &[u8]) -> io::Result<()> {
    if !v2 {
        // Without the shell protocol adbd sends stderr down the same stream as stdout.
        return wr.write_all(data).await;
    }
    for chunk in data.chunks(SHELL_V2_CHUNK) {
        wr.write_all(&shell_v2_packet(id, chunk)).await?;
    }
    Ok(())
}

/// `reverse:` services, answered as adbd's `handle_forward_request` does on the device side: OKAY for the
/// stream, then OKAY or FAIL for the result.
async fn reverse(world: &World, serial: &str, req: &str, rd: OwnedReadHalf, mut wr: OwnedWriteHalf) -> io::Result<()> {
    write_okay(&mut wr).await?;
    let mut out: Vec<u8> = Vec::new();
    let result: Result<(), String> = if req == "list-forward" {
        let text = world
            .with_device(serial, |d| {
                d.reverse
                    .iter()
                    .map(|(l, r)| format!("host-{} {l} {r}\n", d.transport_id))
                    .collect::<String>()
            })
            .unwrap_or_default();
        out = protocol_string(text.as_bytes());
        Ok(())
    } else if req == "killforward-all" {
        world.with_device(serial, |d| d.reverse.clear());
        out.extend(b"OKAY");
        Ok(())
    } else if let Some(local) = req.strip_prefix("killforward:") {
        world
            .with_device(serial, |d| match d.reverse.iter().position(|(l, _)| l == local) {
                Some(i) => {
                    d.reverse.remove(i);
                    Ok(())
                }
                None => Err(format!("listener '{local}' not found")),
            })
            .unwrap_or(Ok(()))
            .map(|_| out.extend(b"OKAY"))
    } else if let Some(spec) = req.strip_prefix("forward:") {
        let (norebind, spec) = match spec.strip_prefix("norebind:") {
            Some(s) => (true, s),
            None => (false, spec),
        };
        match spec.split_once(';') {
            None => Err(format!("malformed forward spec '{spec}'")),
            Some((local, remote)) => {
                let (local, port) = if local == "tcp:0" {
                    let p = world.allocate_reverse_port();
                    (format!("tcp:{p}"), Some(p))
                } else {
                    (local.to_string(), None)
                };
                world
                    .with_device(serial, |d| match d.reverse.iter().position(|(l, _)| *l == local) {
                        Some(_) if norebind => Err("cannot rebind existing socket".to_string()),
                        Some(i) => {
                            d.reverse[i].1 = remote.to_string();
                            Ok(())
                        }
                        None => {
                            d.reverse.push((local.clone(), remote.to_string()));
                            Ok(())
                        }
                    })
                    .unwrap_or(Ok(()))
                    .map(|_| {
                        out.extend(b"OKAY");
                        if let Some(p) = port {
                            out.extend(protocol_string(p.to_string().as_bytes()));
                        }
                    })
            }
        }
    } else {
        Err(format!("unknown reverse service '{req}'"))
    };
    match result {
        Ok(()) => wr.write_all(&out).await?,
        Err(m) => write_fail(&mut wr, &m).await?,
    }
    close(rd, wr).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_payload_sizes() {
        let cases = [
            (ServiceKind::Exec, "cmd package install -S 1234", Some(1234)),
            (ServiceKind::Exec, "cmd package install -r -S 7", Some(7)),
            (ServiceKind::Exec, "cmd package 'install' -S 9", Some(9)),
            (ServiceKind::AbbExec, "package install -S 5", Some(5)),
            (
                ServiceKind::Exec,
                "cmd package install-write -S 42 s1 base.apk -",
                Some(42),
            ),
            (ServiceKind::Exec, "pm install-write -S 42 s1 base.apk -", Some(42)),
            (ServiceKind::Exec, "cmd package install-create '-S100' -r", None),
            (ServiceKind::Exec, "cmd package install-commit s1", None),
            (ServiceKind::Shell, "cmd package install -S 3", None),
            (ServiceKind::Exec, "screencap -p", None),
        ];
        for (kind, cmd, want) in cases {
            assert_eq!(payload_size(kind, cmd), want, "{cmd}");
        }
    }
}
