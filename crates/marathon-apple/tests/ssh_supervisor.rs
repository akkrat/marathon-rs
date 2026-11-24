mod support;
#[path = "support/supervisor.rs"]
mod virtual_supervisor;

use base64::{Engine, engine::general_purpose::STANDARD};
use marathon_apple::cmd::{CommandExecutor, CommandRequest};
use marathon_logging::Level;
use std::{path::Path, sync::Arc};
use support::{context, duration, ssh, watchdog};

#[tokio::test(flavor = "multi_thread")]
async fn probes_each_connection_and_warns_once_for_the_worker_d96() {
    watchdog(async {
        let fake = ssh::fake(
            ssh::script(vec![ssh::exact(
                "echo fallback",
                ssh::output(vec![ssh::chunk("fallback\n")], 0),
            )]),
            4,
        )
        .await;
        let (logging, events) = ssh::logging(false);
        let mut factory = ssh::factory(&logging);
        factory.keep_alive_interval = duration(1000);
        factory.keep_alive_max_count = 1;
        let warned = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        for _ in 0..2 {
            let executor = factory
                .connect_worker("127.0.0.1", fake.port(), ssh::password(), None, false, warned.clone())
                .await
                .unwrap();
            for _ in 0..2 {
                let result = executor
                    .critical_execute(CommandRequest::new(["echo", "fallback"], duration(1000)), context())
                    .await
                    .unwrap();
                assert_eq!(result.stdout, ["fallback"]);
            }
            executor.close().await.unwrap();
        }
        let execs = fake.events("exec").await;
        let probes = execs
            .iter()
            .filter(|row| {
                STANDARD
                    .decode(row["detail"]["base64"].as_str().unwrap())
                    .unwrap()
                    .starts_with(b"/usr/bin/perl -MMIME::Base64 -e ")
            })
            .count();
        assert_eq!((probes, execs.len()), (2, 6));
        let warnings = ssh::lines(&events)
            .into_iter()
            .filter(|(target, level, message)| {
                target.ends_with("::supervisor") && *level == Level::WARN && message.contains("/usr/bin/perl")
            })
            .count();
        assert_eq!(warnings, 1);
        fake.worker.shutdown().await.unwrap();
    })
    .await;
}

struct ProbeReplies {
    first: Option<fake_worker::script::Action>,
    fallback: fake_worker::script::Action,
    attempts: std::sync::atomic::AtomicUsize,
}
impl fake_worker::router::Handler for ProbeReplies {
    fn name(&self) -> &str {
        "probe-fixture"
    }
    fn route<'a>(
        &'a self,
        request: &'a fake_worker::router::Request,
        _: &'a fake_worker::fs::VirtualFs,
    ) -> fake_worker::router::HandlerFuture<'a> {
        Box::pin(async move {
            if !request.text.starts_with("/usr/bin/perl -MMIME::Base64 -e ") {
                return Ok(None);
            }
            let attempt = self.attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Some(if attempt == 0 {
                self.first.as_ref().unwrap_or(&self.fallback).clone()
            } else {
                self.fallback.clone()
            }))
        })
    }
}

#[tokio::test]
async fn a_probe_transport_error_or_malformed_success_is_retried_d96() {
    use fake_worker::script::Action;
    for first in [Action::Reject, Action::output("invalid success\n", 0)] {
        probe_fallback(Some(first), "fixture rejected", 127).await;
    }
}

#[tokio::test]
async fn overlapping_commands_share_one_failed_worker_probe_d96() {
    use fake_worker::script::{Action, Chunk};
    const RELEASE: &str = "/tmp/marathon/probe-fixture-ready";
    let replies = Arc::new(ProbeReplies {
        first: Some(ssh::output(
            vec![Chunk {
                data: "invalid success\n".into(),
                wait_for_file: Some(RELEASE.into()),
                ..Default::default()
            }],
            0,
        )),
        fallback: Action::output("", 127),
        attempts: Default::default(),
    });
    let fake = ssh::fake_with_handlers(ssh::script(vec![]), 3, |_| {}, vec![replies.clone()]).await;
    let (logger, _) = ssh::logging(false);
    let executor = Arc::new(
        ssh::factory(&logger)
            .connect_worker("127.0.0.1", fake.port(), ssh::password(), None, false, Arc::default())
            .await
            .unwrap(),
    );
    let pending: Vec<_> = (0..8)
        .map(|_| {
            let executor = executor.clone();
            tokio::spawn(async move { executor.worker_supervised().await })
        })
        .collect();
    assert_eq!(fake.events_at_least("exec", 1).await.len(), 1);
    fake.worker.fs.write(RELEASE, vec![], 0o600).await.unwrap();
    support::bounded(10, async {
        for pending in pending {
            let error = pending.await.unwrap().unwrap_err();
            assert!(
                error
                    .failure()
                    .to_string()
                    .contains("invalid supervisor bootstrap response")
            );
        }
    })
    .await;
    assert_eq!(replies.attempts.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(!executor.worker_supervised().await.unwrap());
    assert_eq!(replies.attempts.load(std::sync::atomic::Ordering::Relaxed), 2);
    executor.close().await.unwrap();
    fake.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_worker_bootstraps_fall_back_d96() {
    for (diagnostic, status) in [
        ("command filtered", 1),
        ("failed to load a Perl module", 255),
        ("cannot create a temporary directory", 255),
    ] {
        let wire = probe_fallback(None, diagnostic, status).await;
        if Path::new("/bin/csh").exists() && Path::new("/usr/bin/perl").exists() {
            assert!(!wire.contains('\n'));
            let output = tokio::process::Command::new("/bin/csh")
                .args(["-f", "-c", wire.as_str()])
                .output()
                .await
                .unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            let result = String::from_utf8(output.stdout).unwrap();
            let root = result
                .trim()
                .strip_prefix("MARATHON_D96_1 /tmp/marathon-supervisor-")
                .unwrap();
            assert!(root.len() == 10 && root.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)));
            std::fs::remove_dir_all(format!("/tmp/marathon-supervisor-{root}")).unwrap();
        }
    }
}

async fn probe_fallback(first: Option<fake_worker::script::Action>, detail: &str, status: u32) -> String {
    use fake_worker::script::Action;
    let failed_first = first.is_some();
    let replies = Arc::new(ProbeReplies {
        first,
        fallback: ssh::output(vec![ssh::err_chunk(detail)], status),
        attempts: Default::default(),
    });
    let fake = ssh::fake_with_handlers(
        ssh::script(vec![ssh::exact("echo fallback", Action::output("fallback\n", 0))]),
        3,
        |_| {},
        vec![replies.clone()],
    )
    .await;
    let (logger, events) = ssh::logging(false);
    let executor = ssh::factory(&logger)
        .connect_worker("127.0.0.1", fake.port(), ssh::password(), None, false, Arc::default())
        .await
        .unwrap();
    let command = CommandRequest::new(["echo", "fallback"], duration(1000));
    if failed_first {
        assert!(executor.critical_execute(command.clone(), context()).await.is_err());
    }
    for _ in 0..2 {
        assert_eq!(
            executor
                .critical_execute(command.clone(), context())
                .await
                .unwrap()
                .stdout,
            ["fallback"]
        );
    }
    assert_eq!(
        replies.attempts.load(std::sync::atomic::Ordering::Relaxed),
        1 + usize::from(failed_first)
    );
    assert_eq!(
        ssh::lines(&events)
            .iter()
            .filter(|(target, level, _)| target.ends_with("::supervisor") && *level == Level::WARN)
            .count(),
        1
    );
    let probes = fake.events("exec").await;
    let install = STANDARD
        .decode(probes[0]["detail"]["base64"].as_str().unwrap())
        .unwrap();
    executor.close().await.unwrap();
    fake.worker.shutdown().await.unwrap();
    String::from_utf8(install).unwrap()
}

#[test]
fn cancellation_before_launch_prevents_worker_payload_d96() -> Result<(), Box<dyn std::error::Error>> {
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    let root = tempfile::Builder::new()
        .prefix("marathon-supervisor-")
        .rand_bytes(10)
        .tempdir_in("/tmp")?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let program = root.path().join("supervisor.pl");
    std::fs::write(
        &program,
        include_bytes!("../src/cmd/remote/ssh/sshj/supervisor/worker.pl"),
    )?;
    let token = "0123456789abcdef0123456789abcdef";
    let control = std::process::Command::new("/usr/bin/perl")
        .arg(&program)
        .arg("control")
        .arg(root.path())
        .args(["6000", &format!("{token}:3")])
        .output()?;
    assert!(control.status.success(), "{}", String::from_utf8_lossy(&control.stderr));
    assert_eq!(String::from_utf8(control.stdout)?, format!("{token} not-started\n"));
    let effect = root.path().join("payload-ran");
    let status = std::process::Command::new("/usr/bin/perl")
        .arg(program)
        .arg("run")
        .arg(root.path())
        .args(["6000", token, "/usr/bin/touch"])
        .arg(&effect)
        .status()?;
    assert_eq!(status.code(), Some(125));
    assert!(!effect.exists());
    Ok(())
}

/// A worker `control-stream` as the lease watcher drives it.
struct ControlStream {
    stdin: tokio::process::ChildStdin,
    replies: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    next: u64,
}

impl ControlStream {
    /// Sends `token:operation` with a status request for `sibling` behind it, and returns the request's id and the
    /// replies that came before the sibling's, which must be `unknown`. The request is sent again under a new id while
    /// the worker answers `busy` (its token lock was held for that instant; the request was not applied), as the lease
    /// watcher retries it. Hang guard only.
    async fn request(&mut self, token: &str, operation: &str, sibling: &str) -> std::io::Result<(u64, Vec<String>)> {
        use tokio::io::AsyncWriteExt;
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            let mut before = Vec::new();
            loop {
                let (id, probe) = (self.next, self.next + 1);
                self.next += 2;
                self.stdin
                    .write_all(format!("{id} {token}:{operation}\n{probe} {sibling}:status\n").as_bytes())
                    .await?;
                loop {
                    let line = self
                        .replies
                        .next_line()
                        .await?
                        .ok_or_else(|| std::io::Error::other(format!("control stream ended after {before:?}")))?;
                    if line.starts_with(&format!("{probe} ")) {
                        assert_eq!(line, format!("{probe} {sibling} unknown"));
                        break;
                    }
                    before.push(line);
                }
                let busy = format!("{id} {token} busy");
                if !before.contains(&busy) {
                    return Ok((id, before));
                }
                before.retain(|line| *line != busy);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(std::io::Error::other)?
    }
}

#[tokio::test]
async fn stock_perl_detaches_the_recorder_and_returns_its_own_exit_proof_d96() -> Result<(), Box<dyn std::error::Error>>
{
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    use std::{
        os::unix::fs::PermissionsExt,
        process::Stdio,
        time::{Duration, Instant},
    };
    use tokio::io::{AsyncBufReadExt, BufReader};
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    let root = tempfile::Builder::new()
        .prefix("marathon-supervisor-")
        .rand_bytes(10)
        .tempdir_in("/tmp")?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let supervisor = root.path().join("supervisor.pl");
    std::fs::write(
        &supervisor,
        include_bytes!("../src/cmd/remote/ssh/sshj/supervisor/worker.pl"),
    )?;
    let marker = root.path().join("payload.pid");
    let release = root.path().join("release");
    let token = "0123456789abcdef0123456789abcdef";
    let command = |mode: &str| {
        let mut command = tokio::process::Command::new("/usr/bin/perl");
        command.arg(&supervisor).arg(mode).arg(root.path()).arg("60000");
        command.kill_on_drop(true);
        command
    };
    let start = Instant::now();
    let launcher = tokio::time::timeout(
        Duration::from_secs(60),
        command("detach")
            .args([
                token,
                "/usr/bin/perl",
                "-e",
                r#"my $interrupted; $SIG{INT} = sub { $interrupted = 1 }; print STDERR "Recording started\n";
open my $pid, '>', shift or die $!; print $pid "$$\n"; close $pid; my $release = shift;
while (!$interrupted || !-f $release) { select undef, undef, undef, .01 }"#,
            ])
            .arg(&marker)
            .arg(&release)
            .output(),
    )
    .await??;
    assert!(
        launcher.status.success(),
        "{}",
        String::from_utf8_lossy(&launcher.stderr)
    );
    let detached = start.elapsed();
    let pid = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(text) = tokio::fs::read_to_string(&marker).await
                && let Ok(pid) = text.trim().parse::<i32>()
            {
                break Pid::from_raw(pid);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert_eq!(kill(pid, None), Ok(()));
    // D98: the worker holds INT until the recorder's start marker, which status then reports.
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let status = command("control").arg(format!("{token}:status")).output().await?;
            if status.stdout.starts_with(format!("{token} recording:").as_bytes()) {
                return Ok::<_, std::io::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    let mut control = command("control-stream")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stream = ControlStream {
        stdin: control.stdin.take().unwrap(),
        replies: BufReader::new(control.stdout.take().unwrap()).lines(),
        next: 1,
    };
    let sibling = "00000000000000000000000000000000";
    let (first, early) = stream.request(token, "1", sibling).await?;
    assert!(
        early.is_empty(),
        "a recorder's INT is answered with its receipt: {early:?}"
    );
    let (retry, superseded) = stream.request(token, "1", sibling).await?;
    std::fs::write(&release, b"")?;
    let receipt = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(line) = stream.replies.next_line().await? {
            if line.starts_with(&format!("{retry} ")) {
                return Ok::<_, std::io::Error>(Some(line));
            }
        }
        Ok(None)
    })
    .await??;
    assert_eq!(receipt, Some(format!("{retry} {token} done:0")));
    assert!(
        matches!(&superseded[..], [line] if line.starts_with(&format!("{first} {token} recording:"))),
        "the retry must reply to the superseded INT while the recorder is still running: {superseded:?}"
    );

    let foreground_token = "1234567890abcdef0123456789abcdef";
    let mut foreground = command("run")
        .args([
            foreground_token,
            "/usr/bin/perl",
            "-e",
            r#"$SIG{INT} = 'IGNORE'; $SIG{TERM} = sub { exit 0 }; $| = 1; print "ready\n";
while (1) { select undef, undef, undef, .01 }"#,
        ])
        .stdout(Stdio::piped())
        .spawn()?;
    let mut foreground_stdout = BufReader::new(foreground.stdout.take().unwrap()).lines();
    let ready = tokio::time::timeout(Duration::from_secs(60), foreground_stdout.next_line()).await??;
    assert_eq!(ready.as_deref(), Some("ready"));
    let (interrupt, replies) = stream.request(foreground_token, "1", sibling).await?;
    assert!(
        matches!(&replies[..], [line] if line.starts_with(&format!("{interrupt} {foreground_token} running:"))),
        "{replies:?}"
    );
    stream.request(foreground_token, "2", sibling).await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(60), foreground.wait())
            .await??
            .success()
    );

    let ControlStream { stdin, .. } = stream;
    drop(stdin);
    assert!(control.wait().await?.success());
    println!(
        "worker detached in {detached:?} and gave INT proof by {:?}",
        start.elapsed()
    );
    assert_eq!(kill(pid, None), Err(Errno::ESRCH));
    Ok(())
}

