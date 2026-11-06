//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/rsync/RsyncFileBridge.kt
mod support;
use marathon_apple::cmd::{
    FileBridge,
    remote::{
        rsync::{RsyncFileBridge, RsyncTarget, rsync_file_bridge::SEND_IO_TIMEOUT_SECONDS},
        ssh::sshj::config::SshLogging,
    },
};
use marathon_config::vendor::{RsyncConfiguration, SshAuthentication, SshConfiguration};
use std::{os::unix::fs::PermissionsExt, path::Path, sync::Arc};
use support::{Clock, Runtime, duration, ssh};
async fn bridge(
    auth: Option<SshAuthentication>,
    debug: bool,
    ssh_debug: bool,
    interval: i64,
    port: i32,
    logging: SshLogging,
) -> RsyncFileBridge {
    bridge_with_remote_path(auth, debug, ssh_debug, interval, port, logging, "/usr/bin/rsync").await
}
#[allow(clippy::too_many_arguments)]
async fn bridge_with_remote_path(
    auth: Option<SshAuthentication>,
    debug: bool,
    ssh_debug: bool,
    interval: i64,
    port: i32,
    logging: SshLogging,
    remote_path: &str,
) -> RsyncFileBridge {
    RsyncFileBridge::new(
        RsyncTarget {
            addr: "127.0.0.1".into(),
            port,
        },
        debug,
        SshConfiguration {
            authentication: None,
            known_hosts_path: None,
            keep_alive_interval: duration(interval),
            keep_alive_max_count: 4,
            debug: ssh_debug,
            share_worker_connection: false,
        },
        RsyncConfiguration {
            remote_path: remote_path.into(),
        },
        auth,
        Arc::new(Runtime),
        Arc::new(Clock::default()),
        logging,
    )
    .await
    .unwrap()
}
/// A recorded Kotlin argv as a send has it now: `--timeout` after `--times` (D31). Receives have no such flag.
fn with_d31<S: From<String> + PartialEq<&'static str>>(mut argv: Vec<S>, receive: bool) -> Vec<S> {
    if !receive {
        let times = argv.iter().position(|arg| *arg == "--times").unwrap();
        argv.insert(times + 1, format!("--timeout={SEND_IO_TIMEOUT_SECONDS}").into());
    }
    argv
}
fn key(path: &str) -> SshAuthentication {
    SshAuthentication::PublicKey {
        username: "fixture".into(),
        key: jcompat::file_manager::JavaPath::from_text(path),
    }
}
#[tokio::test]
async fn host_argv_matches_the_actual_kotlin_wrapper_probe() {
    let rows = include_str!("../../../fixtures/oracle/connections/rsync-argv.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let mut index = 0;
    for is_key in [false, true] {
        for debug in [false, true] {
            for ssh_debug in [false, true] {
                let auth = if is_key {
                    key("/fixture/key")
                } else {
                    SshAuthentication::Password {
                        username: "fixture".into(),
                        password: "fake-password".into(),
                    }
                };
                if debug {
                    // The probe's debug bridge records its constructor's version probe first.
                    assert_eq!(rows[index]["argv"], serde_json::json!(["--version", "/tmp", "/tmp"]));
                    index += 1;
                }
                for receive in [false, true] {
                    let b = bridge(Some(auth.clone()), debug, ssh_debug, 1500, 22222, ssh::logging(false).0).await;
                    let (source, dest) = if receive {
                        ("127.0.0.1:/tmp/marathon/tree/".to_owned(), "<root>/dest/".to_owned())
                    } else {
                        ("<root>/src/".to_owned(), "127.0.0.1:/tmp/marathon/tree/".to_owned())
                    };
                    let mut args = b.arguments(source, dest, receive).unwrap();
                    for arg in &mut args {
                        if arg == "/usr/bin/rsync" || arg == "/usr/bin/sshpass" {
                            *arg = "<shim>".into();
                        }
                    }
                    let expected = rows[index]["argv"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|s| s.as_str().unwrap().to_owned())
                        .collect::<Vec<_>>();
                    // The shim records argv after argv[0]; password records its executable rsync operand too.
                    assert_eq!(&args[1..], with_d31(expected, receive));
                    index += 1;
                }
            }
        }
    }
    assert_eq!(index, rows.len());
    let b = bridge(None, false, false, 0, 22, ssh::logging(false).0).await;
    assert!(b.send(Path::new("/unused"), "/tmp/marathon/dst").await.is_err());
}
#[tokio::test]
async fn empty_options_match_the_actual_kotlin_wrapper_argv() {
    let rows = include_str!("../../../fixtures/oracle/connections/rsync-empty-options-argv.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let mut index = 0;
    for remote_path in ["/usr/bin/rsync", ""] {
        for password in ["fake-password", ""] {
            let auth = SshAuthentication::Password {
                username: "fixture".into(),
                password: password.into(),
            };
            let b = bridge_with_remote_path(
                Some(auth),
                false,
                false,
                1500,
                22222,
                ssh::logging(false).0,
                remote_path,
            )
            .await;
            for receive in [false, true] {
                let (source, dest) = if receive {
                    ("127.0.0.1:/tmp/marathon/tree/", "<root>/dest/")
                } else {
                    ("<root>/src/", "127.0.0.1:/tmp/marathon/tree/")
                };
                let mut args = b.arguments(source.into(), dest.into(), receive).unwrap();
                assert_eq!(args[0], "/usr/bin/sshpass");
                for arg in &mut args {
                    if arg == "/usr/bin/rsync" {
                        *arg = "<shim>".into();
                    }
                }
                let expected: Vec<_> = rows[index]["argv"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_owned())
                    .collect();
                assert_eq!(args[1..], with_d31(expected, receive));
                index += 1;
            }
        }
    }
}
#[test]
fn real_rsync_roundtrip_with_awaited_worker_shutdown() {
    if std::env::var_os("MRS43_RSYNC_CHILD").is_some() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let shims = root.join("tools/fake-worker/shims");
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "real_rsync_child", "--nocapture"])
        .env("MRS43_RSYNC_CHILD", dir.path())
        .env("RSYNC4J_RSYNC", shims.join("rsync"))
        .env("RSYNC4J_SSHPASS", shims.join("sshpass"))
        .env("FW_REAL_RSYNC", "/usr/bin/rsync")
        .env("FW_REAL_SSH", "/usr/bin/ssh")
        .env(
            "FW_REAL_SSHPASS",
            if cfg!(target_os = "macos") {
                "/opt/homebrew/bin/sshpass"
            } else {
                "/usr/bin/sshpass"
            },
        )
        .env("FW_ARGV_LOG", dir.path().join("argv.jsonl"))
        .env(
            "PATH",
            format!("{}:{}", shims.display(), std::env::var("PATH").unwrap_or_default()),
        )
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let argv = std::fs::read_to_string(dir.path().join("argv.jsonl")).unwrap();
    assert!(argv.contains("--partial-dir=.rsync-partial"));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_rsync_child() {
    let Some(root) = std::env::var_os("MRS43_RSYNC_CHILD") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let fake = ssh::fake(ssh::suite_script(), 8).await;
    let port = i32::from(fake.port());
    fake.worker.fs.mkdir("/tmp/marathon/tree").await.unwrap();
    let private = root.join("client-key");
    std::fs::write(
        &private,
        ssh::client_key_pair()
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
    let src = root.join("source");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("sub/hello"), b"hello\n").unwrap();
    std::fs::write(
        src.join("blob"),
        (0..100000).map(|i| (i % 251) as u8).collect::<Vec<_>>(),
    )
    .unwrap();
    std::os::unix::fs::symlink("sub/hello", src.join("link")).unwrap();
    let (logging, events) = ssh::logging(true);
    let b = bridge(Some(key(private.to_str().unwrap())), true, true, 1500, port, logging).await;
    assert!(b.send(&src, "/tmp/marathon/tree/").await.unwrap());
    fake.worker
        .fs
        .write("/tmp/marathon/tree/stale", b"stale".to_vec(), 0o600)
        .await
        .unwrap();
    assert!(b.send(&src, "/tmp/marathon/tree/").await.unwrap());
    assert!(fake.worker.fs.read("/tmp/marathon/tree/stale").await.is_err());
    let dst = root.join("back");
    std::fs::create_dir(&dst).unwrap();
    assert!(b.receive("/tmp/marathon/tree/", &dst).await.unwrap());
    assert_eq!(
        std::fs::read(dst.join("blob")).unwrap(),
        std::fs::read(src.join("blob")).unwrap()
    );
    assert_eq!(std::fs::read_link(dst.join("link")).unwrap(), Path::new("sub/hello"));
    assert_eq!(std::fs::read(dst.join("sub/hello")).unwrap(), b"hello\n");
    assert!(b.send(&src.join("blob"), "/tmp/marathon/file").await.unwrap());
    let file = root.join("file");
    assert!(b.receive("/tmp/marathon/file", &file).await.unwrap());
    assert_eq!(std::fs::read(file).unwrap(), std::fs::read(src.join("blob")).unwrap());
    b.join_pending().await.unwrap();
    assert!(events.lock().unwrap().iter().any(|(_, _, m, _)| m.contains("rsync")));
    fake.worker.shutdown().await.unwrap();
}

#[test]
fn send_serialization_unbounded_receives_and_the_receive_deadline_match_kotlin() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let script = root.join("tools/oracle/connection-probes/gated-rsync");
    for mode in ["gates", "deadlines"] {
        let dir = tempfile::tempdir().unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "process_gates_child", "--nocapture"])
            .env("MRS43_PROCESS_CHILD", mode)
            .env("MRS43_GATE_ROOT", dir.path())
            .env("RSYNC4J_RSYNC", &script)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