#[tokio::test]
async fn forced_and_expired_recorders_finalize_and_failed_recorders_keep_bounded_stderr_d96()
-> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    let (root, supervisor) = d98_root()?;
    let files = tempfile::tempdir()?;
    // The forced recorder's lease outlasts every hang guard, so only the force can end it in time; every control
    // request goes through `d98_control`, because a single `busy` answer would drop the force.
    for (token, lease, force) in [
        ("00000000000000000000000000000001", "3000", false),
        ("00000000000000000000000000000002", "120000", true),
    ] {
        let ready = files.path().join(format!("{token}.ready"));
        let payload = r#"
    $SIG{INT} = sub { exit(0) };
    print STDERR "Recording started\n";
    open my $marker, '>', shift or die $!;
    close $marker;
    while (1) { select undef, undef, undef, .01 }
    "#;
        let start = d98_invoke_with(
            &supervisor,
            root.path(),
            lease,
            "detach",
            &[token, "/usr/bin/perl", "-e", payload, ready.to_str().unwrap()],
        )
        .await?;
        assert!(start.status.success(), "{}", String::from_utf8_lossy(&start.stderr));
        d98_file(&ready).await?;
        let requested = Instant::now();
        if force {
            let reply = d98_control(&supervisor, root.path(), &format!("{token}:3")).await?;
            assert!(
                [" running:", " recording:"]
                    .iter()
                    .any(|state| reply.starts_with(&format!("{token}{state}"))),
                "{reply}"
            );
        }
        let (receipt, ended) = d98_receipt(&supervisor, root.path(), token).await?;
        assert_eq!(receipt, "done:0", "{token}");
        // Hang guard, far below the forced recorder's lease.
        assert!(ended - requested < Duration::from_secs(60), "{token}");
    }
    let failed = "00000000000000000000000000000003";
    let start = d98_invoke_with(
        &supervisor,
        root.path(),
        "8000",
        "detach",
        &[
            failed,
            "/usr/bin/perl",
            "-e",
            r#"print STDERR "A recording is already in progress\n", "x" x 50000; exit 16"#,
        ],
    )
    .await?;
    assert!(start.status.success());
    let (receipt, _) = d98_receipt(&supervisor, root.path(), failed).await?;
    use base64::{Engine, engine::general_purpose::STANDARD};
    let encoded = receipt.strip_prefix("done:16 ").unwrap_or_else(|| panic!("{receipt}"));
    let stderr = STANDARD.decode(encoded)?;
    assert_eq!(stderr.len(), 8192);
    assert!(stderr.starts_with(b"A recording is already in progress\n"));
    assert_eq!(d98_retire(&supervisor, root.path()).await?, "MARATHON_D96_1 retired");
    Ok(())
}

/// A private worker root with the supervisor installed, as the bootstrap leaves it.
fn d98_root() -> std::io::Result<(tempfile::TempDir, std::path::PathBuf)> {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::Builder::new()
        .prefix("marathon-supervisor-")
        .rand_bytes(10)
        .tempdir_in("/tmp")?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let supervisor = root.path().join("supervisor.pl");
    std::fs::write(
        &supervisor,
        include_bytes!("../src/cmd/remote/ssh/sshj/supervisor/worker.pl"),
    )?;
    Ok((root, supervisor))
}

async fn d98_invoke(
    supervisor: &Path,
    root: &Path,
    mode: &str,
    args: &[&str],
) -> std::io::Result<std::process::Output> {
    d98_invoke_with(supervisor, root, "60000", mode, args).await
}

async fn d98_invoke_with(
    supervisor: &Path,
    root: &Path,
    lease: &str,
    mode: &str,
    args: &[&str],
) -> std::io::Result<std::process::Output> {
    let mut command = tokio::process::Command::new("/usr/bin/perl");
    command.arg(supervisor).arg(mode).arg(root).arg(lease).args(args);
    command.kill_on_drop(true);
    command.output().await
}

/// One control request, repeated while the worker answers `busy` (its token lock was held for that instant; the
/// request was not applied). Hang guard only.
async fn d98_control(supervisor: &Path, root: &Path, item: &str) -> std::io::Result<String> {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let output = d98_invoke(supervisor, root, "control", &[item]).await?;
            if !output.status.success() {
                return Err(std::io::Error::other(
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ));
            }
            let reply = String::from_utf8_lossy(&output.stdout).trim_end().to_owned();
            if !reply.ends_with(" busy") {
                return Ok(reply);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Retires the worker root, repeated while it answers `busy`: a detached supervisor that has published its receipt
/// still holds the installation lock until it exits. Hang guard only.
async fn d98_retire(supervisor: &Path, root: &Path) -> std::io::Result<String> {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let output = d98_invoke(supervisor, root, "retire", &[]).await?;
            let reply = String::from_utf8_lossy(&output.stdout).trim_end().to_owned();
            if reply != "MARATHON_D96_1 busy" {
                return Ok(reply);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Polls `token`'s status until it is a receipt; returns the receipt and when it was first seen.
async fn d98_receipt(supervisor: &Path, root: &Path, token: &str) -> std::io::Result<(String, std::time::Instant)> {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let status = d98_invoke(supervisor, root, "control", &[&format!("{token}:status")]).await?;
            let reply = String::from_utf8_lossy(&status.stdout).trim_end().to_owned();
            if let Some(receipt) = reply
                .strip_prefix(&format!("{token} "))
                .filter(|r| r.starts_with("done:"))
            {
                return Ok((receipt.to_owned(), std::time::Instant::now()));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(std::io::Error::other)?
}

#[tokio::test]
async fn a_detached_recorder_gets_int_only_after_its_own_start_marker_d98() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    let (root, supervisor) = d98_root()?;
    let token = "0123456789abcdef0123456789abcdef";
    // The worker root admits only token state, so the payload's files live beside it.
    let files = tempfile::tempdir()?;
    let (ready, release, early, times) = (
        files.path().join("ready"),
        files.path().join("release"),
        files.path().join("early"),
        files.path().join("times"),
    );
    // A slow simctl: a display note and lookalike lines first, then the marker once released. An INT before the
    // marker ends it with status 2, as it ends simctl.
    let payload = r#"use Time::HiRes qw(time); my ($ready, $release, $early, $times) = @ARGV; my $marked;
$SIG{INT} = sub {
    if (!$marked) { open my $f, '>', $early; close $f; exit 2 }
    open my $f, '>>', $times; print $f time, "\n"; close $f; exit 0;
};
print STDERR "Note: No display specified.\n", "Recording started soon\n", " Recording started\n", "xRecording started\n";
open my $f, '>', $ready or die $!; close $f;
select undef, undef, undef, .01 until -f $release;
$marked = time; open my $t, '>', $times; print $t $marked, "\n"; close $t;
print STDERR "Recording started\n";
while (1) { select undef, undef, undef, .01 }"#;
    let paths = [&ready, &release, &early, &times].map(|path| path.to_str().unwrap().to_owned());
    let mut args = vec![token, "/usr/bin/perl", "-e", payload];
    args.extend(paths.iter().map(String::as_str));
    let launched = d98_invoke(&supervisor, root.path(), "detach", &args).await?;
    assert!(
        launched.status.success(),
        "{}",
        String::from_utf8_lossy(&launched.stderr)
    );
    tokio::time::timeout(Duration::from_secs(60), async {
        while !ready.is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let interrupt = d98_control(&supervisor, root.path(), &format!("{token}:1")).await?;
    assert!(interrupt.starts_with(&format!("{token} running:")), "{interrupt}");
    // The old worker sent INT within one 50 ms loop. A slow simulator can take seconds to print the marker.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!early.exists(), "INT reached the recorder before its start marker");
    let status = d98_control(&supervisor, root.path(), &format!("{token}:status")).await?;
    assert!(
        status.starts_with(&format!("{token} running:")),
        "a lookalike line confirmed the start: {status}"
    );
    std::fs::write(&release, b"")?;
    let (receipt, _) = d98_receipt(&supervisor, root.path(), token).await?;
    assert_eq!(receipt, "done:0");
    assert!(!early.exists());
    let times: Vec<f64> = std::fs::read_to_string(&times)?
        .lines()
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    let [marked, interrupted] = times[..] else {
        panic!("marker and INT times: {times:?}");
    };
    // Hang guard only: the worker reads stderr as soon as it arrives.
    assert!(interrupted >= marked && interrupted - marked < 5.0, "{times:?}");
    println!("INT {:.0} ms after the start marker", (interrupted - marked) * 1000.0);
    assert_eq!(d98_retire(&supervisor, root.path()).await?, "MARATHON_D96_1 retired");
    Ok(())
}

/// Waits until `path` exists (hang guard only).
async fn d98_file(path: &Path) -> std::io::Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while !path.is_file() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(std::io::Error::other)
}

#[tokio::test]
async fn a_start_marker_split_across_writes_counts_only_when_its_line_is_complete_d98()
-> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    let (root, supervisor) = d98_root()?;
    let files = tempfile::tempdir()?;
    let token = "0123456789abcdef0123456789abcdef";
    // Three separate writes: "Recording", " started" and the newline, each after its own release. An INT before the
    // whole line ends the recorder with status 2.
    let payload = r#"select STDERR; $| = 1; my ($dir) = @ARGV; my $complete;
sub wait_for { my $f = "$dir/$_[0]"; select undef, undef, undef, .01 until -f $f }
sub touch { open my $f, '>', "$dir/$_[0]"; close $f }
$SIG{INT} = sub { if (!$complete) { touch('early'); exit 2 } exit 0 };
print STDERR "Recording"; touch('first');
wait_for('release1'); print STDERR " started"; touch('second');
wait_for('release2'); $complete = 1; print STDERR "\n";
while (1) { select undef, undef, undef, .01 }"#;
    let dir = files.path().to_str().unwrap().to_owned();
    let launched = d98_invoke(
        &supervisor,
        root.path(),
        "detach",
        &[token, "/usr/bin/perl", "-e", payload, &dir],
    )
    .await?;
    assert!(
        launched.status.success(),
        "{}",
        String::from_utf8_lossy(&launched.stderr)
    );
    d98_file(&files.path().join("first")).await?;
    let interrupt = d98_control(&supervisor, root.path(), &format!("{token}:1")).await?;
    assert!(interrupt.starts_with(&format!("{token} running:")), "{interrupt}");
    // Each part sits in the worker's pipe for a second; the worker reads it as soon as it arrives.
    tokio::time::sleep(Duration::from_secs(1)).await;
    std::fs::write(files.path().join("release1"), b"")?;
    d98_file(&files.path().join("second")).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !files.path().join("early").exists(),
        "INT before the marker line was complete"
    );
    let status = d98_control(&supervisor, root.path(), &format!("{token}:status")).await?;
    assert!(status.starts_with(&format!("{token} running:")), "{status}");
    std::fs::write(files.path().join("release2"), b"")?;
    let (receipt, _) = d98_receipt(&supervisor, root.path(), token).await?;
    assert_eq!(receipt, "done:0");
    assert!(!files.path().join("early").exists());
    assert_eq!(d98_retire(&supervisor, root.path()).await?, "MARATHON_D96_1 retired");
    Ok(())
}

#[tokio::test]
async fn lease_expiry_gives_an_unconfirmed_recorder_term_at_the_existing_time_d98()
-> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    let (root, supervisor) = d98_root()?;
    let files = tempfile::tempdir()?;
    let token = "00000000000000000000000000000003";
    let (ready, int) = (files.path().join("ready"), files.path().join("int"));
    let payload = r#"my ($ready, $int) = @ARGV; $SIG{INT} = sub { open my $f, '>', $int; close $f };
$SIG{TERM} = sub { exit 15 }; print STDERR "Note: No display specified.\n"; open my $f, '>', $ready; close $f;
while (1) { select undef, undef, undef, .01 }"#;
    let launched_at = Instant::now();
    // A 1.5 s lease that nobody renews: the controller is gone.
    let launched = d98_invoke_with(
        &supervisor,
        root.path(),
        "1500",
        "detach",
        &[
            token,
            "/usr/bin/perl",
            "-e",
            payload,
            ready.to_str().unwrap(),
            int.to_str().unwrap(),
        ],
    )
    .await?;
    assert!(
        launched.status.success(),
        "{}",
        String::from_utf8_lossy(&launched.stderr)
    );
    d98_file(&ready).await?;
    let (receipt, ended) = d98_receipt(&supervisor, root.path(), token).await?;
    assert_eq!(receipt.split(' ').next(), Some("done:15"));
    assert!(!int.exists(), "INT reached an unconfirmed recorder at lease expiry");
    // Expiry, then TERM 2 s later as before D98; the upper bound is a hang guard.
    let elapsed = ended - launched_at;
    assert!(
        elapsed >= Duration::from_millis(3500) && elapsed < Duration::from_secs(25),
        "{elapsed:?}"
    );
    println!("expired recorder: {receipt} after {elapsed:?}");
    assert_eq!(d98_retire(&supervisor, root.path()).await?, "MARATHON_D96_1 retired");
    Ok(())
}

#[tokio::test]
async fn an_unconfirmed_recorder_gets_term_and_kill_at_the_existing_cutoffs_d98()
-> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    let (root, supervisor) = d98_root()?;
    let files = tempfile::tempdir()?;
    // TERM request: TERM 2 s after it, never INT. KILL request: TERM at 2 s, KILL at 3 s, as before D98.
    for (token, operation, payload, expected, after) in [
        (
            "00000000000000000000000000000001",
            "2",
            r#"my ($ready, $int) = @ARGV; $SIG{INT} = sub { open my $f, '>', $int; close $f };
$SIG{TERM} = sub { exit 15 }; print STDERR "Note: No display specified.\n"; open my $f, '>', $ready; close $f;
while (1) { select undef, undef, undef, .01 }"#,
            "done:15",
            Duration::from_secs(2),
        ),
        (
            "00000000000000000000000000000002",
            "3",
            r#"my ($ready, $int) = @ARGV; $SIG{INT} = sub { open my $f, '>', $int; close $f };
$SIG{TERM} = 'IGNORE'; print STDERR "Note: No display specified.\n"; open my $f, '>', $ready; close $f;
while (1) { select undef, undef, undef, .01 }"#,
            "done:137",
            Duration::from_secs(3),
        ),
    ] {
        let ready = files.path().join(format!("{token}.ready"));
        let int = files.path().join(format!("{token}.int"));
        let (ready_arg, int_arg) = (ready.to_str().unwrap().to_owned(), int.to_str().unwrap().to_owned());
        let launched = d98_invoke(
            &supervisor,
            root.path(),
            "detach",
            &[token, "/usr/bin/perl", "-e", payload, &ready_arg, &int_arg],
        )
        .await?;
        assert!(
            launched.status.success(),
            "{}",
            String::from_utf8_lossy(&launched.stderr)
        );
        tokio::time::timeout(Duration::from_secs(60), async {
            while !ready.is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        let requested = Instant::now();
        let stop = d98_control(&supervisor, root.path(), &format!("{token}:{operation}")).await?;
        assert!(stop.starts_with(&format!("{token} running:")), "{stop}");
        let (receipt, ended) = d98_receipt(&supervisor, root.path(), token).await?;
        // A failed recorder's receipt carries its stderr.
        assert_eq!(receipt.split(' ').next(), Some(expected), "{token}");
        assert!(!int.exists(), "INT reached an unconfirmed recorder");
        // The escalation cutoffs are lower bounds; the upper bound is a hang guard.
        let elapsed = ended - requested;
        assert!(
            elapsed >= after && elapsed < after + Duration::from_secs(20),
            "{elapsed:?}"
        );
        println!("{token} {operation}: {receipt} after {elapsed:?}");
    }
    assert_eq!(d98_retire(&supervisor, root.path()).await?, "MARATHON_D96_1 retired");
    Ok(())
}

#[tokio::test]
async fn worker_expiry_compacts_its_receipt_and_retirement_waits_for_owned_groups_d96()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{os::unix::fs::PermissionsExt, time::Duration};
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    let root = tempfile::Builder::new()
        .prefix("marathon-supervisor-")
        .rand_bytes(10)
        .tempdir_in("/tmp")?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let supervisor = root.path().join("supervisor.pl");
    std::fs::write(
        &supervisor,
        include_bytes!("../src/cmd/remote/ssh/sshj/supervisor/worker.pl"),
    )?;
    let invoke = |mode: &str, args: &[&str]| {
        let mut command = tokio::process::Command::new("/usr/bin/perl");
        command
            .arg(&supervisor)
            .arg(mode)
            .arg(root.path())
            .arg("2200")
            .args(args);
        command.kill_on_drop(true);
        command.output()
    };
    let live = "0123456789abcdef0123456789abcdef";
    let sibling = "fedcba9876543210fedcba9876543210";
    assert!(
        invoke(
            "detach",
            &[
                live,
                "/usr/bin/perl",
                "-e",
                "$SIG{INT} = $SIG{TERM} = 'IGNORE'; sleep 30"
            ]
        )
        .await?
        .status
        .success()
    );
    assert_eq!(invoke("retire", &[]).await?.stdout, b"MARATHON_D96_1 busy\n");
    assert!(root.path().join(format!("{live}.state")).is_file());

    assert!(invoke("run", &[sibling, "/usr/bin/true"]).await?.status.success());
    assert_eq!(
        invoke("control", &[&format!("{sibling}:status")]).await?.stdout,
        format!("{sibling} done:0\n").as_bytes()
    );
    // Expiry at 2.2 s, then TERM and KILL by about 5.2 s; the bound is a hang guard.
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if invoke("control", &[&format!("{live}:status")]).await?.stdout == format!("{live} done:137\n").as_bytes()
            {
                return Ok::<_, std::io::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await??;
    let mut states = std::fs::read_dir(root.path())?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    states.sort();
    assert_eq!(
        states,
        [
            format!("{live}.receipt"),
            format!("{sibling}.receipt"),
            "supervisor.pl".to_owned()
        ]
        .map(std::ffi::OsString::from)
    );
    // The compact receipt still fences attempts to run the same token again.
    assert_eq!(invoke("run", &[live, "/usr/bin/true"]).await?.status.code(), Some(125));
    assert_eq!(d98_retire(&supervisor, root.path()).await?, "MARATHON_D96_1 retired");
    assert!(!root.path().exists());
    Ok(())
}

#[test]
fn the_worker_sender_uses_the_original_server_arguments_d96() -> Result<(), Box<dyn std::error::Error>> {
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    let root = tempfile::Builder::new()
        .prefix("marathon-supervisor-")
        .rand_bytes(10)
        .tempdir_in("/tmp")?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let supervisor = root.path().join("supervisor.pl");
    let sender = root.path().join("sender");
    std::fs::write(
        &supervisor,
        include_bytes!("../src/cmd/remote/ssh/sshj/supervisor/worker.pl"),
    )?;
    std::fs::write(&sender, "printf '<%s>\\n' \"$@\"\n")?;
    let token = "0123456789abcdef0123456789abcdef";
    let encoded = STANDARD.encode(format!("/bin/sh '{}'", sender.display()));
    let output = std::process::Command::new("/usr/bin/perl")
        .arg(&supervisor)
        .arg("rsync")
        .arg(root.path())
        .args([
            "27000",
            token,
            &encoded,
            "--server",
            "--sender",
            ".",
            "path with spaces",
        ])
        .output()?;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.stdout, b"<--server>\n<--sender>\n<.>\n<path with spaces>\n");
    let proof = std::process::Command::new("/usr/bin/perl")
        .arg(supervisor)
        .arg("control")
        .arg(root.path())
        .args(["27000", &format!("{token}:status")])
        .output()?;
    assert!(proof.status.success());
    assert_eq!(String::from_utf8(proof.stdout)?, format!("{token} done:0\n"));
    Ok(())
}

#[derive(Default)]
struct SupervisorWorker {
    sibling_token: std::sync::Mutex<Option<String>>,
    resistant_token: std::sync::Mutex<Option<String>>,
    stall_term: bool,
    /// D98: a running recorder has printed its start marker; status says `recording:`.
    confirm_recordings: std::sync::atomic::AtomicBool,
    /// D98: the control stream exits at once, as a filtered or failing worker's would.
    refuse_stream: bool,
    /// D98: a recorder's status requests get no reply while set, after the first `answered_statuses`.
    silent_status: std::sync::atomic::AtomicBool,
    answered_statuses: std::sync::Mutex<usize>,
    recordings: std::sync::Mutex<std::collections::HashMap<String, bool>>,
    batches: std::sync::Mutex<std::collections::HashSet<String>>,
    /// U261: every recorder has exited with status 16 and this stderr in its receipt.
    refused_stderr: Option<String>,
}

impl fake_worker::router::Handler for SupervisorWorker {
    fn name(&self) -> &str {
        "supervisor-fixture"
    }

    fn route<'a>(
        &'a self,
        request: &'a fake_worker::router::Request,
        _fs: &'a fake_worker::fs::VirtualFs,
    ) -> fake_worker::router::HandlerFuture<'a> {
        use fake_worker::script::Action;
        Box::pin(async move {
            let argv: Vec<_> = request.argv.iter().map(String::as_str).collect();
            let root = "/tmp/marathon-supervisor-0123456789";
            match argv.as_slice() {
                ["/usr/bin/perl", "-MMIME::Base64", "-e", _, "--", _, _] => {
                    Ok(Some(Action::output(&format!("MARATHON_D96_1 {root}\n"), 0)))
                }
                ["/usr/bin/perl", _, "control-stream", ..] if self.refuse_stream => Ok(Some(Action::output("", 1))),
                ["/usr/bin/perl", program, "retire", directory, _]
                    if *directory == root && *program == format!("{root}/supervisor.pl") =>
                {
                    Ok(Some(Action::output("MARATHON_D96_1 retired\n", 0)))
                }
                ["/usr/bin/perl", program, "control", directory, _, control]
                    if *directory == root && *program == format!("{root}/supervisor.pl") =>
                {
                    let (token, operation) = control.split_once(':').unwrap();
                    if operation == "status"
                        && self.silent_status.load(std::sync::atomic::Ordering::SeqCst)
                        && self.recordings.lock().unwrap().contains_key(token)
                        && {
                            let mut left = self.answered_statuses.lock().unwrap();
                            let silent = *left == 0;
                            *left = left.saturating_sub(1);
                            silent
                        }
                    {
                        return Ok(Some(ssh::hang(vec![])));
                    }
                    if let Some(stderr) = &self.refused_stderr
                        && self.recordings.lock().unwrap().contains_key(token)
                    {
                        let receipt = format!("{token} done:16 {}\n", STANDARD.encode(stderr));
                        return Ok(Some(Action::output(&receipt, 0)));
                    }
                    if let Some(recording) = self.recordings.lock().unwrap().get_mut(token) {
                        let running = if self.confirm_recordings.load(std::sync::atomic::Ordering::SeqCst) {
                            "recording:100000"
                        } else {
                            "running:100000"
                        };
                        let proof = if *recording {
                            "done:0"
                        } else if operation == "1" {
                            *recording = true;
                            running
                        } else {
                            running
                        };
                        return Ok(Some(Action::output(&format!("{token} {proof}\n"), 0)));
                    }
                    if self.batches.lock().unwrap().contains(token) {
                        let proof = if operation == "3" { "done:137" } else { "running:100000" };
                        return Ok(Some(Action::output(&format!("{token} {proof}\n"), 0)));
                    }
                    if operation == "2" && self.stall_term {
                        return Ok(Some(ssh::hang(vec![])));
                    }
                    let proof = if self.sibling_token.lock().unwrap().as_deref() == Some(token) {
                        "done:0"
                    } else if self.resistant_token.lock().unwrap().as_deref() == Some(token) {
                        if operation == "3" { "done:137" } else { "running:100000" }
                    } else if operation == "3" {
                        "not-started"
                    } else {
                        "unknown"
                    };
                    Ok(Some(Action::output(&format!("{token} {proof}\n"), 0)))
                }
                [
                    "exec",
                    "/usr/bin/perl",
                    program,
                    "run",
                    directory,
                    _,
                    token,
                    "/bin/sh",
                    "-c",
                    "echo sibling",
                ] if *directory == root && *program == format!("{root}/supervisor.pl") => {
                    *self.sibling_token.lock().unwrap() = Some((*token).into());
                    Ok(Some(Action::output("sibling\n", 0)))
                }
                [
                    "exec",
                    "/usr/bin/perl",
                    program,
                    "run",
                    directory,
                    _,
                    token,
                    "/bin/sh",
                    "-c",
                    "echo resistant",
                ] if *directory == root && *program == format!("{root}/supervisor.pl") => {
                    *self.resistant_token.lock().unwrap() = Some((*token).into());
                    let mut action = ssh::hang(vec![ssh::chunk("ready\n")]);
                    if let Action::Output { withhold_close, .. } = &mut action {
                        *withhold_close = true;
                    }
                    Ok(Some(action))
                }
                [
                    "exec",
                    "/usr/bin/perl",
                    program,
                    mode @ ("detach" | "run"),
                    directory,
                    _,
                    token,
                    "/bin/sh",
                    "-c",
                    command,
                ] if *directory == root
                    && *program == format!("{root}/supervisor.pl")
                    && command.starts_with("xcrun simctl io ")
                    && command.contains(" recordVideo ") =>
                {
                    self.recordings.lock().unwrap().insert((*token).into(), false);
                    Ok(Some(if *mode == "detach" {
                        Action::output("", 0)
                    } else {
                        ssh::hang(vec![])
                    }))
                }
                [
                    "exec",
                    "/usr/bin/perl",
                    program,
                    "run",
                    directory,
                    _,
                    token,
                    "/bin/sh",
                    "-c",
                    "echo batch",
                ] if *directory == root && *program == format!("{root}/supervisor.pl") => {
                    self.batches.lock().unwrap().insert((*token).into());
                    Ok(Some(ssh::hang(vec![ssh::chunk("ready\n")])))
                }
                _ => Ok(None),
            }
        })
    }
}

#[tokio::test]
async fn five_recording_simulators_share_a_ten_session_worker_d96() {
    use marathon_apple::bin::{
        Tools,
        xcrun::simctl::{SimctlService, service::IoService},
    };
    use marathon_config::vendor::{AppleTimeoutParameters, Codec, Display, Mask, apple_timeout_configuration};
    support::bounded(10, async {
        let world = Arc::new(SupervisorWorker::default());
        let fake = ssh::fake_with_handlers(ssh::script(vec![]), 10, |_| {}, vec![world.clone()]).await;
        let (logging, _) = ssh::logging(false);
        let executor = Arc::new(
            ssh::factory(&logging)
                .connect_worker("127.0.0.1", fake.port(), ssh::password(), None, false, Arc::default())
                .await
                .unwrap(),
        );
        let io = IoService::new(SimctlService::new(Tools {
            executor: executor.clone(),
            timeouts: Arc::new(apple_timeout_configuration(AppleTimeoutParameters::empty())),
            runtime: Arc::new(support::Runtime),
            logging,
        }));
        let mut batches = Vec::new();
        let mut recordings = Vec::new();
        let start = std::time::Instant::now();
        for index in 0..5 {
            let batch = executor
                .execute(CommandRequest::new(["echo", "batch"], duration(60_000)), context())
                .await
                .unwrap();
            assert_eq!(batch.base.stdout.receive().await.unwrap().as_deref(), Some("ready"));
            batches.push(batch);
            recordings.push(
                io.record_video(
                    &format!("fixture-{index}"),
                    "/tmp/marathon/video.mp4",
                    Codec::H264,
                    Display::Internal,
                    Mask::Black,
                    &context(),
                )
                .await
                .unwrap(),
            );
        }
        fake.events_at_least("exec", 11).await;
        assert_eq!(world.recordings.lock().unwrap().len(), 5);
        let launch = start.elapsed();
        for _ in 0..5 {
            let healthcheck = executor
                .critical_execute(CommandRequest::new(["echo", "sibling"], duration(1000)), context())
                .await
                .unwrap();
            assert_eq!(healthcheck.stdout, ["sibling"]);
        }
        assert!(fake.events("session_rejected").await.is_empty());
        for recording in &recordings {
            recording.interrupt().unwrap();
        }
        for recording in recordings {
            assert_eq!(recording.await_result(&context()).await.unwrap().exit_code, Some(0));
            recording.close().await.unwrap();
        }
        println!(
            "five batch and recorder pairs opened in {launch:?}; healthchecks and finalization by {:?}",
            start.elapsed()
        );
        for batch in batches {
            batch.kill().unwrap();
            batch.close().await.unwrap();
        }
        executor.close().await.unwrap();
        fake.worker.shutdown().await.unwrap();
    })
    .await;
}

/// D98: a fake worker with the given supervisor behaviour, an executor with a 60 s keep-alive interval (so nothing
/// waits a renewal interval inside a test's hang guard), and a launched detached recorder.
async fn d98_recorder(
    world: Arc<SupervisorWorker>,
    interval_seconds: i64,
) -> (
    ssh::Fake,
    Arc<marathon_apple::cmd::remote::ssh::sshj::SshCommandExecutor>,
    marathon_apple::cmd::CommandSession,
) {
    use marathon_apple::bin::{
        Tools,
        xcrun::simctl::{SimctlService, service::IoService},
    };
    use marathon_config::vendor::{AppleTimeoutParameters, Codec, Display, Mask, apple_timeout_configuration};
    let fake = ssh::fake_with_handlers(ssh::script(vec![]), 10, |_| {}, vec![world]).await;
    let (logging, _) = ssh::logging(false);
    let mut factory = ssh::factory(&logging);
    factory.keep_alive_interval = jcompat::jtime::Duration::of_seconds(interval_seconds);
    let executor = Arc::new(
        factory
            .connect_worker("127.0.0.1", fake.port(), ssh::password(), None, false, Arc::default())
            .await
            .unwrap(),
    );
    let io = IoService::new(SimctlService::new(Tools {
        executor: executor.clone(),
        timeouts: Arc::new(apple_timeout_configuration(AppleTimeoutParameters::empty())),
        runtime: Arc::new(support::Runtime),
        logging,
    }));
    let recording = io
        .record_video(
            "fixture",
            "/tmp/marathon/video.mp4",
            Codec::H264,
            Display::Internal,
            Mask::Black,
            &context(),
        )
        .await
        .unwrap();
    (fake, executor, recording)
}