async fn marker(root: &Path, label: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while !root.join(format!("{label}.entered")).exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn process_gates_child() {
    let Ok(mode) = std::env::var("MRS43_PROCESS_CHILD") else {
        return;
    };
    let root = std::path::PathBuf::from(std::env::var_os("MRS43_GATE_ROOT").unwrap());
    let src = root.join("source");
    std::fs::write(&src, b"payload").unwrap();
    let b = Arc::new(bridge(Some(key("/fixture/key")), false, false, 0, 22222, ssh::logging(false).0).await);
    let send = {
        let b = b.clone();
        let src = src.clone();
        tokio::spawn(async move { b.send(&src, "/tmp/marathon/send1").await })
    };
    marker(&root, "send1").await;
    let receive = {
        let b = b.clone();
        let dst = root.join("back1");
        tokio::spawn(async move { b.receive("/tmp/marathon/receive1", &dst).await })
    };
    marker(&root, "receive1").await;
    if mode == "gates" {
        let send2 = {
            let b = b.clone();
            let src = src.clone();
            tokio::spawn(async move { b.send(&src, "/tmp/marathon/send2").await })
        };
        let receive2 = {
            let b = b.clone();
            let dst = root.join("back2");
            tokio::spawn(async move { b.receive("/tmp/marathon/receive2", &dst).await })
        };
        marker(&root, "receive2").await;
        let early = root.join("send2.entered").exists();
        std::fs::write(root.join("release"), b"").unwrap();
        for task in [send, send2, receive, receive2] {
            assert!(task.await.unwrap().unwrap());
        }
        b.join_pending().await.unwrap();
        assert!(!early);
        let rows = std::fs::read_to_string(root.join("events.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let first_exit = rows
            .iter()
            .position(|r| r["label"] == "send1" && r["event"] == "exit")
            .unwrap();
        let second_enter = rows
            .iter()
            .position(|r| r["label"] == "send2" && r["event"] == "enter")
            .unwrap();
        assert!(first_exit < second_enter);
    } else {
        let result = tokio::time::timeout(std::time::Duration::from_secs(40), receive).await;
        let still_running = !send.is_finished();
        std::fs::write(root.join("release"), b"").unwrap();
        assert!(send.await.unwrap().unwrap());
        b.join_pending().await.unwrap();
        assert!(!result.unwrap().unwrap().unwrap());
        assert!(still_running);
    }
}

#[test]
fn nonzero_and_process_start_errors_keep_the_kotlin_boundaries() {
    for profile in ["nonzero", "badutf8", "nonexec", "missing"] {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("rsync");
        if profile != "missing" {
            let script: &[u8] = if profile == "badutf8" {
                b"#!/bin/sh\nprintf '\\355\\240\\200\\rline' >&2\nexit 23\n"
            } else {
                b"#!/bin/sh\nprintf 'failure\\rline' >&2\nexit 23\n"
            };
            std::fs::write(&program, script).unwrap();
            std::fs::set_permissions(
                &program,
                std::fs::Permissions::from_mode(if profile == "nonexec" { 0o600 } else { 0o700 }),
            )
            .unwrap();
        }
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "process_failure_child", "--nocapture"])
            .env("MRS43_FAILURE_CHILD", profile)
            .env("RSYNC4J_RSYNC", &program)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
#[tokio::test]
async fn process_failure_child() {
    let Ok(profile) = std::env::var("MRS43_FAILURE_CHILD") else {
        return;
    };
    let (log, events) = ssh::logging(true);
    let b = bridge(Some(key("/fixture/key")), false, false, 0, 22, log).await;
    let send = b.send(Path::new("/unused"), "/tmp/marathon/dst").await;
    let receive = b.receive("/tmp/marathon/src", Path::new("/unused")).await;
    if profile == "nonzero" || profile == "badutf8" {
        assert_eq!(send, Ok(false));
        assert_eq!(receive, Ok(false));
        let events = events.lock().unwrap();
        let output = if profile == "badutf8" {
            "�\nline\n"
        } else {
            "failure\nline\n"
        };
        assert_eq!(events[0].2, format!("send error: {output}"));
        assert_eq!(events[1].2, output);
    } else {
        let class = if profile == "missing" {
            "java.lang.IllegalStateException"
        } else {
            "java.io.IOException"
        };
        for result in [send, receive] {
            assert!(
                matches!(result,Err(marathon_core::error::VendorError::Other(ref error))if error.class.to_string_lossy()==class)
            );
        }
    }
    b.join_pending().await.unwrap();
}

/// A receive whose 30-second monitor clock fails before the child exits.
struct FailingDelay(Clock);
impl marathon_core::runtime::VendorClock for FailingDelay {
    fn delay(
        &self,
        _: i64,
        _: Arc<dyn marathon_core::runtime::CancellationSignal>,
    ) -> marathon_core::runtime::VendorFuture<'_, ()> {
        Box::pin(async {
            Err(marathon_core::error::VendorError::Other(marathon_apple::cmd::failure(
                "java.lang.IllegalStateException",
                "clock failed",
            )))
        })
    }
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn marathon_core::runtime::CancellationSignal>,
        millis: i64,
        body: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> marathon_core::runtime::VendorFuture<'a, ()> {
        self.0.with_timeout(parent, millis, body)
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn marathon_core::runtime::CancellationSignal>,
        millis: i64,
        body: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> marathon_core::runtime::VendorFuture<'a, Option<()>> {
        self.0.with_timeout_or_null(parent, millis, body)
    }
}
/// Not a Kotlin test: owned-task pruning and early-error process cleanup.
#[test]
fn finished_transfers_are_pruned_and_an_early_error_still_reaps_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("rsync");
    std::fs::write(&program, b"#!/bin/sh\nsleep 0.3\necho exited >> \"$MRS43_EXITS\"\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "early_error_child", "--nocapture"])
        .env("MRS43_EXITS", dir.path().join("exits"))
        .env("RSYNC4J_RSYNC", &program)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
#[tokio::test]
async fn early_error_child() {
    let Some(exits) = std::env::var_os("MRS43_EXITS") else {
        return;
    };
    let exited = || std::fs::read_to_string(&exits).unwrap_or_default().lines().count();
    let b = bridge(Some(key("/fixture/key")), false, false, 0, 22, ssh::logging(false).0).await;
    for _ in 0..3 {
        assert_eq!(b.send(Path::new("/unused"), "/tmp/marathon/dst").await, Ok(true));
    }
    assert_eq!(b.retained_tasks(), 1);
    b.join_pending().await.unwrap();
    assert_eq!(b.retained_tasks(), 0);

    let failing = RsyncFileBridge::new(
        RsyncTarget {
            addr: "127.0.0.1".into(),
            port: 22,
        },
        false,
        SshConfiguration {
            authentication: None,
            known_hosts_path: None,
            keep_alive_interval: duration(0),
            keep_alive_max_count: 4,
            debug: false,
            share_worker_connection: false,
        },
        RsyncConfiguration {
            remote_path: "/usr/bin/rsync".into(),
        },
        Some(key("/fixture/key")),
        Arc::new(Runtime),
        Arc::new(FailingDelay(Clock::default())),
        ssh::logging(false).0,
    )
    .await
    .unwrap();
    let result = failing.receive("/tmp/marathon/src", Path::new("/unused")).await;
    assert!(
        matches!(result, Err(marathon_core::error::VendorError::Other(ref error)) if error.message.as_ref().map(|m| m.to_string_lossy()).as_deref() == Some("clock failed"))
    );
    // The clock error is returned only after the child it started has exited.
    assert_eq!(exited(), 4);
    failing.join_pending().await.unwrap();
}

/// One D31 run, in a child process because rsync's path is process-wide. `program` stands in for rsync. Scripted stand-ins
/// run in compressed time by default; `factor` 1 keeps the bridge's steps real, for what must finish inside one.
struct StalledSend<'a> {
    mode: &'a str,
    program: &'a Path,
    root: &'a Path,
    extra_path: Option<&'a Path>,
    sshpass: Option<&'a Path>,
    factor: i64,
    silent_peer: bool,
    /// The child test to run: a stalled send by default, or a receive past its deadline (U155).
    child: &'a str,
}
impl<'a> StalledSend<'a> {
    fn new(mode: &'a str, program: &'a Path, root: &'a Path) -> Self {
        Self {
            mode,
            program,
            root,
            extra_path: None,
            sshpass: None,
            factor: 100,
            silent_peer: false,
            child: "stalled_send_runs",
        }
    }
}
fn stalled_send_child(mode: &str, program: &Path, extra_path: Option<&Path>, root: &Path) {
    run_stalled_send(StalledSend {
        extra_path,
        ..StalledSend::new(mode, program, root)
    })
}
fn run_stalled_send(run: StalledSend) {
    let StalledSend {
        mode,
        program,
        root,
        extra_path,
        sshpass,
        factor,
        silent_peer,
        child: child_test,
    } = run;
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", child_test, "--nocapture"])
        .env("MRS_D31_MODE", mode)
        .env("MRS_D31_ROOT", root)
        .env("MRS_D31_FACTOR", factor.to_string())
        .env("RSYNC4J_RSYNC", program);
    if silent_peer {
        command.env("MRS_D31_SILENT_PEER", "1");
    }
    if let Some(sshpass) = sshpass {
        command.env("RSYNC4J_SSHPASS", sshpass).env("MRS_D31_PASSWORD", "1");
    }
    if let Some(path) = extra_path {
        command.env(
            "PATH",
            format!("{}:{}", path.display(), std::env::var("PATH").unwrap_or_default()),
        );
    }
    // Into a file and under a deadline: a regression here leaves the child stuck, and it must fail, not hang the suite.
    let log = root.join("child.log");
    let file = std::fs::File::create(&log).unwrap();
    let mut child = command.stdout(file.try_clone().unwrap()).stderr(file).spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let mut survivors = Vec::new();
    for file in ["pid", "ssh-pid", "held"] {
        if let Some(pid) = std::fs::read_to_string(root.join(file))
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
        {
            let pid = nix::unistd::Pid::from_raw(pid);
            // openrsync ends ssh on the bridge's TERM. rsync 3 exits by itself, and what becomes of a hung ssh is its affair,
            // so the bridge can only answer for rsync there.
            let answers_for = file == "pid"
                || (file == "ssh-pid" && cfg!(target_os = "macos") && child_test != "early_exit_receive_runs");
            if answers_for && nix::sys::signal::kill(pid, None).is_ok() {
                survivors.push(file);
            }
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
        }
    }
    let output = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(status.is_some_and(|status| status.success()), "{output}");
    assert_eq!(survivors, Vec::<&str>::new(), "{output}");
    // Kotlin's diagnostic: it says the deadline passed, so it comes with that and not with a process that ended before it.
    let diagnostic = "Timeout of 30 seconds reached, terminating process...";
    if child_test == "stalled_receive_runs" {
        assert!(output.contains(diagnostic), "{output}");
    } else if child_test == "early_exit_receive_runs" {
        assert!(!output.contains(diagnostic), "{output}");
    }
}
fn script(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
/// Runs a healthy send when the destination is `.../next`. Otherwise it records its pid and does `body`.
fn fake_rsync(root: &Path, body: &str) -> std::path::PathBuf {
    let program = root.join("rsync");
    script(
        &program,
        &format!(
            r#"#!/bin/bash
case "$*" in *--timeout=300*) ;; *) echo missing timeout >&2; exit 99;; esac
case "$*" in *127.0.0.1:/tmp/marathon/next*) echo next >> "$MRS_D31_ROOT/order"; exit 0;; esac
echo $$ > "$MRS_D31_ROOT/pid"
echo stalled >> "$MRS_D31_ROOT/order"
{body}
"#
        ),
    );
    program
}
#[test]
fn a_stalled_send_that_waits_for_a_hung_ssh_is_stopped_on_term_d31() {
    // openrsync's way: the report, then a wait for ssh that never ends.
    let dir = tempfile::tempdir().unwrap();
    let program = fake_rsync(
        dir.path(),
        "echo 'rsync(1): error: poll: timeout' >&2\nexec sleep 100037",
    );
    stalled_send_child("hung", &program, None, dir.path());
}
#[test]
fn a_stalled_send_that_also_ignores_term_is_killed_d31() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake_rsync(
        dir.path(),
        "trap '' TERM\necho 'rsync(1): error: poll: timeout' >&2\nwhile :; do /bin/sleep 0.05; done",
    );
    stalled_send_child("hung", &program, None, dir.path());
}
#[test]
fn a_stalled_send_that_exits_while_ssh_holds_its_stderr_returns_d31() {
    // rsync 3's way, with a child that keeps the pipes open past its parent's exit.
    let dir = tempfile::tempdir().unwrap();
    let program = fake_rsync(
        dir.path(),
        "/bin/sleep 100037 &\necho $! > \"$MRS_D31_ROOT/held\"\necho '[sender] io timeout after 300 seconds -- exiting' >&2\nexit 30",
    );
    stalled_send_child("hung", &program, None, dir.path());
}
#[test]
fn a_failed_send_without_a_stall_report_keeps_its_message_and_stderr_d31() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake_rsync(dir.path(), "echo 'rsync: some other failure' >&2\nexit 23");
    stalled_send_child("other", &program, None, dir.path());
}
/// What `sshpass` does on TERM, measured with the real one: it exits at once, and rsync and ssh stay. See the real test.
const FAKE_SSHPASS: &str = "#!/bin/bash\nshift 2\ntrap 'exit 143' TERM\n\"$@\" &\nwait $!\n";
#[test]
fn a_stalled_send_under_sshpass_also_stops_rsync_d31() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake_rsync(
        dir.path(),
        "echo 'rsync(1): error: poll: timeout' >&2\nexec /bin/sleep 100037",
    );
    let sshpass = dir.path().join("sshpass");
    script(&sshpass, FAKE_SSHPASS);
    run_stalled_send(StalledSend {
        sshpass: Some(&sshpass),
        factor: 1,
        ..StalledSend::new("hung", &program, dir.path())
    });
}
#[test]
fn rsync_under_sshpass_that_ignores_term_is_killed_d31() {
    // KILL still reaches the listed process, whose start time has not changed.
    let dir = tempfile::tempdir().unwrap();
    let program = fake_rsync(
        dir.path(),
        "trap '' TERM\necho 'rsync(1): error: poll: timeout' >&2\nexec /bin/sleep 100038",
    );
    let sshpass = dir.path().join("sshpass");
    script(&sshpass, FAKE_SSHPASS);
    run_stalled_send(StalledSend {
        sshpass: Some(&sshpass),
        factor: 1,
        ..StalledSend::new("hung", &program, dir.path())
    });
}
#[test]
fn rsync_under_sshpass_gets_its_step_after_term_d31() {
    // sshpass is gone at once, and that must not cost rsync the step before KILL: a real 5 s against 0.5 s here.
    let dir = tempfile::tempdir().unwrap();
    let program = fake_rsync(
        dir.path(),
        "trap '/bin/sleep 0.5; echo finished > \"$MRS_D31_ROOT/handler\"; exit 143' TERM\necho 'rsync(1): error: poll: timeout' >&2\nwhile :; do /bin/sleep 0.05; done",
    );
    let sshpass = dir.path().join("sshpass");
    script(&sshpass, FAKE_SSHPASS);
    run_stalled_send(StalledSend {
        sshpass: Some(&sshpass),
        factor: 1,
        ..StalledSend::new("hung", &program, dir.path())
    });
    assert_eq!(
        std::fs::read_to_string(dir.path().join("handler")).unwrap(),
        "finished\n"
    );
}
/// A tool on this host's `PATH` or in `/usr/bin`, which is where the real-tool tests take it from.
fn real_tool(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var("PATH").unwrap_or_default();
    std::env::split_paths(&format!("{path}:/usr/bin"))
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}
/// The real rsync with a short `--timeout` and the real ssh, hung on a peer that accepts the connection and says nothing.
/// The report's wording and the wait after it are the installed rsync's own, and what TERM and a hangup do to ssh is the real
/// ssh's: a stand-in such as `sleep` dies on the hangup where ssh does not, which would hide what `sshpass` leaves behind.
/// The bridge's steps are real here, because a KILL that comes too early would orphan ssh.
fn real_rsync_with_a_hung_ssh(sshpass: Option<&Path>, child: &str) {
    let (Some(rsync), Some(ssh)) = (real_tool("rsync"), real_tool("ssh")) else {
        eprintln!("skipped: this host has no rsync or ssh to run");
        return;
    };
    // The bridge's own check wants these two there; on a host with them elsewhere RSYNC4J_* would say so.
    if !Path::new("/usr/bin/ssh").exists() || !Path::new("/usr/bin/ssh-keygen").exists() {
        eprintln!("skipped: this host has no /usr/bin/ssh and /usr/bin/ssh-keygen");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    // `exec` keeps the pid, so what is recorded is the real ssh.
    script(
        &bin.join("ssh"),
        &format!(
            "#!/bin/sh\necho $$ > \"$MRS_D31_ROOT/ssh-pid\"\nexec {} \"$@\"\n",
            ssh.display()
        ),
    );
    let program = dir.path().join("rsync");
    script(
        &program,
        &format!(
            r#"#!/bin/bash
case "$*" in *127.0.0.1:/tmp/marathon/next*) echo next >> "$MRS_D31_ROOT/order"; exit 0;; esac
echo $$ > "$MRS_D31_ROOT/pid"
echo stalled >> "$MRS_D31_ROOT/order"
args=()
for arg in "$@"; do
  case "$arg" in --timeout=300) args+=(--timeout=1);; *) args+=("$arg");; esac
done
exec {} "${{args[@]}}"
"#,
            rsync.display()
        ),
    );
    run_stalled_send(StalledSend {
        extra_path: Some(&bin),
        sshpass,
        factor: 1,
        silent_peer: true,
        child,
        ..StalledSend::new("hung", &program, dir.path())
    });
}
#[test]
fn the_real_rsync_gives_up_on_a_hung_ssh_d31() {
    real_rsync_with_a_hung_ssh(None, "stalled_send_runs");
}
fn real_sshpass() -> Option<&'static Path> {
    ["/opt/homebrew/bin/sshpass", "/usr/bin/sshpass"]
        .into_iter()
        .map(Path::new)
        .find(|path| path.exists())
}
#[test]
fn the_real_rsync_under_the_real_sshpass_gives_up_on_a_hung_ssh_d31() {
    match real_sshpass() {
        Some(sshpass) => real_rsync_with_a_hung_ssh(Some(sshpass), "stalled_send_runs"),
        None => eprintln!("skipped: no sshpass on this host"),
    }
}
#[test]
fn the_real_rsync_is_stopped_at_a_receives_deadline_u155() {
    real_rsync_with_a_hung_ssh(None, "stalled_receive_runs");
}
#[test]
fn the_real_rsync_under_the_real_sshpass_is_stopped_at_a_receives_deadline_u155() {
    match real_sshpass() {
        Some(sshpass) => real_rsync_with_a_hung_ssh(Some(sshpass), "stalled_receive_runs"),
        None => eprintln!("skipped: no sshpass on this host"),
    }
}
/// A stand-in for rsync on a receive, which has no `--timeout`, that records its pid and does `body`. With a real 3 s for the
/// deadline, at the factor these use, so that it has started by then on a loaded machine.
fn receive_past_its_deadline(root: &Path, body: &str, sshpass: bool, extra_path: Option<&Path>) {
    let program = root.join("rsync");
    script(
        &program,
        &format!(
            "#!/bin/bash\ncase \"$*\" in *--timeout*) echo unexpected timeout >&2; exit 99;; esac\necho $$ > \"$MRS_D31_ROOT/pid\"\n{body}\n"
        ),
    );
    let fake_sshpass = root.join("sshpass");
    script(&fake_sshpass, FAKE_SSHPASS);
    run_stalled_send(StalledSend {
        sshpass: sshpass.then_some(fake_sshpass.as_path()),
        extra_path,
        factor: 10,
        child: "stalled_receive_runs",
        ..StalledSend::new("hung", &program, root)
    });
}
/// A receive whose stand-in for rsync ends by itself and leaves a child holding its pipes, as a hung ssh would. Factor 1, because
/// the bound on the pipes is real time and the receive must come back well before its real 30 s.
fn receive_with_its_pipes_held(root: &Path, status: i32) {
    let program = root.join("rsync");
    script(
        &program,
        &format!(
            "#!/bin/bash\necho $$ > \"$MRS_D31_ROOT/pid\"\n/bin/sleep 100037 &\necho $! > \"$MRS_D31_ROOT/held\"\necho 'what rsync said' >&2\nexit {status}\n"
        ),
    );
    run_stalled_send(StalledSend {
        factor: 1,
        child: "early_exit_receive_runs",
        ..StalledSend::new(if status == 0 { "held-ok" } else { "held-failed" }, &program, root)
    });
}
#[test]
fn a_receive_that_fails_while_a_child_holds_its_pipes_returns_with_its_text_u158() {
    let dir = tempfile::tempdir().unwrap();
    receive_with_its_pipes_held(dir.path(), 23);
}
#[test]
fn a_receive_that_succeeds_while_a_child_holds_its_pipes_returns_u158() {
    let dir = tempfile::tempdir().unwrap();
    receive_with_its_pipes_held(dir.path(), 0);
}
#[test]
fn the_real_ssh_holding_the_pipes_of_a_killed_rsync_does_not_hold_up_a_receive_u158() {
    // The real rsync dies, and the real ssh it started stays, hung on a silent peer, with rsync's stderr.
    real_rsync_with_a_hung_ssh(None, "early_exit_receive_runs");
}
const IGNORES_TERM: &str = "trap '' TERM\nwhile :; do /bin/sleep 0.05; done";
#[test]
fn a_receive_past_its_deadline_that_ignores_term_is_killed_u155() {
    let dir = tempfile::tempdir().unwrap();
    receive_past_its_deadline(dir.path(), IGNORES_TERM, false, None);
}
#[test]
fn a_receive_past_its_deadline_under_sshpass_also_stops_rsync_u155() {
    let dir = tempfile::tempdir().unwrap();
    receive_past_its_deadline(dir.path(), "exec /bin/sleep 100037", true, None);
}
#[test]
fn rsync_that_ignores_term_at_a_receives_deadline_under_sshpass_is_killed_u155() {
    let dir = tempfile::tempdir().unwrap();
    receive_past_its_deadline(dir.path(), IGNORES_TERM, true, None);
}
/// `pgrep` that leaves a mark in the run's directory, for the tests of when children are listed.
fn recording_pgrep(root: &Path) -> std::path::PathBuf {
    let real = real_tool("pgrep").expect("the bridge lists children with pgrep");
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    script(
        &bin.join("pgrep"),
        &format!(
            "#!/bin/sh\ntouch \"$MRS_D31_ROOT/pgrep-ran\"\nexec {} \"$@\"\n",
            real.display()
        ),
    );
    bin
}
#[test]
fn only_a_receive_under_sshpass_lists_children_u155() {
    for (sshpass, listed) in [(false, false), (true, true)] {
        let dir = tempfile::tempdir().unwrap();
        let bin = recording_pgrep(dir.path());
        receive_past_its_deadline(dir.path(), "exec /bin/sleep 100037", sshpass, Some(&bin));
        assert_eq!(
            dir.path().join("pgrep-ran").exists(),
            listed,
            "under sshpass: {sshpass}"
        );
    }
}
/// The bridge of a child test, as `run_stalled_send`'s environment describes it, and the silent peer it may point at, which
/// the caller holds for the whole run: the kernel accepts the connection, and nothing ever answers it.
async fn stalled_bridge(
    logging: marathon_apple::cmd::remote::ssh::sshj::config::SshLogging,
) -> (RsyncFileBridge, Option<std::net::TcpListener>) {
    let silent_peer =
        std::env::var_os("MRS_D31_SILENT_PEER").map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap());
    let port = silent_peer
        .as_ref()
        .map_or(22, |peer| i32::from(peer.local_addr().unwrap().port()));
    let factor = std::env::var("MRS_D31_FACTOR").unwrap().parse().unwrap();
    let bridge = RsyncFileBridge::new(
        RsyncTarget {
            addr: "127.0.0.1".into(),
            port,
        },
        false,
        SshConfiguration {
            authentication: None,
            known_hosts_path: None,
            keep_alive_interval: duration(0),
            keep_alive_max_count: 4,
            debug: false,
            share_worker_connection: false,
        },
        RsyncConfiguration {
            remote_path: "/usr/bin/rsync".into(),
        },
        Some(if std::env::var_os("MRS_D31_PASSWORD").is_some() {
            SshAuthentication::Password {
                username: "fixture".into(),
                password: "fake-password".into(),
            }
        } else {
            key("/fixture/key")
        }),
        Arc::new(Runtime),
        Arc::new(ssh::ScaledClock::new(factor)),
        logging,
    )
    .await
    .unwrap();
    (bridge, silent_peer)
}
/// U155: the receive fails at its deadline and no sooner, and the process it started is gone when it returns.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_receive_runs() {
    if std::env::var_os("MRS_D31_MODE").is_none() {
        return;
    }
    let root = std::path::PathBuf::from(std::env::var_os("MRS_D31_ROOT").unwrap());
    let (logging, events) = ssh::logging(false);
    let (bridge, _silent_peer) = stalled_bridge(logging).await;
    let factor: u64 = std::env::var("MRS_D31_FACTOR").unwrap().parse().unwrap();
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        bridge.receive("/tmp/marathon/stuck", &root.join("back")),
    )
    .await
    .unwrap();
    assert_eq!(result, Ok(false));
    let deadline = std::time::Duration::from_millis(30_000 / factor);
    assert!(started.elapsed() >= deadline, "{:?}", started.elapsed());
    // At once if it is the child, which is reaped. An rsync killed under `sshpass` was orphaned, and init reaps it soon after.
    let pid = std::fs::read_to_string(root.join("pid")).unwrap();
    let pid = nix::unistd::Pid::from_raw(pid.trim().parse().unwrap());
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while nix::sys::signal::kill(pid, None) != Err(nix::errno::Errno::ESRCH) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // Whatever rsync said before the stop is its own text, as on any failed receive, with no word of a send.
    let lines = ssh::lines(&events);
    assert!(
        !lines.iter().any(|(.., message)| message.contains("send error")),
        "{lines:?}"
    );
    bridge.join_pending().await.unwrap();
}
/// U158: a receive whose process ended before its deadline returns within the pipes' bound, with that process's status and
/// what it wrote. Mode `hung` kills a real rsync once its real ssh is up; the others are `receive_with_its_pipes_held`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn early_exit_receive_runs() {
    let Ok(mode) = std::env::var("MRS_D31_MODE") else {
        return;
    };
    let root = std::path::PathBuf::from(std::env::var_os("MRS_D31_ROOT").unwrap());
    let (logging, events) = ssh::logging(false);
    let (bridge, _silent_peer) = stalled_bridge(logging).await;
    if mode == "hung" {
        let root = root.clone();
        tokio::spawn(async move {
            let read = |name: &str| std::fs::read_to_string(root.join(name)).ok();
            while read("ssh-pid").is_none() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            let pid: i32 = read("pid").unwrap().trim().parse().unwrap();
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL).unwrap();
        });
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        bridge.receive("/tmp/marathon/stuck", &root.join("back")),
    )
    .await
    .unwrap();
    // The harness checks that this was not the deadline: the diagnostic must be absent.
    assert_eq!(result, Ok(mode == "held-ok"));
    // The child is still up, so it was the bound and not the end of the pipes that let this return.
    let holder = if mode == "hung" { "ssh-pid" } else { "held" };
    let holder: i32 = std::fs::read_to_string(root.join(holder))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(holder), None), Ok(()));
    if mode == "held-failed" {
        let lines = ssh::lines(&events);
        assert!(
            lines.iter().any(|(.., message)| message == "what rsync said\n"),
            "{lines:?}"
        );
    }
    bridge.join_pending().await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_send_runs() {
    let Ok(mode) = std::env::var("MRS_D31_MODE") else {
        return;
    };
    let root = std::path::PathBuf::from(std::env::var_os("MRS_D31_ROOT").unwrap());
    let source = root.join("source");
    std::fs::write(&source, b"payload").unwrap();
    let (logging, events) = ssh::logging(false);
    let (bridge, _silent_peer) = stalled_bridge(logging).await;
    let bridge = Arc::new(bridge);
    let first = {
        let (bridge, source) = (bridge.clone(), source.clone());
        tokio::spawn(async move { bridge.send(&source, "/tmp/marathon/stuck").await })
    };
    let pid_file = root.join("pid");
    let pid: i32 = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // Queued behind the stuck send by the per-bridge mutex; it gets its turn when that one ends.
    let queued = {
        let (bridge, source) = (bridge.clone(), source.clone());
        tokio::spawn(async move { bridge.send(&source, "/tmp/marathon/next").await })
    };
    let finished = async { (first.await.unwrap(), queued.await.unwrap()) };
    let (first, queued) = tokio::time::timeout(std::time::Duration::from_secs(60), finished)
        .await
        .unwrap();
    assert_eq!((first, queued), (Ok(false), Ok(true)));
    // Gone, so a retry cannot overlap it, and the next send ran after it. Under `sshpass` rsync is an orphan by now, so
    // its reaper is init, which may not have got to it yet: a zombie still answers signal 0.
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) != Err(nix::errno::Errno::ESRCH) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(root.join("order")).unwrap(), "stalled\nnext\n");
    let lines = ssh::lines(&events);
    let errors: Vec<_> = lines
        .iter()
        .filter(|(_, level, _)| *level == jcompat::logging::Level::Error)
        .map(|(.., message)| message.as_str())
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    if mode == "hung" {
        assert!(
            errors[0].starts_with(
                "send error: no progress in 300 s sending to 127.0.0.1:/tmp/marathon/stuck, rsync stopped: "
            ),
            "{errors:?}"
        );
    } else {
        assert_eq!(errors[0], "send error: rsync: some other failure\n");
    }
    bridge.join_pending().await.unwrap();
}