#[tokio::test]
async fn a_refused_recorder_is_one_debug_line_and_its_stderr_reaches_the_controller_u261_u264() {
    use marathon_apple::bin::{
        Tools,
        xcrun::simctl::{SimctlService, service::IoService},
    };
    use marathon_config::vendor::{AppleTimeoutParameters, Codec, Display, Mask, apple_timeout_configuration};
    for stderr in [
        // Xcode 26.6's busy refusal; before U264 the supervisor warned about it with restart advice (U262).
        "Error starting video recorder: Error Domain=NSPOSIXErrorDomain Code=16 \"Resource busy\" \
         UserInfo={NSLocalizedFailureReason=Host recording is already in progress}.\n",
        "Invalid device: fixture\n",
    ] {
        for debug in [false, true] {
            support::bounded(30, async {
                let world = Arc::new(SupervisorWorker {
                    refused_stderr: Some(stderr.into()),
                    ..SupervisorWorker::default()
                });
                let fake = ssh::fake_with_handlers(ssh::script(vec![]), 10, |_| {}, vec![world]).await;
                let (logging, events) = ssh::logging(debug);
                let executor = Arc::new(
                    ssh::factory(&logging)
                        .connect_worker("127.0.0.1", fake.port(), ssh::password(), None, false, Arc::default())
                        .await
                        .unwrap(),
                );
                let io = IoService::new(SimctlService::new(Tools {
                    executor: executor.clone(),
                    timeouts: Arc::new(apple_timeout_configuration(AppleTimeoutParameters::empty())),
                    runtime: Arc::new(support::Runtime),
                    logging,
                }));
                let recording = io
                    .record_video(
                        "fixture-udid",
                        "/tmp/marathon/video.mp4",
                        Codec::H264,
                        Display::Internal,
                        Mask::Black,
                        &context(),
                    )
                    .await
                    .unwrap();
                recording.interrupt().unwrap();
                assert_eq!(recording.await_result(&context()).await.unwrap().exit_code, Some(16));
                // The controller (the iOS recording) reports the failure, once, from the receipt's stderr.
                let report = recording.worker_report().unwrap();
                assert_eq!(report.stderr.get().map(String::as_str), Some(stderr));
                let lines: Vec<_> = events
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, _, message, _)| message.starts_with("Worker recorder "))
                    .map(|(_, level, message, _)| (*level, message.clone()))
                    .collect();
                if debug {
                    assert_eq!(lines.len(), 1, "{lines:?}");
                    let (level, line) = &lines[0];
                    assert_eq!(*level, Level::DEBUG, "{line}");
                    // U261: the simulator and worker are named.
                    assert!(
                        line.contains(&format!(
                            " for simulator fixture-udid on worker 127.0.0.1:{} failed",
                            fake.port()
                        )),
                        "{line}"
                    );
                    assert!(!line.contains("restart"), "{line}");
                } else {
                    assert!(lines.is_empty(), "nothing with debug off: {lines:?}");
                }
                recording.close().await.unwrap();
                executor.close().await.unwrap();
                fake.worker.shutdown().await.unwrap();
            })
            .await;
        }
    }
}

async fn d98_execs(fake: &ssh::Fake) -> Vec<String> {
    fake.events("exec")
        .await
        .iter()
        .map(|row| String::from_utf8(STANDARD.decode(row["detail"]["base64"].as_str().unwrap()).unwrap()).unwrap())
        .collect()
}

async fn d98_status_requests(fake: &ssh::Fake) -> usize {
    fake.events("supervisor_request")
        .await
        .iter()
        .filter(|row| matches_operation(row, ":status"))
        .count()
}

#[tokio::test]
async fn a_supervised_recorder_reports_its_start_from_the_worker_status_d98() {
    for confirmed in [true, false] {
        // Hang guard only. Without the launch wake the first status comes one 60 s renewal interval later.
        support::bounded(30, async {
            let world = Arc::new(SupervisorWorker {
                confirm_recordings: confirmed.into(),
                ..SupervisorWorker::default()
            });
            let (fake, executor, recording) = d98_recorder(world, 60).await;
            let mut started = recording
                .worker_report()
                .expect("a detached recorder reports its start")
                .started;
            if confirmed {
                started.wait_for(|started| *started).await.unwrap();
            } else {
                // Several status probes after launch, none of which reported a start.
                while d98_status_requests(&fake).await < 3 {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                assert!(!*started.borrow());
            }
            recording.interrupt().unwrap();
            assert_eq!(recording.await_result(&context()).await.unwrap().exit_code, Some(0));
            recording.close().await.unwrap();
            executor.close().await.unwrap();
            fake.worker.shutdown().await.unwrap();
        })
        .await;
    }
}

#[tokio::test]
async fn start_probes_do_not_move_the_renewal_schedule_d98() {
    use std::time::{Duration, Instant};
    support::bounded(90, async {
        let world = Arc::new(SupervisorWorker::default());
        // A 12 s renewal interval; the recorder confirms its start 10 s after the first renewal. The wide interval
        // keeps the two outcomes 10 s apart, 5 s each side of the bound, whatever the host's scheduling delays.
        let (fake, executor, recording) = d98_recorder(world.clone(), 12).await;
        let renewals = || async {
            fake.events("supervisor_request")
                .await
                .iter()
                .filter(|row| {
                    String::from_utf8_lossy(&STANDARD.decode(row["detail"]["base64"].as_str().unwrap()).unwrap())
                        .contains(":renew-")
                })
                .count()
        };
        let wait_for_renewals = |count: usize| async move {
            while renewals().await < count {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Instant::now()
        };
        let first = wait_for_renewals(1).await;
        tokio::time::sleep(Duration::from_secs(10)).await;
        world
            .confirm_recordings
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let mut started = recording.worker_report().unwrap().started;
        started.wait_for(|started| *started).await.unwrap();
        let second = wait_for_renewals(2).await;
        // On schedule the second renewal is 12 s after the first. Timed from the probe that ended polling it came
        // 22 s after.
        let gap = second - first;
        println!("renewals {gap:?} apart");
        assert!(gap < Duration::from_secs(17), "{gap:?}");
        recording.interrupt().unwrap();
        assert_eq!(recording.await_result(&context()).await.unwrap().exit_code, Some(0));
        recording.close().await.unwrap();
        executor.close().await.unwrap();
        fake.worker.shutdown().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn an_unanswered_start_probe_does_not_hold_back_a_renewal_d98() {
    use std::time::{Duration, Instant};
    support::bounded(60, async {
        // The launch probe is answered, which gives the lease its renewal stamp; later probes never are, and renewals
        // are. The 1 s renewal falls inside the second probe's 4 s wait.
        let world = Arc::new(SupervisorWorker {
            silent_status: true.into(),
            answered_statuses: 1.into(),
            ..SupervisorWorker::default()
        });
        let (fake, executor, recording) = d98_recorder(world.clone(), 1).await;
        let requests = |operation: &'static str| {
            let fake = &fake;
            async move {
                fake.events("supervisor_request")
                    .await
                    .iter()
                    .filter(|row| {
                        String::from_utf8_lossy(&STANDARD.decode(row["detail"]["base64"].as_str().unwrap()).unwrap())
                            .contains(operation)
                    })
                    .count()
            }
        };
        // The first unanswered probe: the second status request.
        while requests(":status").await < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let probed = Instant::now();
        while requests(":renew-").await == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // The renewal falls due about 0.8 s after that probe was sent; held back by it, it would come with the probe's
        // 4 s timeout.
        let waited = probed.elapsed();
        println!("first renewal {waited:?} after the unanswered probe");
        assert!(waited < Duration::from_millis(2500), "{waited:?}");
        world.silent_status.store(false, std::sync::atomic::Ordering::SeqCst);
        recording.interrupt().unwrap();
        assert_eq!(recording.await_result(&context()).await.unwrap().exit_code, Some(0));
        recording.close().await.unwrap();
        executor.close().await.unwrap();
        fake.worker.shutdown().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn the_start_probe_never_dials_falls_back_or_counts_a_miss_d98() {
    for refuse_stream in [true, false] {
        support::bounded(60, async {
            let world = Arc::new(SupervisorWorker {
                confirm_recordings: true.into(),
                refuse_stream,
                silent_status: (!refuse_stream).into(),
                ..SupervisorWorker::default()
            });
            let (fake, executor, recording) = d98_recorder(world.clone(), 60).await;
            let streams = |execs: &[String]| execs.iter().filter(|wire| wire.contains(" control-stream ")).count();
            if refuse_stream {
                // The launch opens the stream once, which exits at once. Probes then find no stream to ask.
                while streams(&d98_execs(&fake).await) == 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                // Five probe periods: the old launch status dialled and used a data session within this time.
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            } else {
                // Each probe goes unanswered for 4 s. Two such misses used to replace the control connection, and one
                // used to send the request again on a data session.
                while d98_status_requests(&fake).await < 3 {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
            let execs = d98_execs(&fake).await;
            let data_requests: Vec<_> = execs
                .iter()
                .filter(|wire| wire.contains("/supervisor.pl control "))
                .collect();
            assert!(
                data_requests.is_empty(),
                "a start probe used a data session: {data_requests:#?}"
            );
            assert_eq!(streams(&execs), 1, "a start probe dialled or reconnected");
            let report = recording.worker_report().unwrap();
            assert!(!*report.started.borrow());
            world.silent_status.store(false, std::sync::atomic::Ordering::SeqCst);
            recording.interrupt().unwrap();
            assert_eq!(recording.await_result(&context()).await.unwrap().exit_code, Some(0));
            recording.close().await.unwrap();
            executor.close().await.unwrap();
            fake.worker.shutdown().await.unwrap();
        })
        .await;
    }
}

#[tokio::test]
async fn short_worker_commands_finish_without_a_control_connection_d96() {
    use marathon_apple::cmd::device_commands::DeviceCommands;
    let fake = ssh::fake_with_handlers(
        ssh::script(vec![]),
        1,
        |_| {},
        vec![Arc::new(SupervisorWorker::default())],
    )
    .await;
    let (logging, _) = ssh::logging(false);
    let executor = Arc::new(
        ssh::factory(&logging)
            .connect_worker("127.0.0.1", fake.port(), ssh::password(), None, false, Arc::default())
            .await
            .unwrap(),
    );
    let commands = DeviceCommands::new(executor.clone());
    for _ in 0..4 {
        let result = commands
            .critical_execute(CommandRequest::new(["echo", "sibling"], duration(1000)), context())
            .await
            .unwrap();
        assert_eq!(result.stdout, ["sibling"]);
    }
    commands
        .wait_handed(marathon_core::executor::shared::Signal::new())
        .await
        .unwrap();
    assert!(commands.handed_left().is_empty());
    assert_eq!(fake.events("connection").await.len(), 1);
    assert_eq!(fake.events("exec").await.len(), 5);
    executor.close().await.unwrap();
    let commands = fake.events("exec").await;
    assert_eq!(commands.len(), 6);
    let retirement = STANDARD
        .decode(commands[5]["detail"]["base64"].as_str().unwrap())
        .unwrap();
    assert!(
        String::from_utf8(retirement)
            .unwrap()
            .contains("/supervisor.pl retire ")
    );
    assert_eq!(fake.events("connection").await.len(), 1);
    fake.worker.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sealed_device_stops_a_token_when_the_exec_reply_is_missing_d96() {
    use marathon_apple::cmd::{
        TransferCleanup,
        device_commands::{DeviceCommands, SealCause},
        failure,
    };
    use marathon_core::executor::shared::Signal;
    use marathon_error::FailureKind;
    let fake = ssh::fake_with_handlers(
        ssh::script(vec![]),
        2,
        |config| config.script.faults.silent_exec = Some("fixture-no-launch-ack".into()),
        vec![Arc::new(SupervisorWorker::default())],
    )
    .await;
    let (logging, _) = ssh::logging(false);
    let executor = ssh::factory(&logging)
        .connect_worker("127.0.0.1", fake.port(), ssh::password(), None, false, Arc::default())
        .await
        .unwrap();
    let executor = Arc::new(executor);
    let device = DeviceCommands::new(executor.clone());
    let sibling = DeviceCommands::new(executor);
    let executing = {
        let device = device.clone();
        tokio::spawn(marathon_apple::cmd::exec_outcome::observe(async move {
            device
                .execute(
                    CommandRequest::new(["echo", "fixture-no-launch-ack"], duration(30000)),
                    context(),
                )
                .await
        }))
    };
    let wire = |row: &serde_json::Value| {
        String::from_utf8(STANDARD.decode(row["detail"]["base64"].as_str().unwrap()).unwrap()).unwrap()
    };
    let execs = fake.events_at_least("exec", 2).await;
    let waiting = execs
        .iter()
        .find(|row| wire(row).ends_with("fixture-no-launch-ack'"))
        .unwrap();
    let handed = device.handed_commands();
    assert_eq!(handed.len(), 1);
    assert!(handed[0].ended().is_none());
    device.seal(
        SealCause::RunStop,
        failure(FailureKind::IllegalState, "fixture shutdown"),
    );
    let (proof, command) = tokio::time::timeout(std::time::Duration::from_secs(6), async {
        tokio::join!(
            handed[0].wait_in(Signal::new()),
            sibling.critical_execute(CommandRequest::new(["echo", "sibling"], duration(30000)), context())
        )
    })
    .await
    .unwrap();
    let proof = proof.unwrap();
    assert_eq!(command.unwrap().stdout, ["sibling"]);
    assert_eq!(proof, TransferCleanup::Confirmed);
    // U271 (review round 2): the exec was sent and its reply had not come, so it may have started; the gate's
    // cancellation must not turn that into "started nothing", which would let xcodebuild's opener count it as closed.
    let (result, started_nothing) = executing.await.unwrap();
    assert!(result.is_err());
    assert!(
        !started_nothing,
        "an exec awaiting its reply was reported as never started"
    );
    let launch = wire(waiting);
    let token = launch.split_ascii_whitespace().nth(6).unwrap();
    let requests = fake.events("supervisor_request").await;
    let stopped: Vec<_> = requests
        .iter()
        .filter(|row| wire(row).ends_with(&format!("{token}:3")))
        .collect();
    assert_eq!(stopped.len(), 1);
    assert_ne!(stopped[0]["connection"], waiting["connection"]);
    assert!(device.handed_commands().is_empty());
    drop((device, sibling));
    fake.worker.shutdown().await.unwrap();
}

/// U271: a supervised exec whose reply has not come when its caller stops it (a screenshot at its test's end, say)
/// returns the caller's cancellation, which `safe_execute` does not log; it was a connection failure, logged as
/// `ERROR Unable to start a remote shell session [...]`. A real loss of the connection on the same wait still is.
#[tokio::test(flavor = "multi_thread")]
async fn our_own_stop_of_an_exec_waiting_for_its_reply_is_not_an_error_u271() {
    use marathon_core::{error::VendorError, executor::shared::Signal, vendor::VendorContext};
    let fake = ssh::fake_with_handlers(
        ssh::script(vec![]),
        3,
        |config| config.script.faults.silent_exec = Some("fixture-no-launch-ack".into()),
        vec![Arc::new(SupervisorWorker::default())],
    )
    .await;
    let (logging, events) = ssh::logging(false);
    let executor = Arc::new(
        ssh::factory(&logging)
            .connect_worker("127.0.0.1", fake.port(), ssh::password(), None, false, Arc::default())
            .await
            .unwrap(),
    );
    let request = || CommandRequest::new(["echo", "fixture-no-launch-ack"], duration(30000));
    let session_lines = || {
        ssh::lines(&events)
            .into_iter()
            .filter(|(_, level, message)| {
                *level == Level::ERROR && message.contains("a remote shell session [echo, fixture-no-launch-ack]")
            })
            .count()
    };
    let stop = Signal::new();
    let stopped = {
        let executor = executor.clone();
        let context = VendorContext {
            cancellation: stop.clone(),
            ..context()
        };
        tokio::spawn(async move { executor.safe_execute(request(), context).await })
    };
    fake.events_at_least("exec", 2).await;
    stop.cancel(marathon_apple::cmd::cancelled("fixture stop"));
    let result = support::bounded(10, stopped).await.unwrap();
    assert!(matches!(result, Err(VendorError::Cancelled(_))), "{result:?}");
    assert_eq!(session_lines(), 0);

    let lost = {
        let executor = executor.clone();
        tokio::spawn(async move { executor.safe_execute(request(), context()).await })
    };
    fake.events_at_least("exec", 3).await;
    fake.worker.shutdown().await.unwrap();
    let result = support::bounded(40, lost).await.unwrap();
    assert!(matches!(result, Ok(None)), "{result:?}");
    assert_eq!(session_lines(), 1, "{:?}", ssh::lines(&events));
}

#[tokio::test]
async fn d30_escalation_joins_worker_proof_without_a_data_close_d96() {
    use marathon_apple::cmd::remote::ssh::sshj::SshjCommandExecutorFactory;

    let fake = ssh::fake_with_handlers(
        ssh::script(vec![]),
        3,
        |_| {},
        vec![Arc::new(SupervisorWorker::default())],
    )
    .await;
    let (logging, _) = ssh::logging(false);
    let expire_grace = Arc::new(tokio::sync::Notify::new());
    let clock = support::Clock::with_gated_delay(20_000, expire_grace.clone());
    let executor = SshjCommandExecutorFactory::new(logging, Arc::new(support::Runtime), Arc::new(clock))
        .connect_worker("127.0.0.1", fake.port(), ssh::password(), None, false, Arc::default())
        .await
        .unwrap();
    let session = executor
        .execute(CommandRequest::new(["echo", "resistant"], duration(120000)), context())
        .await
        .unwrap();
    assert_eq!(session.base.stdout.receive().await.unwrap().as_deref(), Some("ready"));

    let closing = tokio::spawn(async move { session.close().await });
    let term = tokio::time::timeout(std::time::Duration::from_secs(4), async {
        loop {
            let events = fake.events("supervisor_request").await;
            if let Some(term) = events.into_iter().find(|row| matches_operation(row, ":2")) {
                break term;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!closing.is_finished());

    support::bounded(5, async {
        loop {
            if fake
                .events("supervisor_reply")
                .await
                .iter()
                .any(|end| end["connection"] == term["connection"] && end["detail"] == term["detail"])
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;

    // The worker's send record can precede receipt of the TERM reply.
    // Expire only the D30 grace so an in-flight control exchange stays live.
    expire_grace.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), closing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let execs = fake.events("exec").await;
    let requests = fake.events("supervisor_request").await;
    let kill = requests.iter().find(|row| matches_operation(row, ":3")).unwrap();
    let run = execs
        .iter()
        .find(|row| matches_operation(row, "echo resistant'"))
        .unwrap();
    assert_ne!(kill["connection"], run["connection"]);
    assert_ne!(term["connection"], run["connection"]);
    assert_eq!(kill["channel"], term["channel"]);
    assert!(kill["seq"].as_u64().unwrap() > term["seq"].as_u64().unwrap());
    // Neither escalation request consumed the original SSH session's acknowledgement or CLOSE.
    assert!(fake.events("signal").await.is_empty());
    executor.close().await.unwrap();
    fake.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn d97_signal_deadline_kills_the_worker_token_while_term_is_waiting() {
    use marathon_apple::cmd::remote::ssh::sshj::SshjCommandExecutorFactory;
    use marathon_core::signal_finish::{SignalDeadline, SignalFinish};
    let fake = ssh::fake_with_handlers(
        ssh::script(vec![]),
        3,
        |_| {},
        vec![Arc::new(SupervisorWorker {
            stall_term: true,
            ..Default::default()
        })],
    )
    .await;
    let (logging, _) = ssh::logging(false);
    let clock = Arc::new(support::Clock::default());
    let factory = SshjCommandExecutorFactory::new(logging, Arc::new(support::Runtime), clock.clone());
    let executor = factory
        .connect_worker("127.0.0.1", fake.port(), ssh::password(), None, false, Arc::default())
        .await
        .unwrap();
    let finish = Arc::new(SignalDeadline::default());
    executor.set_provider_signal_deadline(finish.clone());
    let command = executor
        .execute(CommandRequest::new(["echo", "resistant"], duration(120_000)), context())
        .await
        .unwrap();
    assert_eq!(command.base.stdout.receive().await.unwrap().as_deref(), Some("ready"));
    let closing = tokio::spawn(async move { command.close().await });
    let term = support::bounded(5, async {
        loop {
            if let Some(row) = fake
                .events("supervisor_request")
                .await
                .into_iter()
                .find(|row| matches_operation(row, ":2"))
            {
                break row;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;

    // Start the close late in the run's signal window. Keep the virtual clock
    // at 17 s while the network delivers KILL and proof, before TERM can time out.
    tokio::time::pause();
    finish.set(SignalFinish::from_start(clock.now() - 16_000));
    tokio::time::advance(std::time::Duration::from_millis(1_030)).await;
    let watchdog = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let kill = loop {
        if let Some(kill) = fake
            .events("supervisor_request")
            .await
            .into_iter()
            .find(|row| matches_operation(row, ":3"))
        {
            break kill;
        }
        assert!(
            std::time::Instant::now() < watchdog,
            "the token must be killed at the signal cutoff"
        );
        tokio::task::yield_now().await;
    };
    tokio::time::resume();
    support::bounded(5, closing).await.unwrap().unwrap();
    assert_eq!(kill["connection"], term["connection"]);
    assert_eq!(kill["channel"], term["channel"]);
    assert!(fake.events("signal").await.is_empty());
    executor.close().await.unwrap();
    fake.worker.shutdown().await.unwrap();
}

fn matches_operation(row: &serde_json::Value, suffix: &str) -> bool {
    STANDARD
        .decode(row["detail"]["base64"].as_str().unwrap())
        .unwrap()
        .ends_with(suffix.as_bytes())
}

/// U270: a supervised command outlives its first renewal, which dials the supervisor's control connection; closing the
/// executor then ends that connection with the data connection, while the executor is still held. Before, the control
/// connection stayed open until the executor was dropped, which a task still holding its simulator put off until exit.
#[tokio::test(flavor = "multi_thread")]
async fn closing_an_executor_closes_its_supervisor_control_connection_u270() {
    let fake = ssh::fake_with_handlers(
        ssh::script(vec![]),
        3,
        |_| {},
        vec![Arc::new(SupervisorWorker::default())],
    )
    .await;
    let (logging, events) = ssh::logging(false);
    let mut factory = ssh::factory(&logging);
    // The supervisor renews a lease once per keep-alive interval (1 s here) on its control connection.
    factory.keep_alive_interval = duration(1000);
    let executor = factory
        .connect_worker("127.0.0.1", fake.port(), ssh::password(), None, false, Arc::default())
        .await
        .unwrap();
    let session = executor
        .execute(CommandRequest::new(["echo", "batch"], duration(120_000)), context())
        .await
        .unwrap();
    assert_eq!(session.base.stdout.receive().await.unwrap().as_deref(), Some("ready"));
    let renewal = fake.events_at_least("supervisor_request", 1).await.remove(0);
    let connections = fake.events("connection").await;
    assert_eq!(connections.len(), 2, "the data connection and the control connection");
    assert_ne!(renewal["connection"], connections[0]["connection"]);
    support::bounded(30, executor.close()).await.unwrap();
    let ended = support::bounded(30, async {
        loop {
            let ended = fake.events("connection_end").await;
            if ended.len() == 2 {
                break ended;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(ended.iter().any(|e| e["connection"] == renewal["connection"]));
    assert_eq!(
        fake.events("connection").await.len(),
        2,
        "no control connection is dialed after the close"
    );
    drop(session);
    drop(executor);
    let dying: Vec<_> = ssh::lines(&events)
        .into_iter()
        .filter(|(_, _, message)| message.starts_with("Dying because"))
        .collect();
    assert_eq!(dying, vec![]);
    fake.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_slow_control_exchange_keeps_its_transport_d96() {
    use marathon_apple::cmd::remote::ssh::sshj::SshjCommandExecutorFactory;
    let fake = ssh::fake_with_handlers(
        ssh::script(vec![]),
        3,
        |_| {},
        vec![Arc::new(SupervisorWorker {
            stall_term: true,
            ..Default::default()
        })],
    )
    .await;
    let (logging, _) = ssh::logging(false);
    let expire_grace = Arc::new(tokio::sync::Notify::new());
    let clock = support::Clock::with_gated_delay(20_000, expire_grace.clone());
    let executor = SshjCommandExecutorFactory::new(logging, Arc::new(support::Runtime), Arc::new(clock))
        .connect_worker("127.0.0.1", fake.port(), ssh::password(), None, false, Arc::default())
        .await
        .unwrap();
    let session = executor
        .execute(CommandRequest::new(["echo", "resistant"], duration(120_000)), context())
        .await
        .unwrap();
    assert_eq!(session.base.stdout.receive().await.unwrap().as_deref(), Some("ready"));
    let closing = tokio::spawn(async move { session.close().await });
    let term = fake.events_at_least("supervisor_request", 1).await.remove(0);
    assert!(matches_operation(&term, ":2"));
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_millis(4_100)).await;
    tokio::time::resume();
    support::bounded(5, async {
        loop {
            if fake
                .events("exec")
                .await
                .iter()
                .any(|event| matches_operation(event, ":2"))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    expire_grace.notify_one();
    support::bounded(5, closing).await.unwrap().unwrap();
    let requests = fake.events("supervisor_request").await;
    let kill = requests.iter().find(|event| matches_operation(event, ":3")).unwrap();
    assert_eq!(kill["connection"], term["connection"]);
    assert_eq!(kill["channel"], term["channel"]);
    executor.close().await.unwrap();
    fake.worker.shutdown().await.unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn worker_commands_and_senders_stop_over_independent_real_ssh_d96() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempfile::tempdir()?;
    let ssh = fixture.path().join("ssh");
    std::fs::write(
        &ssh,
        "#!/bin/sh\nexec /usr/bin/ssh -o UserKnownHostsFile=/dev/null -o GlobalKnownHostsFile=/dev/null \"$@\"\n",
    )?;
    std::fs::set_permissions(ssh, std::fs::Permissions::from_mode(0o700))?;
    let mut path = std::ffi::OsString::from(fixture.path());
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    // U269: the child abandons its worker connection, as a hard-killed run does, so nothing retires the supervisor
    // roots its real worker creates. Each is recorded right after it is created; this guard retires them once the child has
    // exited, whatever the child's outcome, and on a panic here too.
    let mut roots = SupervisorRoots::new(fixture.path())?;
    let output = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "real_worker_commands_and_senders_child"])
        .env("PATH", path)
        .env("MRS_D96_REAL_WORKER", "1")
        .env("MRS_D96_ROOT_DIR", fixture.path())
        .output()?;
    let retired = roots.retire();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let retired = retired?;
    if !String::from_utf8_lossy(&output.stderr).contains("loopback witness requires") {
        assert!(!retired.is_empty(), "the child's sshd recorded no supervisor root");
    }
    for root in retired {
        assert!(!root.exists(), "{}", root.display());
    }
    Ok(())
}

/// U269: a Perl module the loopback child's sshd loads into every Perl its sessions run (through `PERL5OPT`). It wraps
/// `mkdir`, which `File::Temp` calls when the worker's bootstrap (or `init`) creates a supervisor root: under a shared
/// lock on the ledger's lock file, it records a root in the ledger right after the `mkdir` that created it succeeds.
/// `mkdir` fails on an existing name, so only roots this test created are ever recorded, never one another process
/// owns. A session killed between that `mkdir` and its record leaks the root (accepted: a leak is better than retiring
/// a root that is not this test's). Once the parent holds the lock exclusively, a creation under way has finished and
/// a later one fails.
#[cfg(target_os = "macos")]
const ROOT_LEDGER_MODULE: &str = r#"package U269Ledger;
use strict;
use warnings;
use Fcntl qw(:flock);
my $attempts = 0;
sub note {
    open my $ledger, '>>', $ENV{MRS_U269_LEDGER} or die "open root ledger: $!";
    print {$ledger} "@_\n" or die "write root ledger: $!";
    close $ledger or die "close root ledger: $!";
}
BEGIN {
    *CORE::GLOBAL::mkdir = sub (_;$) {
        my ($path, @mode) = @_;
        my $mode = @mode ? $mode[0] : 0777;
        return CORE::mkdir($path, $mode) unless $path =~ m{\A/tmp/marathon-supervisor-};
        open my $lock, '<', $ENV{MRS_U269_LOCK} or die "open root ledger lock: $!";
        flock($lock, LOCK_SH | LOCK_NB) or die "the test is retiring its supervisor roots";
        # Unique among the sshd's sessions: a process id with this process's own count.
        my $attempt = "$$." . ++$attempts;
        my $made = CORE::mkdir($path, $mode);
        my $error = $!;
        if ($made) {
            # The root's inode, so the parent retires this directory and not a later one that reused the name.
            my $inode = (lstat $path)[1];
            note(defined $inode ? "+ $attempt $path $inode" : "+ $attempt $path");
        }
        $! = $error;
        return $made;
    };
}
1;
"#;

/// U269: the supervisor roots a loopback child's real worker created, retired as production retires one.
#[cfg(target_os = "macos")]
struct SupervisorRoots {
    dir: std::path::PathBuf,
    /// The ledger's lock, held exclusively from the start of the retirement on: no root can be created after it.
    closed: Option<std::fs::File>,
    retiring: bool,
}
#[cfg(target_os = "macos")]
impl SupervisorRoots {
    /// One hang guard for the whole retirement.
    const HANG_GUARD: std::time::Duration = std::time::Duration::from_secs(60);

    fn new(dir: &Path) -> std::io::Result<Self> {
        std::fs::write(dir.join("U269Ledger.pm"), ROOT_LEDGER_MODULE)?;
        std::fs::write(Self::lock(dir), "")?;
        Ok(Self {
            dir: dir.to_owned(),
            closed: None,
            retiring: false,
        })
    }
    fn ledger(dir: &Path) -> std::path::PathBuf {
        dir.join("supervisor-roots")
    }
    fn lock(dir: &Path) -> std::path::PathBuf {
        dir.join("supervisor-roots.lock")
    }
    /// The child's sshd configuration lines that load the module into its sessions' Perl.
    fn sshd_environment(dir: &Path) -> String {
        format!(
            "SetEnv PERL5OPT=\"-I{} -MU269Ledger\" MRS_U269_LEDGER=\"{}\" MRS_U269_LOCK=\"{}\"\n",
            dir.display(),
            Self::ledger(dir).display(),
            Self::lock(dir).display()
        )
    }
    /// The roots the ledger holds, each with the inode it was created with when the record has one: one record per
    /// successful creation, keyed by its attempt id (a `-` line, which earlier rounds wrote for a failed attempt,
    /// still retracts only its own record).
    fn recorded(ledger: &str) -> std::io::Result<std::collections::BTreeMap<String, Option<u64>>> {
        let mut attempts = std::collections::BTreeMap::new();
        let invalid = |line: &str| std::io::Error::other(format!("not a ledger line: {line:?}"));
        for line in ledger.lines() {
            match line.split(' ').collect::<Vec<_>>()[..] {
                ["+", attempt, root] if attempts.insert(attempt.to_owned(), (root.to_owned(), None)).is_none() => {}
                ["+", attempt, root, inode] => {
                    let inode = inode.parse::<u64>().map_err(|_| invalid(line))?;
                    if attempts
                        .insert(attempt.to_owned(), (root.to_owned(), Some(inode)))
                        .is_some()
                    {
                        return Err(invalid(line));
                    }
                }
                ["-", attempt, root] if attempts.remove(attempt).is_some_and(|(recorded, _)| recorded == root) => {}
                _ => return Err(invalid(line)),
            }
        }
        Ok(attempts.into_values().collect())
    }
    /// Runs `body` on its own thread and gives up at `deadline`.
    fn bounded<T: Send + 'static>(
        deadline: std::time::Instant,
        what: &str,
        body: impl FnOnce() -> std::io::Result<T> + Send + 'static,
    ) -> std::io::Result<T> {
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(body());
        });
        result
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .map_err(|_| std::io::Error::other(format!("{what}: still waiting after the hang guard")))?
    }
    fn flock(deadline: std::time::Instant, what: &str, file: std::fs::File) -> std::io::Result<std::fs::File> {
        Self::bounded(deadline, what, move || {
            rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)?;
            Ok(file)
        })
    }
    /// Closes the ledger, then retires every recorded root that exists: waits for the exclusive lock on its
    /// `supervisor.pl` that the worker's operations hold shared while they run (the abandoned commands' owners outlive
    /// the child by a moment), then runs the worker's own `retire`, which takes that lock again and removes the root.
    /// Every wait is under one hang guard, and it runs once: after a failure `Drop` does not retry.
    fn retire(&mut self) -> std::io::Result<Vec<std::path::PathBuf>> {
        self.retiring = true;
        let deadline = std::time::Instant::now() + Self::HANG_GUARD;
        let lock = std::fs::File::open(Self::lock(&self.dir))?;
        self.closed = Some(Self::flock(deadline, "the root ledger", lock)?);
        let ledger = match std::fs::read_to_string(Self::ledger(&self.dir)) {
            Ok(ledger) => ledger,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        let recorded = Self::recorded(&ledger)?;
        let mut roots = Vec::new();
        for (root, inode) in &recorded {
            let valid = root.strip_prefix("/tmp/marathon-supervisor-").is_some_and(|suffix| {
                suffix.len() == 10
                    && suffix
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            });
            if !valid {
                return Err(std::io::Error::other(format!("not a supervisor root: {root:?}")));
            }
            // Only a directory that still holds the inode it was created with is this test's: a record without one
            // (its `lstat` failed) is never acted on, and a name that now holds another directory (removed, then
            // reused by another process) is skipped. The identity is checked again right before each removal, after
            // any lock wait.
            let Some(inode) = *inode else { continue };
            let ours = || -> std::io::Result<Option<bool>> {
                use std::os::unix::fs::MetadataExt;
                match std::fs::symlink_metadata(root) {
                    Ok(meta) => Ok(Some(meta.ino() == inode)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(error),
                }
            };
            match ours()? {
                Some(true) => {}
                Some(false) => continue,
                None => {
                    roots.push(root.clone());
                    continue;
                }
            }
            roots.push(root.clone());
            let script = format!("{root}/supervisor.pl");
            loop {
                let pinned = match std::fs::File::open(&script) {
                    Ok(pinned) => pinned,
                    // Retired already (the child got as far as closing its connection), or never installed: the
                    // bootstrap removes its root when it cannot install, and a bootstrap killed before that leaves the
                    // empty directory.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => match ours()? {
                        Some(true) => match std::fs::remove_dir(root) {
                            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
                            _ => break,
                        },
                        _ => break,
                    },
                    Err(error) => return Err(error),
                };
                drop(Self::flock(deadline, root, pinned)?);
                if ours()? != Some(true) {
                    break;
                }
                let child = std::process::Command::new("/usr/bin/perl")
                    .args([&script, "retire", root, "60000"])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()?;
                let pid = nix::unistd::Pid::from_raw(i32::try_from(child.id()).map_err(std::io::Error::other)?);
                let output = Self::bounded(deadline, root, move || child.wait_with_output()).inspect_err(|_| {
                    let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
                })?;
                match (output.status.success(), output.stdout.as_slice()) {
                    (true, b"MARATHON_D96_1 retired\n") => break,
                    // An operation started between the lock and the retirement; wait for it as well.
                    (true, b"MARATHON_D96_1 busy\n") if std::time::Instant::now() < deadline => {}
                    _ => return Err(std::io::Error::other(format!("{root}: {output:?}"))),
                }
            }
        }
        Ok(roots.into_iter().map(std::path::PathBuf::from).collect())
    }
}
/// U269: a failed attempt retracts only its own record. Two attempts at one name, the second failing because the first
/// took it, used to retract both and leave the first's root unretired.
#[cfg(target_os = "macos")]
#[test]
fn a_failed_root_attempt_keeps_another_attempts_root_u269() {
    let x = "/tmp/marathon-supervisor-AAAAAAAAAA";
    let y = "/tmp/marathon-supervisor-BBBBBBBBBB";
    let recorded = |ledger: &str| {
        SupervisorRoots::recorded(ledger)
            .unwrap()
            .into_keys()
            .collect::<Vec<_>>()
    };
    assert_eq!(recorded(&format!("+ 10.1 {x}\n+ 11.1 {x}\n- 11.1 {x}\n")), [x]);
    assert_eq!(recorded(&format!("+ 10.1 {x}\n- 10.1 {x}\n+ 10.2 {y}\n")), [y]);
    assert_eq!(recorded(&format!("+ 10.1 {x}\n+ 10.2 {y}\n")), [x, y]);
    assert_eq!(recorded(""), Vec::<String>::new());
    // A record carries the root's inode; a malformed one is refused.
    assert_eq!(
        SupervisorRoots::recorded(&format!("+ 10.1 {x} 42\n")).unwrap(),
        std::collections::BTreeMap::from([(x.to_owned(), Some(42))])
    );
    assert!(SupervisorRoots::recorded(&format!("+ 10.1 {x} inode\n")).is_err());
    assert!(SupervisorRoots::recorded(&format!("+ 10.1 {x} 42\n+ 10.1 {y} 43\n")).is_err());
    // A retraction must match its own attempt, and an attempt is recorded once.
    for ledger in [
        format!("- 10.1 {x}\n"),
        format!("+ 10.1 {x}\n- 10.2 {x}\n"),
        format!("+ 10.1 {x}\n- 10.1 {y}\n"),
        format!("+ 10.1 {x}\n+ 10.1 {y}\n"),
        format!("+ {x}\n"),
    ] {
        assert!(SupervisorRoots::recorded(&ledger).is_err(), "{ledger:?}");
    }
}

#[cfg(target_os = "macos")]
impl Drop for SupervisorRoots {
    fn drop(&mut self) {
        if !self.retiring {
            let _ = self.retire();
        }
    }
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread")]
async fn real_worker_commands_and_senders_child() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("MRS_D96_REAL_WORKER").is_none() {
        return Ok(());
    }
    use marathon_apple::cmd::{
        FileBridge, TransferCleanup, TransferReceipt, TransferStop,
        remote::{
            rsync::{RsyncFileBridge, RsyncTarget},
            ssh::sshj::SshAuthentication,
        },
    };
    use marathon_config::vendor::{RsyncConfiguration, SshAuthentication as Auth, SshConfiguration};
    use marathon_core::executor::shared::Signal;
    use std::process::Stdio;
    if !Path::new("/usr/sbin/sshd").is_file() || !Path::new("/usr/bin/perl").is_file() {
        eprintln!("loopback witness requires stock sshd and Perl");
        return Ok(());
    }
    let server = tempfile::tempdir()?;
    let host = server.path().join("host");
    let key = server.path().join("client");
    for path in [&host, &key] {
        let status = tokio::process::Command::new("/usr/bin/ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "fixture", "-f"])
            .arg(path)
            .status()
            .await?;
        assert!(status.success());
    }
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    // U269: the parent test retires the supervisor roots this sshd's sessions record.
    let roots = std::env::var_os("MRS_D96_ROOT_DIR")
        .map(|dir| SupervisorRoots::sshd_environment(Path::new(&dir)))
        .unwrap_or_default();
    let config = server.path().join("sshd_config");
    std::fs::write(
        &config,
        format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nAuthorizedKeysFile {}\nPidFile {}\nUsePAM no\nStrictModes no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nMaxSessions 1\n{roots}",
            host.display(),
            key.with_extension("pub").display(),
            server.path().join("pid").display(),
        ),
    )?;
    let mut daemon = tokio::process::Command::new("/usr/sbin/sshd")
        .args(["-D", "-e", "-f"])
        .arg(config)
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let output = tokio::process::Command::new("/usr/bin/id").arg("-un").output().await?;
    assert!(output.status.success());
    let user = String::from_utf8(output.stdout)?.trim().to_string();
    let (logging, events) = ssh::logging(false);
    let mut factory = ssh::factory(&logging);
    factory.connection_attempts = 1;
    factory.keep_alive_interval = duration(1000);
    factory.keep_alive_max_count = 1;
    let executor = tokio::time::timeout(std::time::Duration::from_secs(6), async {
        loop {
            if let Some(status) = daemon.try_wait()? {
                return Err::<_, Box<dyn std::error::Error>>(format!("fixture SSH daemon exited with {status}").into());
            }
            match factory
                .connect_worker(
                    "127.0.0.1",
                    port,
                    SshAuthentication::PublicKey {
                        username: user.clone(),
                        key: key.clone(),
                    },
                    None,
                    false,
                    Arc::default(),
                )
                .await
            {
                Ok(executor) => return Ok(executor),
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
    })
    .await??;
    assert!(
        executor
            .worker_supervised()
            .await
            .map_err(|e| e.failure().to_string())?
    );
    if std::env::var_os("MRS_D96_SSH_BENCH").is_some() {
        let authentication = SshAuthentication::PublicKey { username: user, key };
        let plain = factory
            .connect("127.0.0.1", port, authentication, None, false)
            .await
            .map_err(|error| error.into_vendor().failure().to_string())?;
        for (label, command) in [
            ("true", vec!["/usr/bin/true"]),
            ("sleep 0.3", vec!["/bin/sleep", "0.3"]),
        ] {
            for (mode, commands) in [("plain", &plain), ("supervised", &executor)] {
                let request = CommandRequest::new(command.clone(), duration(15_000));
                for _ in 0..3 {
                    commands
                        .critical_execute(request.clone(), context())
                        .await
                        .map_err(|error| error.failure().to_string())?;
                }
                let mut elapsed = Vec::new();
                for _ in 0..30 {
                    let start = std::time::Instant::now();
                    commands
                        .critical_execute(request.clone(), context())
                        .await
                        .map_err(|error| error.failure().to_string())?;
                    elapsed.push(start.elapsed().as_secs_f64() * 1_000.0);
                }
                elapsed.sort_by(f64::total_cmp);
                println!(
                    "{label} {mode}: median {:.2} ms, mean {:.2} ms (n={})",
                    (elapsed[14] + elapsed[15]) / 2.0,
                    elapsed.iter().sum::<f64>() / elapsed.len() as f64,
                    elapsed.len()
                );
            }
        }
        for (mode, commands) in [("plain", &plain), ("supervised", &executor)] {
            let closing = std::time::Instant::now();
            commands.close().await.map_err(|error| error.failure().to_string())?;
            println!("{mode} close: {:.2} ms", closing.elapsed().as_secs_f64() * 1_000.0);
        }
        daemon.kill().await?;
        return Ok(());
    }
    let context = context();
    let healthy = executor
        .execute(
            CommandRequest::new(["/bin/sh", "-c", "exit 23"], duration(15000)),
            context.clone(),
        )
        .await
        .map_err(|e| e.failure().to_string())?;
    let ended = healthy
        .await_result(&context)
        .await
        .map_err(|e| e.failure().to_string())?;
    assert_eq!(ended.exit_code, Some(23));

    healthy.close().await.map_err(|e| e.failure().to_string())?;
    let executor = Arc::new(executor);
    let local = server.path().join("local");
    let remote = server.path().join("remote");
    std::fs::write(&remote, b"rsync fixture")?;
    let bridge = |remote_path: String| {
        RsyncFileBridge::new(
            RsyncTarget {
                addr: "127.0.0.1".into(),
                port: i32::from(port),
            },
            false,
            SshConfiguration {
                authentication: None,
                known_hosts_path: None,
                keep_alive_interval: duration(1000),
                keep_alive_max_count: 1,
                debug: false,
                share_worker_connection: false,
            },
            RsyncConfiguration { remote_path },
            Some(Auth::PublicKey {
                username: user.clone(),
                key: key.clone(),
            }),
            Arc::new(support::Runtime),
            Arc::new(support::Clock::default()),
            logging.clone(),
        )
    };
    let receiving = bridge("/usr/bin/rsync".into())
        .await
        .map_err(|e| e.failure().to_string())?;
    let receipt = TransferReceipt::new();
    let received = receiving
        .receive_with(
            remote.to_str().ok_or("non-UTF8 test source")?,
            &local,
            TransferStop::never()
                .on_worker(executor.clone())
                .owned_by(receipt.clone()),
        )
        .await
        .map_err(|e| e.failure().to_string())?;
    assert!(
        received,
        "rsync stderr and receipt: {:?}; {:?}",
        ssh::lines(&events),
        receipt.ended()
    );
    assert_eq!(std::fs::read(&local)?, b"rsync fixture");
    assert_eq!(receipt.ended(), Some(TransferCleanup::Confirmed));

    let failing_sender = server.path().join("failing-sender");
    std::fs::write(&failing_sender, "/usr/bin/rsync \"$@\"\nexit 125\n")?;
    let receiving = bridge(format!("/bin/sh {}", failing_sender.display()))
        .await
        .map_err(|e| e.failure().to_string())?;
    let undelivered = server.path().join("undelivered");
    let received = receiving
        .receive_with(
            remote.to_str().ok_or("non-UTF8 test source")?,
            &undelivered,
            TransferStop::never().on_worker(executor.clone()),
        )
        .await
        .map_err(|e| e.failure().to_string())?;
    assert!(!received);
    assert!(!undelivered.exists(), "payload bytes alone do not prove sender success");

    let sender = server.path().join("sender");
    let marker = server.path().join("sender-pid");
    std::fs::write(
        &sender,
        format!(
            "trap '' HUP TERM\nprintf '%s\\n' \"$$\" > \"{}\"\nexec /bin/sleep 60\n",
            marker.display()
        ),
    )?;
    let waiting = bridge(format!("/bin/sh {}", sender.display()))
        .await
        .map_err(|e| e.failure().to_string())?;
    let signal = Signal::new();
    let receipt = TransferReceipt::new();
    let pull = {
        let stop = TransferStop::new(signal.clone())
            .on_worker(executor.clone())
            .owned_by(receipt.clone());
        tokio::spawn(async move { waiting.receive_with(&remote.to_string_lossy(), &local, stop).await })
    };
    let pid = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            if let Ok(text) = tokio::fs::read_to_string(&marker).await
                && let Ok(pid) = text.trim().parse::<i32>()
            {
                break nix::unistd::Pid::from_raw(pid);
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    signal.cancel(marathon_apple::cmd::cancelled("fixture stopped"));
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(8), pull)
            .await??
            .is_err()
    );
    assert_eq!(receipt.ended(), Some(TransferCleanup::Confirmed));
    assert_eq!(nix::sys::signal::kill(pid, None), Err(nix::errno::Errno::ESRCH));

    let command = executor
        .execute(
            CommandRequest::new(["/bin/sh", "-c", "echo $$; exec /bin/sleep 60"], duration(30000)),
            context,
        )
        .await
        .map_err(|e| e.failure().to_string())?;
    let line = tokio::time::timeout(std::time::Duration::from_secs(6), command.base.stdout.receive())
        .await?
        .map_err(|e| e.failure().to_string())?
        .ok_or("Missing worker PID")?;
    let pid = nix::unistd::Pid::from_raw(line.parse()?);
    executor.abandon();
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        loop {
            if nix::sys::signal::kill(pid, None) == Err(nix::errno::Errno::ESRCH) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await?;
    daemon.kill().await?;
    Ok(())
}

/// The test code for each `# x111-test-hook: <name>` line of the worker script, in the order of the script. The tests'
/// copy of the script ([`x111_root`]) replaces each such line by its code, indented like it; nothing else differs.
/// Hook `<name>` is set by the environment variable `MARATHON_D96_TEST_<NAME>`:
///
/// - `wakes=<file>`: the owner appends one line to the file when its loop ends (before its receipt), one letter per
///   pass by what ended that pass's sleep (`e` a pipe, `w` the wake FIFO, `d` a deadline, `b` a busy retry, `p` the
///   longest sleep), and `!` where another hook moved or stopped it.
/// - `fallback_ms=<ms>`: the longest sleep once the wake FIFO is open (100 ms in the shipped script), so a witness can
///   tell a wake from a poll.
/// - `kill_after_commit`: a control request kills itself right after its commit, still holding the token lock.
/// - `parked=<file>`: on its first pass with the FIFO open, the owner creates the file right before its `select`,
///   after it has read the state, so a witness can send a request strictly after that read.
/// - `expiry_in=<ms>`: on that pass, the owner's clock moves so that its lease expiry comes `<ms>` after the pass's
///   clock sample, as if the lease had been that short; with a long real lease, no real time decides whether the
///   owner reaches a hook before its expiry.
/// - `stop_at=sample|sleep` and `stop_file=<file>`: on that pass, right after its clock sample or right before its
///   `select`, the owner writes its pid and the ms left to its lease expiry to the file and stops itself with
///   `SIGSTOP` until the test continues it, as if descheduled there.
/// - `late_after_exit=<ms>`: after the first `select` that found the status pipe readable, the owner's clock moves
///   `<ms>` on, as if it had been descheduled that long right after seeing the exit.
const X111_HOOKS: &[(&str, &str)] = &[
    (
        "table",
        r##"my %hook = map { /\AMARATHON_D96_TEST_([A-Z_]+)\z/ ? (lc $1, $ENV{$_}) : () } keys %ENV;
my $skew = 0;
sub descheduled {
    my ($expiry) = @_;
    my $path = $hook{stop_file};
    open my $file, '>', "$path.new" or die "write stop file: $!";
    print {$file} "$$ ", $expiry - clock_ms(), "\n";
    close $file or die "close stop file: $!";
    rename "$path.new", $path or die "commit stop file: $!";
    kill 'STOP', $$;
}"##,
    ),
    ("clock", "$now += $skew;"),
    ("commit", "kill 'KILL', $$ if $notify && $hook{kill_after_commit};"),
    (
        "loop",
        "my $trace = defined $hook{wakes} ? '' : undef;\nmy ($passes_open, $late) = (0, 0);",
    ),
    ("pass", "my $first_open = $wake && !$passes_open++;"),
    (
        "poll",
        "$poll = $hook{fallback_ms} if $wake && defined $hook{fallback_ms};",
    ),
    (
        "sample",
        r##"if ($first_open && defined $hook{expiry_in}) {
    $skew += $expiry - $hook{expiry_in} - $now;
    $now = $expiry - $hook{expiry_in};
}
if ($first_open && ($hook{stop_at} // '') eq 'sample') {
    descheduled($expiry);
    $trace .= '!' if defined $trace;
}"##,
    ),
    ("term", "$term_at += $hook{kill_later} if $hook{kill_later};"),
    (
        "sleep",
        r##"if ($first_open && ($hook{stop_at} // '') eq 'sleep') {
    descheduled($expiry);
    $trace .= '!' if defined $trace;
}
if ($first_open && $hook{parked} && open my $file, '>', $hook{parked}) {
    close $file;
}"##,
    ),
    (
        "woken",
        r##"if ($hook{late_after_exit} && $found > 0 && vec($out, fileno($status), 1) && !$late++) {
    $skew += $hook{late_after_exit};
    $trace .= '!' if defined $trace;
}
$trace .= $found > 0 ? ($woken ? 'w' : 'e') : $cause if defined $trace;"##,
    ),
    (
        "end",
        r##"if (defined $trace && open my $file, '>>', $hook{wakes}) {
    print {$file} "$trace\n";
    close $file;
}"##,
    ),
];

/// The hook name of a `# x111-test-hook: <name>` line and its indentation, or `None` for any other line.
fn x111_marker(line: &str) -> Option<(&str, &str)> {
    let code = line.trim_start();
    let name = code.strip_prefix("# x111-test-hook: ")?;
    Some((&line[..line.len() - code.len()], name))
}

/// The worker script with each marker line replaced by its hook code from [`X111_HOOKS`].
fn x111_hooked(script: &str) -> String {
    let mut hooked = String::new();
    for line in script.lines() {
        match x111_marker(line) {
            Some((indent, name)) => {
                let (_, code) = X111_HOOKS
                    .iter()
                    .find(|(hook, _)| *hook == name)
                    .unwrap_or_else(|| panic!("unknown hook {name}"));
                for code in code.lines() {
                    hooked.push_str(indent);
                    hooked.push_str(code);
                    hooked.push('\n');
                }
            }
            None => {
                hooked.push_str(line);
                hooked.push('\n');
            }
        }
    }
    hooked
}

/// Why `script` is not a worker script with no test code of its own, or `None` if it is: every line that names a
/// hook is a comment, the markers are exactly the hooks of [`X111_HOOKS`], once each and in that order, and no test
/// variable is named. So the shipped script has no hook table, reader, sub, variable or call that any input could
/// reach or override.
fn x111_hook_violation(script: &str) -> Option<String> {
    if let Some(line) = script
        .lines()
        .find(|line| line.to_ascii_lowercase().contains("hook") && !line.trim_start().starts_with('#'))
    {
        return Some(format!("code names a hook: {line}"));
    }
    let markers: Vec<&str> = script.lines().filter_map(x111_marker).map(|(_, name)| name).collect();
    let hooks: Vec<&str> = X111_HOOKS.iter().map(|(name, _)| *name).collect();
    if markers != hooks {
        return Some(format!("markers {markers:?}, not {hooks:?}"));
    }
    if script.contains("MARATHON_D96_TEST") {
        return Some("a test variable is named".into());
    }
    None
}

/// Why `copy` differs from `script` other than by replacing each marker line by its hook code, or `None` if it
/// does not.
fn x111_copy_difference(script: &str, copy: &str) -> Option<String> {
    let mut copied = copy.lines();
    for (number, line) in script.lines().enumerate() {
        let expected: Vec<String> = match x111_marker(line) {
            Some((indent, name)) => {
                let Some((_, code)) = X111_HOOKS.iter().find(|(hook, _)| *hook == name) else {
                    return Some(format!("line {}: unknown hook {name}", number + 1));
                };
                code.lines().map(|code| format!("{indent}{code}")).collect()
            }
            None => vec![line.to_owned()],
        };
        for expected in expected {
            match copied.next() {
                Some(actual) if actual == expected => {}
                actual => return Some(format!("line {}: {actual:?}, not {expected:?}", number + 1)),
            }
        }
    }
    copied.next().map(|extra| format!("an extra line at the end: {extra}"))
}

/// A worker root like [`d98_root`] whose `supervisor.pl` is the worker script with its test hooks in (see
/// [`X111_HOOKS`]), checked to differ from the shipped script only at its marker lines.
fn x111_root() -> std::io::Result<(tempfile::TempDir, std::path::PathBuf)> {
    let (root, supervisor) = d98_root()?;
    let script = include_str!("../src/cmd/remote/ssh/sshj/supervisor/worker.pl");
    assert_eq!(x111_hook_violation(script), None);
    std::fs::write(&supervisor, x111_hooked(script))?;
    assert_eq!(
        x111_copy_difference(script, &std::fs::read_to_string(&supervisor)?),
        None
    );
    Ok((root, supervisor))
}

/// The shipped worker script has no test code: only inert comment markers where the tests' copy puts its hooks, so
/// no environment, argument, controller or added definition can turn a hook on or override one. The tests' copy
/// differs from it only at those markers, so the witnesses below run the shipped logic.
#[test]
fn the_shipped_worker_script_has_no_test_hooks_and_the_tests_run_its_logic_x111() {
    let script = include_str!("../src/cmd/remote/ssh/sshj/supervisor/worker.pl");
    assert_eq!(x111_hook_violation(script), None);
    let copy = x111_hooked(script);
    assert_eq!(x111_copy_difference(script, &copy), None);
    assert_ne!(copy, script);
    // Each of these, added to the shipped script, is test code the check must see.
    let added = [
        "sub main::hook { 1 }",
        "sub hook { 1 }",
        "my %hook;",
        "my %Hook = (wakes => 1);",
        "$hook{wakes} = 1;",
        "kill 'KILL', $$ if hook('kill_after_commit');",
        "*{'main::hook'} = sub { 1 };",
        "$ENV{MARATHON_D96_TEST_WAKES} = 1;",
        "# x111-test-hook: table",
        "# x111-test-hook: other",
    ];
    for added in added {
        let changed = script.replacen("\nsub readable {", &format!("\n{added}\nsub readable {{"), 1);
        assert_ne!(changed, script);
        assert!(x111_hook_violation(&changed).is_some(), "not seen: {added}");
    }
    let dropped = script.replacen("    # x111-test-hook: sleep\n", "", 1);
    assert_ne!(dropped, script);
    assert!(x111_hook_violation(&dropped).is_some(), "a missing marker");
    // And a copy that changes anything but the markers is not the shipped logic.
    let changed = [
        copy.replacen(
            "use constant LONGEST_SLEEP => 100;",
            "use constant LONGEST_SLEEP => 1000;",
            1,
        ),
        copy.replacen("\nsub readable {", "\nmy $x = 1;\nsub readable {", 1),
        copy.replacen("$now += $skew;", "$now += $skew + 1;", 1),
        format!("{copy}1;\n"),
        copy.replacen(
            "            # Opened after the fork",
            "            # Opened after a fork",
            1,
        ),
    ];
    for changed in changed {
        assert_ne!(changed, copy);
        assert!(x111_copy_difference(script, &changed).is_some());
    }
}

/// Starts `mode` (`run` or `detach`) of the hooked script of [`x111_root`] with its wake trace hook on: the owner
/// appends one letter per pass of its monitor loop to `trace`, by what woke it (`e` a pipe, `w` the wake FIFO, `d` a
/// deadline, `b` a busy retry, `p` the longest sleep), once it has ended and before its receipt. `env` sets further
/// test hooks.
fn x111_spawn(
    supervisor: &Path,
    root: &Path,
    lease: &str,
    mode: &str,
    args: &[&str],
    trace: &Path,
    env: &[(&str, &str)],
) -> std::io::Result<tokio::process::Child> {
    let mut command = tokio::process::Command::new("/usr/bin/perl");
    command
        .arg(supervisor)
        .arg(mode)
        .arg(root)
        .arg(lease)
        .args(args)
        .env("MARATHON_D96_TEST_WAKES", trace)
        .envs(env.iter().copied())
        .kill_on_drop(true);
    command.spawn()
}

/// The traces the owners wrote, one line per supervised command.
fn x111_traces(trace: &Path) -> std::io::Result<Vec<String>> {
    Ok(std::fs::read_to_string(trace)?.lines().map(str::to_owned).collect())
}

/// A `run` payload that writes `int` and exits 3 on INT gets INT as the only control request, sent once the payload
/// has installed its INT handler (it then creates `handler`) and its owner has read the state on its first pass with
/// the wake FIFO open and is about to sleep (the `parked` hook). With `kill_after_commit`, the control process is
/// killed right after its commit, while it still holds the token lock. The owner's longest sleep is 120 s and its
/// lease 600 s, so nothing but the request's wake can make it read the state again within the 60 s hang guard: INT
/// arriving at all shows the wake.
async fn x111_stop_witness(kill_after_commit: bool) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::process::ExitStatusExt;
    use std::time::{Duration, Instant};
    let (root, supervisor) = x111_root()?;
    let files = tempfile::tempdir()?;
    let trace = files.path().join("trace");
    let (parked, int) = (files.path().join("parked"), files.path().join("int"));
    let handler = files.path().join("handler");
    let token = "0123456789abcdef0123456789abcdef";
    let payload = r#"my ($int, $handler) = @ARGV; $SIG{INT} = sub { open my $f, '>', $int; close $f; exit 3 };
open my $f, '>', $handler or die $!; close $f; while (1) { select undef, undef, undef, .01 }"#;
    let owner = x111_spawn(
        &supervisor,
        root.path(),
        "600000",
        "run",
        &[
            token,
            "/usr/bin/perl",
            "-e",
            payload,
            int.to_str().unwrap(),
            handler.to_str().unwrap(),
        ],
        &trace,
        &[
            ("MARATHON_D96_TEST_FALLBACK_MS", "120000"),
            ("MARATHON_D96_TEST_PARKED", parked.to_str().unwrap()),
        ],
    )?;
    d98_file(&handler).await?;
    d98_file(&parked).await?;
    let requested = Instant::now();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let mut control = tokio::process::Command::new("/usr/bin/perl");
            control
                .arg(&supervisor)
                .arg("control")
                .arg(root.path())
                .arg("600000")
                .arg(format!("{token}:1"))
                .kill_on_drop(true);
            if kill_after_commit {
                control.env("MARATHON_D96_TEST_KILL_AFTER_COMMIT", "1");
            }
            let output = control.output().await?;
            let reply = String::from_utf8_lossy(&output.stdout).trim_end().to_owned();
            // A busy token lock: nothing was committed (and nobody killed); ask again.
            if reply.ends_with(" busy") {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
            if kill_after_commit {
                assert_eq!(output.status.signal(), Some(9), "{output:?}");
            } else {
                assert!(output.status.success(), "{output:?}");
                assert!(reply.starts_with(&format!("{token} running:")), "{reply}");
            }
            return Ok::<_, std::io::Error>(());
        }
    })
    .await??;
    d98_file(&int).await?;
    println!("INT {:?} after the request", requested.elapsed());
    let output = tokio::time::timeout(Duration::from_secs(60), owner.wait_with_output()).await??;
    assert_eq!(output.status.code(), Some(3));
    let (receipt, _) = d98_receipt(&supervisor, root.path(), token).await?;
    assert_eq!(receipt, "done:3");
    let traces = x111_traces(&trace)?;
    let [trace] = &traces[..] else { panic!("{traces:?}") };
    println!("trace {trace}");
    // The owner was parked before the request, so the request's one byte is what woke it.
    assert_eq!(trace.matches('w').count(), 1, "{trace}");
    assert!(std::fs::symlink_metadata(root.path().join(format!("{token}.wake"))).is_err());
    assert_eq!(d98_retire(&supervisor, root.path()).await?, "MARATHON_D96_1 retired");
    Ok(())
}

#[tokio::test]
async fn a_stop_request_wakes_the_worker_supervisor_through_its_fifo_x111() -> Result<(), Box<dyn std::error::Error>> {
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    x111_stop_witness(false).await
}

#[tokio::test]
async fn a_stop_request_killed_right_after_its_commit_still_wakes_the_worker_supervisor_x111()
-> Result<(), Box<dyn std::error::Error>> {
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    x111_stop_witness(true).await
}

#[tokio::test]
async fn an_idle_supervised_command_and_recorder_wake_rarely_x111() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    let (root, supervisor) = x111_root()?;
    let files = tempfile::tempdir()?;
    let trace = files.path().join("trace");
    let (command, recorder) = ("00000000000000000000000000000001", "00000000000000000000000000000002");
    let (ready, release) = (files.path().join("ready"), files.path().join("release"));
    // Both stay idle until the test releases them: the command polls for a file, the recorder (started, so its
    // stderr pipe is watched) waits for INT.
    let started = Instant::now();
    let owner = x111_spawn(
        &supervisor,
        root.path(),
        "60000",
        "run",
        &[
            command,
            "/usr/bin/perl",
            "-e",
            "select undef, undef, undef, .01 until -f $ARGV[0]",
            release.to_str().unwrap(),
        ],
        &trace,
        &[],
    )?;
    let payload = r#"$SIG{INT} = sub { exit 0 }; print STDERR "Recording started\n";
open my $f, '>', $ARGV[0] or die $!; close $f; while (1) { select undef, undef, undef, .01 }"#;
    let detached = x111_spawn(
        &supervisor,
        root.path(),
        "60000",
        "detach",
        &[recorder, "/usr/bin/perl", "-e", payload, ready.to_str().unwrap()],
        &trace,
        &[],
    )?;
    let output = tokio::time::timeout(Duration::from_secs(60), detached.wait_with_output()).await??;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    d98_file(&ready).await?;
    // Wait until the recorder's start is published, so its loop is idle from here on.
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let status = d98_control(&supervisor, root.path(), &format!("{recorder}:status")).await?;
            if status.starts_with(&format!("{recorder} recording:")) {
                return Ok::<_, std::io::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    // An idle window long enough for the old 25 ms loop to pass a hundred times.
    tokio::time::sleep(Duration::from_secs(3)).await;
    std::fs::write(&release, b"")?;
    let output = tokio::time::timeout(Duration::from_secs(60), owner.wait_with_output()).await??;
    assert!(output.status.success());
    let reply = d98_control(&supervisor, root.path(), &format!("{recorder}:1")).await?;
    assert!(reply.starts_with(&format!("{recorder} recording:")), "{reply}");
    let (receipt, _) = d98_receipt(&supervisor, root.path(), recorder).await?;
    assert_eq!(receipt, "done:0");
    let elapsed = started.elapsed();
    let traces = x111_traces(&trace)?;
    assert_eq!(traces.len(), 2, "{traces:?}");
    // The old cadence for the first 250 ms (at most 10 passes), then about one pass per 100 ms longest sleep, plus the
    // few events of a start and a stop. The old loop passed 40 times a second when idle, so it exceeds this, about
    // twice over, for any window of a second or more.
    let ceiling = 15 * elapsed.as_secs() as usize + 20;
    for trace in &traces {
        println!("{} passes in {elapsed:?}: {trace}", trace.len());
        assert!(trace.len() <= ceiling, "{} passes in {elapsed:?}: {trace}", trace.len());
    }
    assert_eq!(d98_retire(&supervisor, root.path()).await?, "MARATHON_D96_1 retired");
    Ok(())
}

#[test]
fn the_worker_supervisor_sleeps_until_its_next_deadline_x111() -> Result<(), Box<dyn std::error::Error>> {
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    // A controlled clock: each case gives the owner's `now` (ms) and its pending times, as `next_wake` takes them
    // (now, lease expiry, first stop request, TERM sent, stopping, forced, busy retry or 0, longest sleep; `-` for
    // none),
    // and the delay it must sleep and what it waits for.
    let cases = [
        (
            "1000 1050 - - 0 0 0 100",
            "50 d",
            "the lease expiry is due before the longest sleep ends",
        ),
        ("1000 3000 - - 0 0 0 100", "100 p", "the longest sleep ends first"),
        ("1000 1100 - - 0 0 0 100", "100 d", "a deadline wins a tie"),
        (
            "1000 1000 - - 0 1 0 25",
            "25 p",
            "an expired lease the pass has acted on (forced) is not waited for",
        ),
        (
            "1000 1000 - - 0 0 0 120000",
            "10 d",
            "a lease expiring exactly now that the pass has not acted on gets the floor, never the longest sleep",
        ),
        (
            "1050 1000 - - 0 0 0 120000",
            "10 d",
            "a lease expired 50 ms ago that the pass has not acted on gets the floor, never the longest sleep",
        ),
        (
            "1000 1000 - - 0 0 1 120000",
            "1 d",
            "a due deadline with a 1 ms busy retry pending sleeps that 1 ms, not the 10 ms floor",
        ),
        (
            "3000 60000 1000 - 1 0 0 120000",
            "10 d",
            "a TERM deadline exactly now gets the floor",
        ),
        (
            "4050 60000 1000 3000 1 1 0 120000",
            "10 d",
            "a KILL deadline 50 ms past gets the floor",
        ),
        ("1000 1004 - - 0 0 0 25", "10 d", "never below 10 ms"),
        (
            "1000 61000 1000 - 1 0 0 100",
            "100 p",
            "a deadline further than the longest sleep waits for the next pass",
        ),
        ("1000 9000 - - 0 0 10 100", "10 b", "a busy lock is retried in 10 ms"),
        (
            "1000 9000 - - 0 0 1 100",
            "1 b",
            "the first busy retry after a wake comes after 1 ms",
        ),
        (
            "1000 1004 - - 0 0 1 100",
            "1 b",
            "the 10 ms floor does not delay that retry",
        ),
        (
            "1000 1300 - - 0 0 0 120000",
            "300 d",
            "the deadline holds whatever the longest sleep (a test hook lengthens it)",
        ),
        (
            "2950 60000 1000 - 1 0 0 100",
            "50 d",
            "TERM 2 s after the first stop request",
        ),
        (
            "2500 60000 1000 - 0 0 0 100",
            "100 p",
            "an INT request alone has no TERM deadline",
        ),
        (
            "3930 60000 1000 3000 1 1 0 100",
            "70 d",
            "KILL 1 s after TERM when forced",
        ),
        (
            "3700 60000 1000 3000 1 0 0 100",
            "100 p",
            "no KILL deadline without force",
        ),
        ("2920 2960 1000 - 1 0 0 100", "40 d", "the nearest of several deadlines"),
        (
            "3930 3950 1000 3000 1 1 0 100",
            "70 d",
            "a forced recorder no longer waits for its lease expiry",
        ),
    ];
    let worker = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cmd/remote/ssh/sshj/supervisor/worker.pl");
    let program = r#"my $worker = shift; do $worker or die "load worker: ", $@ || $!;
print main::LONGEST_SLEEP(), "\n";
for (@ARGV) { print join(' ', next_wake(map { $_ eq '-' ? undef : $_ } split / /)), "\n" }"#;
    let output = std::process::Command::new("/usr/bin/perl")
        .arg("-e")
        .arg(program)
        .arg(&worker)
        .args(cases.iter().map(|(case, _, _)| case))
        .output()?;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8(output.stdout)?;
    let mut chosen = stdout.lines();
    // An owner whose wake FIFO is open sleeps at most 100 ms: after being descheduled anywhere in a pass, it acts on
    // a deadline that passed meanwhile within 100 ms of resuming.
    assert_eq!(chosen.next(), Some("100"), "the longest sleep");
    let chosen: Vec<&str> = chosen.collect();
    assert_eq!(chosen.len(), cases.len(), "{stdout}");
    for ((case, expected, why), chosen) in cases.iter().zip(chosen) {
        assert_eq!(chosen, *expected, "{why}: {case}");
    }
    Ok(())
}

/// A payload's exit seen by the owner's sleep, followed by the owner being descheduled past a deadline before its
/// next pass (the `late_after_exit` hook moves its clock 700 s on, past the 600 s lease), keeps its own status: the
/// next pass finds the reported exit before its clock sample. A `run` command whose payload exits 0 must end
/// `done:0`, not be killed for the expiry it missed (`done:137`); a forced recorder that exits 0 on TERM must end
/// `done:0`, not `done:137` for its KILL deadline after TERM. Each payload ends only once its owner sleeps: the
/// command when the test opens its gate after the owner parked, the recorder on TERM, which the owner sends in the
/// pass before its sleep (its stderr is closed at once, so only its exit can wake that sleep). The `kill_later` hook
/// moves the recorder's KILL deadline 300 s past its real 1 s, beyond the 60 s hang guard, so however late its exit
/// after TERM is reported, nothing but that exit can end the recorder before the clock moves past the deadline.
#[tokio::test]
async fn an_exit_seen_before_a_missed_deadline_keeps_its_own_status_x111() -> Result<(), Box<dyn std::error::Error>> {
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    let (root, supervisor) = x111_root()?;
    let files = tempfile::tempdir()?;
    let trace = files.path().join("trace");
    let (parked, gate) = (files.path().join("parked"), files.path().join("gate"));
    let hooks = [
        ("MARATHON_D96_TEST_FALLBACK_MS", "120000"),
        ("MARATHON_D96_TEST_LATE_AFTER_EXIT", "700000"),
        ("MARATHON_D96_TEST_PARKED", parked.to_str().unwrap()),
        ("MARATHON_D96_TEST_KILL_LATER", "300000"),
    ];
    let command = "00000000000000000000000000000001";
    let owner = x111_spawn(
        &supervisor,
        root.path(),
        "600000",
        "run",
        &[
            command,
            "/usr/bin/perl",
            "-e",
            "select undef, undef, undef, .01 until -e $ARGV[0]; exit 0",
            gate.to_str().unwrap(),
        ],
        &trace,
        &hooks[..3],
    )?;
    d98_file(&parked).await?;
    std::fs::write(&gate, b"")?;
    let output = tokio::time::timeout(std::time::Duration::from_secs(60), owner.wait_with_output()).await??;
    assert_eq!(output.status.code(), Some(0));
    let (receipt, _) = d98_receipt(&supervisor, root.path(), command).await?;
    assert_eq!(receipt, "done:0");
    let recorder = "00000000000000000000000000000002";
    let ready = files.path().join("ready");
    let payload = r#"close STDERR; $SIG{TERM} = sub { exit 0 }; open my $f, '>', $ARGV[0] or die $!; close $f;
while (1) { select undef, undef, undef, .01 }"#;
    let detached = x111_spawn(
        &supervisor,
        root.path(),
        "600000",
        "detach",
        &[recorder, "/usr/bin/perl", "-e", payload, ready.to_str().unwrap()],
        &trace,
        &[hooks[0], hooks[1], hooks[3]],
    )?;
    let output = tokio::time::timeout(std::time::Duration::from_secs(60), detached.wait_with_output()).await??;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    d98_file(&ready).await?;
    // KILL: an unconfirmed recorder gets TERM 2 s later and, if still running, KILL 1 s after that (here 301 s).
    let reply = d98_control(&supervisor, root.path(), &format!("{recorder}:3")).await?;
    assert!(reply.starts_with(&format!("{recorder} running:")), "{reply}");
    let (receipt, _) = d98_receipt(&supervisor, root.path(), recorder).await?;
    assert_eq!(receipt, "done:0");
    let traces = x111_traces(&trace)?;
    assert_eq!(traces.len(), 2, "{traces:?}");
    for trace in &traces {
        println!("trace {trace}");
        assert_eq!(trace.matches('!').count(), 1, "the hook moved the clock once: {trace}");
    }
    assert_eq!(d98_retire(&supervisor, root.path()).await?, "MARATHON_D96_1 retired");
    Ok(())
}

/// The owner is descheduled at a risky point of its first pass with the wake FIFO open, whose lease expiry the
/// `expiry_in` hook puts 5 s after that pass's clock sample: right after the sample, so the pass decides and chooses
/// its sleep from a stale sample (`sample`), or after it chose its sleep, right before `select` (`sleep`). It stops
/// itself with SIGSTOP until the test continues it past the expiry. It must act on the expiry within its longest
/// sleep (100 ms) of resuming, not sleep the stale 5 s its sample put before the expiry: a `run` command ends
/// `done:137` well within 2.5 s of SIGCONT, which separates the two. The 100 ms itself is pinned by
/// `the_worker_supervisor_sleeps_until_its_next_deadline_x111`, not by a real-time bound.
async fn x111_descheduled_witness(point: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};
    let (root, supervisor) = x111_root()?;
    let files = tempfile::tempdir()?;
    let (trace, stop) = (files.path().join("trace"), files.path().join("stop"));
    let command = "00000000000000000000000000000001";
    let owner = x111_spawn(
        &supervisor,
        root.path(),
        "600000",
        "run",
        &[command, "/bin/sleep", "600"],
        &trace,
        &[
            ("MARATHON_D96_TEST_EXPIRY_IN", "5000"),
            ("MARATHON_D96_TEST_STOP_AT", point),
            ("MARATHON_D96_TEST_STOP_FILE", stop.to_str().unwrap()),
        ],
    )?;
    d98_file(&stop).await?;
    let told = std::fs::read_to_string(&stop)?;
    let [pid, left] = told.split_whitespace().collect::<Vec<_>>()[..] else {
        panic!("{told}")
    };
    let (pid, left): (i32, u64) = (pid.parse()?, left.parse()?);
    // Continue it only once it is stopped, and only after its expiry.
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let state = tokio::process::Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .await?;
            if String::from_utf8_lossy(&state.stdout).trim_start().starts_with('T') {
                return Ok::<_, std::io::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    tokio::time::sleep(Duration::from_millis(left + 300)).await;
    let continued = Instant::now();
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGCONT)?;
    let output = tokio::time::timeout(Duration::from_secs(60), owner.wait_with_output()).await??;
    let late = continued.elapsed();
    println!("{point}: ended {late:?} after SIGCONT");
    assert_eq!(output.status.code(), Some(137));
    assert!(late < Duration::from_millis(2500), "{point}: {late:?}");
    let (receipt, _) = d98_receipt(&supervisor, root.path(), command).await?;
    assert_eq!(receipt, "done:137");
    let traces = x111_traces(&trace)?;
    let [trace] = &traces[..] else { panic!("{traces:?}") };
    println!("trace {trace}");
    assert_eq!(trace.matches('!').count(), 1, "{trace}");
    assert_eq!(d98_retire(&supervisor, root.path()).await?, "MARATHON_D96_1 retired");
    Ok(())
}

#[tokio::test]
async fn an_owner_descheduled_after_its_clock_sample_acts_on_its_deadline_within_its_longest_sleep_x111()
-> Result<(), Box<dyn std::error::Error>> {
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    x111_descheduled_witness("sample").await
}

#[tokio::test]
async fn an_owner_descheduled_before_its_select_acts_on_its_deadline_within_its_longest_sleep_x111()
-> Result<(), Box<dyn std::error::Error>> {
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    x111_descheduled_witness("sleep").await
}

#[tokio::test]
async fn escalation_and_lease_expiry_end_with_their_receipts_x111() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;
    if !Path::new("/usr/bin/perl").is_file() {
        eprintln!("worker witness requires stock Perl");
        return Ok(());
    }
    let (root, supervisor) = x111_root()?;
    let files = tempfile::tempdir()?;
    // An unconfirmed recorder that ignores TERM: the KILL request sends TERM 2 s after it and ends the group 1 s later.
    // The cutoffs themselves are pinned by the D98 tests above.
    let recorder = "00000000000000000000000000000001";
    let ready = files.path().join("ready");
    let payload = r#"$SIG{TERM} = 'IGNORE'; print STDERR "Note: No display specified.\n";
open my $f, '>', $ARGV[0] or die $!; close $f; while (1) { select undef, undef, undef, .01 }"#;
    let detached = x111_spawn(
        &supervisor,
        root.path(),
        "60000",
        "detach",
        &[recorder, "/usr/bin/perl", "-e", payload, ready.to_str().unwrap()],
        &files.path().join("recorder"),
        &[],
    )?;
    let output = tokio::time::timeout(Duration::from_secs(60), detached.wait_with_output()).await??;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    d98_file(&ready).await?;
    let reply = d98_control(&supervisor, root.path(), &format!("{recorder}:3")).await?;
    assert!(reply.starts_with(&format!("{recorder} running:")), "{reply}");
    let (receipt, _) = d98_receipt(&supervisor, root.path(), recorder).await?;
    assert_eq!(receipt.split(' ').next(), Some("done:137"));
    // A 1.5 s lease that nobody renews ends a command at its expiry.
    let command = "00000000000000000000000000000002";
    let owner = x111_spawn(
        &supervisor,
        root.path(),
        "1500",
        "run",
        &[command, "/bin/sleep", "60"],
        &files.path().join("command"),
        &[],
    )?;
    let output = tokio::time::timeout(Duration::from_secs(60), owner.wait_with_output()).await??;
    assert_eq!(output.status.code(), Some(137));
    let (receipt, _) = d98_receipt(&supervisor, root.path(), command).await?;
    assert_eq!(receipt, "done:137");
    assert_eq!(d98_retire(&supervisor, root.path()).await?, "MARATHON_D96_1 retired");
    Ok(())
}
