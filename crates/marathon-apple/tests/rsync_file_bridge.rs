//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/rsync/RsyncFileBridge.kt
use marathon_error::{FailureKind, InjectedKind};
mod support;
use marathon_apple::cmd::{
    FileBridge,
    remote::{
        rsync::{
            RsyncFileBridge, RsyncTarget,
            rsync_file_bridge::{TRANSFER_IO_TIMEOUT_SECONDS, TRANSFER_STALLED},
        },
        ssh::sshj::config::SshLogging,
    },
};
use marathon_config::vendor::{RsyncConfiguration, SshAuthentication, SshConfiguration};
use marathon_core::error::{LeavePoolReason, VendorError};
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
/// A recorded Kotlin argv as a transfer has it now: `--timeout` after `--times` (D31 on a send, D43 on a receive).
fn with_timeout<S: From<String> + PartialEq<&'static str>>(mut argv: Vec<S>) -> Vec<S> {
    let times = argv.iter().position(|arg| *arg == "--times").unwrap();
    argv.insert(times + 1, format!("--timeout={TRANSFER_IO_TIMEOUT_SECONDS}").into());
    argv
}
fn key(path: &str) -> SshAuthentication {
    SshAuthentication::PublicKey {
        username: "fixture".into(),
        key: std::path::PathBuf::from(path),
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
                    let mut args = b.arguments(source, dest).unwrap();
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
                    assert_eq!(&args[1..], with_timeout(expected));
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
                let mut args = b.arguments(source.into(), dest.into()).unwrap();
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
                assert_eq!(args[1..], with_timeout(expected));
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
fn send_serialization_unbounded_receives_and_no_total_limit_on_a_receive_d43() {
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
    let b = Arc::new(
        RsyncFileBridge::new(
            RsyncTarget {
                addr: "127.0.0.1".into(),
                port: 22222,
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
            Arc::new(ssh::ScaledClock::new(10)),
            ssh::logging(false).0,
        )
        .await
        .unwrap(),
    );
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
        // D43, where fixtures/oracle/connections/deadlines.jsonl is the JVM's witness: there the receive fails at 30 s and the
        // send stays running. On this clock 30 s is 3 s, which this outlasts, and the 300 s with nothing arriving is 30 s.
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let still_running = !receive.is_finished() && !send.is_finished();
        std::fs::write(root.join("release"), b"").unwrap();
        assert!(send.await.unwrap().unwrap());
        assert!(receive.await.unwrap().unwrap());
        b.join_pending().await.unwrap();
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
        let native: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/phase8/d57-process-text.json")).unwrap();
        let output = if profile == "badutf8" {
            native["rsync_stderr"].as_str().unwrap()
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
                matches!(result,Err(marathon_core::error::VendorError::Other(ref error))if error.kind.legacy_class()==class)
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
                FailureKind::IllegalState,
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
/// Not a Kotlin test: owned-task pruning, and what a receive still needs of the clock.
#[test]
fn finished_transfers_are_pruned_and_a_receive_has_no_use_for_the_clock_d43() {
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
    // D43: a receive that reports no stall asks the clock for nothing, so a clock that fails doesn't reach it.
    assert_eq!(
        failing.receive("/tmp/marathon/src", Path::new("/unused")).await,
        Ok(true)
    );
    assert_eq!(exited(), 4);
    failing.join_pending().await.unwrap();
}

/// One D31 run, in a child process because rsync's path is process-wide. `program` stands in for rsync. Scripted stand-ins
/// run in compressed time by default, which also shortens the time with no progress before a report counts as a stall (285 s,
/// 2.85 s at the default); a lower `factor` keeps the bridge's steps longer, for what must finish inside one.
struct StalledSend<'a> {
    mode: &'a str,
    program: &'a Path,
    root: &'a Path,
    extra_path: Option<&'a Path>,
    sshpass: Option<&'a Path>,
    factor: i64,
    /// How fast the bridge's clock says time passes, if not `factor`: faster, for a bridge whose delays come late.
    elapsed_factor: Option<i64>,
    silent_peer: bool,
    /// D54: how many times longer than nominal the bridge's delays take, if not compressed by `factor`.
    slow: Option<i64>,
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
            elapsed_factor: None,
            silent_peer: false,
            slow: None,
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
        elapsed_factor,
        silent_peer,
        slow,
        child: child_test,
    } = run;
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", child_test, "--nocapture"])
        .env("MRS_D31_MODE", mode)
        .env("MRS_D31_ROOT", root)
        .env("MRS_D31_FACTOR", factor.to_string())
        .env("RSYNC4J_RSYNC", program);
    if let Some(slow) = slow {
        command.env("MRS_D54_SLOW", slow.to_string());
    }
    if silent_peer {
        command.env("MRS_D31_SILENT_PEER", "1");
    }
    if let Some(elapsed) = elapsed_factor {
        command.env("MRS_D31_ELAPSED_FACTOR", elapsed.to_string());
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
    for file in ["pid", "ssh-pid", "held", "grandchild"] {
        if let Some(pid) = std::fs::read_to_string(root.join(file))
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
        {
            let pid = nix::unistd::Pid::from_raw(pid);
            // A stop leaves nothing the transfer started, on any host. U158's rsync ends by itself and is not stopped, so
            // what it leaves holding its pipes stays, as that test asserts.
            let answers_for = file == "pid" || child_test != "early_exit_receive_runs";
            if answers_for && running(pid) {
                survivors.push(file);
            }
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
        }
    }
    let output = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(status.is_some_and(|status| status.success()), "{output}");
    assert_eq!(survivors, Vec::<&str>::new(), "{output}");
    // Kotlin's diagnostic for its 30 s deadline, which D43 removes.
    assert!(
        !output.contains("Timeout of 30 seconds reached, terminating process..."),
        "{output}"
    );
}
/// Alive and not a zombie: an orphan that was killed waits for init to reap it, and answers signal 0 until then.
fn running(pid: nix::unistd::Pid) -> bool {
    nix::sys::signal::kill(pid, None).is_ok()
        && std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .is_ok_and(|output| {
                output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().starts_with('Z')
            })
}
fn script(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
/// A stand-in for rsync on a receive that says nothing, as rsync 3 does with an rsync 3 worker, and after `moving` seconds of
/// writing where a pull lands does `then`. A real 10 s for the limit, against writes every 0.2 s.
fn silent_receive(root: &Path, moving: u32, then: &str, mode: &str) {
    let program = root.join("rsync");
    script(
        &program,
        &format!(
            "#!/bin/bash\necho $$ > \"$MRS_D31_ROOT/pid\"\nmkdir -p \"$MRS_D31_ROOT/.rsync-partial\"\nfor i in $(seq 1 {}); do echo x >> \"$MRS_D31_ROOT/.rsync-partial/back\"; /bin/sleep 0.2; done\n{then}\n",
            moving * 5
        ),
    );
    run_stalled_send(StalledSend {
        factor: 30,
        child: "stalled_receive_runs",
        ..StalledSend::new(mode, &program, root)
    });
}
#[test]
fn a_silent_receive_that_stops_moving_is_stopped_d43() {
    let dir = tempfile::tempdir().unwrap();
    silent_receive(dir.path(), 1, "exec /bin/sleep 100037", "hung");
}
#[test]
fn a_silent_receive_that_keeps_moving_past_the_limit_finishes_d43() {
    let dir = tempfile::tempdir().unwrap();
    silent_receive(dir.path(), 15, "exit 0", "moving");
}
#[test]
fn a_stalled_receive_is_stopped_while_another_into_the_same_folder_keeps_writing_d43() {
    // Only the other pull's files move: its own name's temporary file and partial file, beside and under this one's.
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("rsync");
    script(
        &program,
        r#"#!/bin/bash
case "$*" in
  *127.0.0.1:/tmp/marathon/front*)
    mkdir -p "$MRS_D31_ROOT/.rsync-partial"
    until [ -e "$MRS_D31_ROOT/release" ]; do
      echo x >> "$MRS_D31_ROOT/.front.a1B2c3"; echo x >> "$MRS_D31_ROOT/.rsync-partial/front"; /bin/sleep 0.2
    done
    mv "$MRS_D31_ROOT/.front.a1B2c3" "$MRS_D31_ROOT/front"
    exit 0;;
esac
echo $$ > "$MRS_D31_ROOT/pid"
for i in 1 2 3; do echo x >> "$MRS_D31_ROOT/.back.a1B2c3"; /bin/sleep 0.2; done
exec /bin/sleep 100037
"#,
    );
    run_stalled_send(StalledSend {
        factor: 30,
        child: "two_receives_into_one_folder_runs",
        ..StalledSend::new("hung", &program, dir.path())
    });
}
/// Holds a stand-in on a send until the test has its next send waiting behind this one, which is what U163 is about. Only a
/// send's child writes `source`.
const WAIT_FOR_THE_QUEUE: &str =
    r#"until [ ! -e "$MRS_D31_ROOT/source" ] || [ -e "$MRS_D31_ROOT/queued" ]; do /bin/sleep 0.01; done"#;
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
{WAIT_FOR_THE_QUEUE}
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
/// `ps` that leaves what it listed in the run's directory, so that a stand-in can wait until the bridge has seen its children,
/// as it would have long before a real 300 s.
fn listing_ps(root: &Path) -> std::path::PathBuf {
    let real = real_tool("ps").expect("the bridge lists processes with ps");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    script(
        &bin.join("ps"),
        &format!(
            "#!/bin/sh\nout=$({} \"$@\")\nstatus=$?\nprintf '%s\\n' \"$out\" > \"$MRS_D31_ROOT/listing.$$\"\nmv \"$MRS_D31_ROOT/listing.$$\" \"$MRS_D31_ROOT/listing\"\nprintf '%s\\n' \"$out\"\nexit $status\n",
            real.display()
        ),
    );
    bin
}
/// Waits until a listing the bridge has read includes `pid`, a shell expression. A listing from before that process started
/// would not do: it can come first when the host is slow.
fn wait_till_listed(pid: &str) -> String {
    format!(
        r#"until [ -e "$MRS_D31_ROOT/listing" ] && awk -v p="{pid}" '$1 == p {{ found = 1 }} END {{ exit !found }}' "$MRS_D31_ROOT/listing"; do /bin/sleep 0.01; done"#
    )
}
/// rsync 3's way: a child that stands in for ssh, a hung one that keeps the pipes open past rsync's exit, then after the limit
/// the report and exit 30.
fn exits_and_leaves_ssh() -> String {
    format!(
        "/bin/sleep 100037 &\necho $! > \"$MRS_D31_ROOT/held\"\n{}\n{PAST_THE_LIMIT}\necho '[sender] io timeout after 300 seconds -- exiting' >&2\nexit 30",
        wait_till_listed("$!")
    )
}
#[test]
fn a_stalled_send_that_exits_while_ssh_holds_its_stderr_returns_and_leaves_no_ssh_d31() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake_rsync(dir.path(), &exits_and_leaves_ssh());
    let bin = listing_ps(dir.path());
    stalled_send_child("hung", &program, Some(&bin), dir.path());
}
#[test]
fn a_stalled_send_under_sshpass_that_exits_leaves_no_ssh_d31() {
    // `sshpass` follows rsync out at once, and what rsync started is nobody's child by the time the bridge stops it.
    let dir = tempfile::tempdir().unwrap();
    let program = fake_rsync(dir.path(), &exits_and_leaves_ssh());
    let bin = listing_ps(dir.path());
    let sshpass = dir.path().join("sshpass");
    script(&sshpass, FAKE_SSHPASS);
    run_stalled_send(StalledSend {
        sshpass: Some(&sshpass),
        extra_path: Some(&bin),
        factor: 100,
        ..StalledSend::new("hung", &program, dir.path())
    });
}
/// Long enough at the default factor for the 285 s with no progress that a stall needs, with room for a slow host: a stand-in
/// that ends by itself must do so after it.
const PAST_THE_LIMIT: &str = "/bin/sleep 6";
#[test]
fn a_send_that_exits_with_rsyncs_timeout_status_stalled_and_leaves_nothing_d31() {
    // rsync 3's own status for `--timeout`, whatever it printed. The stall is decided once rsync has exited, and what it
    // started goes too.
    let dir = tempfile::tempdir().unwrap();
    let program = fake_rsync(
        dir.path(),
        &format!(
            "/bin/sleep 100037 &\necho $! > \"$MRS_D31_ROOT/held\"\n{}\n{PAST_THE_LIMIT}\necho 'rsync error: timeout in data send/receive (code 30) at io.c(201) [sender=3.4.1]' >&2\nexit 30",
            wait_till_listed("$!")
        ),
    );
    let bin = listing_ps(dir.path());
    stalled_send_child("hung", &program, Some(&bin), dir.path());
}
#[test]
fn a_stall_is_measured_on_the_clock_while_the_bridge_is_not_scheduled_d31() {
    // The bridge's delays come ten times late, as on a host that leaves it unscheduled: 6 s are 600 s on its clock, but
    // only 60 one-second delays. rsync 3 reports and exits after its limit, and the stall must still count.
    let dir = tempfile::tempdir().unwrap();
    let program = fake_rsync(dir.path(), &exits_and_leaves_ssh());
    let bin = listing_ps(dir.path());
    run_stalled_send(StalledSend {
        extra_path: Some(&bin),
        factor: 10,
        elapsed_factor: Some(100),
        ..StalledSend::new("hung", &program, dir.path())
    });
}
#[test]
fn a_send_stopped_at_rsyncs_requested_limit_keeps_its_retries_d31() {
    // `--stop-after` and `--stop-at` end with 30 as well.
    let dir = tempfile::tempdir().unwrap();
    let said = "[sender] stopping at requested limit\nrsync error: timeout in data send/receive (code 30) at io.c(201) [sender=3.4.1]\n";
    std::fs::write(dir.path().join("said"), said).unwrap();
    std::fs::write(dir.path().join("said-raw"), said).unwrap();
    let program = fake_rsync(
        dir.path(),
        &format!("{PAST_THE_LIMIT}\ncat \"$MRS_D31_ROOT/said-raw\" >&2\nexit 30"),
    );
    stalled_send_child("other", &program, None, dir.path());
}
#[test]
fn a_failed_send_that_names_the_timeout_report_keeps_its_retries_d31() {
    // The report's words in a file name, or another I/O error, with rsync's other statuses and ssh's. A failure that comes at
    // once is never a stall, even with the report as a whole line, as a file name with newlines can make it.
    for (said, status) in [
        (
            "rsync(9): error: x\nrsync(1): error: poll: timeout\n: stat: No such file or directory\n",
            23,
        ),
        ("[sender] io timeout after 300 seconds -- exiting\n", 30),
        (
            "rsync: link_stat \"/tmp/marathon/rsync(1): error: poll: timeout\" failed: No such file or directory (2)\nrsync error: some files/attrs were not transferred (see previous errors) (code 23) at main.c(1338) [sender=3.4.1]\n",
            23,
        ),
        (
            "rsync: [sender] link_stat \"/tmp/marathon/[sender] io timeout after 300 seconds -- exiting\" failed: No such file or directory (2)\n",
            23,
        ),
        (
            "rsync(4): error: /tmp/marathon/poll: timeout: open: No such file or directory\n",
            1,
        ),
        (
            "rsync: [sender] write error: Broken pipe (32)\nrsync error: error in rsync protocol data stream (code 12) at io.c(232) [sender=3.4.1]\n",
            12,
        ),
        (
            "ssh: connect to host 127.0.0.1 port 22: Operation timed out\r\nrsync: connection unexpectedly closed (0 bytes received so far) [sender]\n",
            255,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        // What the bridge logs: lines as Java's reader has them.
        std::fs::write(dir.path().join("said"), said.replace("\r\n", "\n")).unwrap();
        let program = fake_rsync(
            dir.path(),
            &format!("cat \"$MRS_D31_ROOT/said-raw\" >&2\nexit {status}"),
        );
        std::fs::write(dir.path().join("said-raw"), said).unwrap();
        stalled_send_child("other", &program, None, dir.path());
    }
}
#[test]
fn a_failed_send_without_a_stall_report_keeps_its_message_and_stderr_d31() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake_rsync(dir.path(), "echo 'rsync: some other failure' >&2\nexit 23");
    stalled_send_child("other", &program, None, dir.path());
}
/// What `sshpass` 1.06 does on TERM, measured with the real one: it exits at once, and rsync and ssh stay. See the real test.
/// Left alone, it exits with rsync's status.
const FAKE_SSHPASS: &str = "#!/bin/bash\nshift 2\ntrap 'exit 143' TERM\n\"$@\" &\nwait $!\n";
/// U181: what `sshpass` 1.10 does on TERM before ssh has prompted, measured with the real one: it exits 0 at once, whatever
/// rsync does.
const SSHPASS_1_10: &str = "#!/bin/bash\nshift 2\ntrap 'exit 0' TERM\n\"$@\" &\nwait $!\n";
#[test]
fn a_stalled_send_under_sshpass_1_10_is_still_a_stall_u181() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake_rsync(
        dir.path(),
        "echo 'rsync(1): error: poll: timeout' >&2\nexec /bin/sleep 100037",
    );
    let sshpass = dir.path().join("sshpass");
    script(&sshpass, SSHPASS_1_10);
    run_stalled_send(StalledSend {
        sshpass: Some(&sshpass),
        factor: 100,
        ..StalledSend::new("hung", &program, dir.path())
    });
}
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
        factor: 100,
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
        factor: 100,
        ..StalledSend::new("hung", &program, dir.path())
    });
}
#[test]
fn rsync_under_sshpass_gets_its_step_after_term_d31() {
    // An sshpass gone at once must not cost rsync the step before KILL: 0.5 s at this factor against 0.05 s here.
    let dir = tempfile::tempdir().unwrap();
    let program = fake_rsync(
        dir.path(),
        "trap '/bin/sleep 0.05; echo finished > \"$MRS_D31_ROOT/handler\"; exit 143' TERM\necho 'rsync(1): error: poll: timeout' >&2\nwhile :; do /bin/sleep 0.05; done",
    );
    let sshpass = dir.path().join("sshpass");
    script(&sshpass, FAKE_SSHPASS);
    run_stalled_send(StalledSend {
        sshpass: Some(&sshpass),
        factor: 10,
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
/// The clock is compressed so that rsync's report, 5 s in, comes after the 285 s a stall needs; the bridge stops ssh with rsync.
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
    let bin = listing_ps(dir.path());
    // `exec` keeps the pid, so what is recorded is the real ssh. On a stop it waits until the bridge has listed it, as it
    // would have long before a real 300 s; rsync 3 then reports and exits, and leaves this ssh to the bridge.
    let wait = if child == "early_exit_receive_runs" {
        String::new()
    } else {
        wait_till_listed("$$")
    };
    script(
        &bin.join("ssh"),
        &format!(
            "#!/bin/sh\necho $$ > \"$MRS_D31_ROOT/ssh-pid\"\n{wait}\nexec {} \"$@\"\n",
            ssh.display()
        ),
    );
    // That test kills rsync while its ssh is up, so that rsync must not report first. Otherwise rsync reports well after the
    // bridge's first listing at 1 s, which is what lets it answer for an ssh that rsync 3 leaves.
    let timeout = if child == "early_exit_receive_runs" { 300 } else { 5 };
    let program = dir.path().join("rsync");
    script(
        &program,
        &format!(
            r#"#!/bin/bash
case "$*" in *127.0.0.1:/tmp/marathon/next*) echo next >> "$MRS_D31_ROOT/order"; exit 0;; esac
echo $$ > "$MRS_D31_ROOT/pid"
echo stalled >> "$MRS_D31_ROOT/order"
{WAIT_FOR_THE_QUEUE}
args=()
for arg in "$@"; do
  case "$arg" in --timeout=300) args+=(--timeout={timeout});; *) args+=("$arg");; esac
done
exec {} "${{args[@]}}"
"#,
            rsync.display()
        ),
    );
    run_stalled_send(StalledSend {
        extra_path: Some(&bin),
        sshpass,
        factor: if child == "early_exit_receive_runs" { 1 } else { 100 },
        silent_peer: true,
        child,
        ..StalledSend::new("hung", &program, dir.path())
    });
}
#[test]
fn the_real_rsync_gives_up_on_a_hung_ssh_d31() {
    real_rsync_with_a_hung_ssh(None, "stalled_send_runs");
}
/// The `sshpass` on this host's `PATH`, so that a build of another version can be tried, else Homebrew's.
fn real_sshpass() -> Option<std::path::PathBuf> {
    real_tool("sshpass")
        .or_else(|| Some(std::path::PathBuf::from("/opt/homebrew/bin/sshpass")).filter(|path| path.exists()))
}
#[test]
fn the_real_rsync_under_the_real_sshpass_gives_up_on_a_hung_ssh_d31() {
    match real_sshpass() {
        Some(sshpass) => real_rsync_with_a_hung_ssh(Some(&sshpass), "stalled_send_runs"),
        None => eprintln!("skipped: no sshpass on this host"),
    }
}
#[test]
fn the_real_rsync_is_stopped_when_a_receive_stalls_d43() {
    real_rsync_with_a_hung_ssh(None, "stalled_receive_runs");
}
#[test]
fn the_real_rsync_under_the_real_sshpass_is_stopped_when_a_receive_stalls_d43() {
    match real_sshpass() {
        Some(sshpass) => real_rsync_with_a_hung_ssh(Some(&sshpass), "stalled_receive_runs"),
        None => eprintln!("skipped: no sshpass on this host"),
    }
}
/// A stand-in for rsync on a receive that records its pid, reports a stall as openrsync does, and does `body`, under a
/// stand-in for `sshpass` if one is given.
fn stalled_receive(root: &Path, body: &str, sshpass: Option<&str>, extra_path: Option<&Path>) {
    let program = root.join("rsync");
    script(
        &program,
        &format!(
            "#!/bin/bash\ncase \"$*\" in *--timeout=300*) ;; *) echo missing timeout >&2; exit 99;; esac\necho $$ > \"$MRS_D31_ROOT/pid\"\necho 'rsync(1): error: poll: timeout' >&2\n{body}\n"
        ),
    );
    let fake_sshpass = root.join("sshpass");
    script(&fake_sshpass, sshpass.unwrap_or(FAKE_SSHPASS));
    run_stalled_send(StalledSend {
        sshpass: sshpass.is_some().then_some(fake_sshpass.as_path()),
        extra_path,
        factor: 100,
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
fn a_stalled_receive_that_ignores_term_is_killed_d43() {
    let dir = tempfile::tempdir().unwrap();
    stalled_receive(dir.path(), IGNORES_TERM, None, None);
}
#[test]
fn a_stalled_receive_under_sshpass_also_stops_rsync_d43() {
    let dir = tempfile::tempdir().unwrap();
    stalled_receive(dir.path(), "exec /bin/sleep 100037", Some(FAKE_SSHPASS), None);
}
#[test]
fn a_stalled_receive_under_sshpass_1_10_returns_false_u181() {
    let dir = tempfile::tempdir().unwrap();
    stalled_receive(dir.path(), "exec /bin/sleep 100037", Some(SSHPASS_1_10), None);
}
#[test]
fn rsync_that_ignores_term_on_a_stalled_receive_under_sshpass_is_killed_d43() {
    let dir = tempfile::tempdir().unwrap();
    stalled_receive(dir.path(), IGNORES_TERM, Some(FAKE_SSHPASS), None);
}
/// A stand-in for ssh under the rsync stand-in, which ignores TERM as a wedged one might.
const SSH_IGNORING_TERM: &str = "(trap '' TERM; exec /bin/sleep 100039) &\necho $! > \"$MRS_D31_ROOT/grandchild\"\n";
#[test]
fn a_stalled_receive_whose_ssh_ignores_term_leaves_nothing_d43() {
    for sshpass in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("rsync");
        // Started before the report, as ssh is, and so in the listing at the stop.
        script(
            &program,
            &format!(
                "#!/bin/bash\necho $$ > \"$MRS_D31_ROOT/pid\"\n{SSH_IGNORING_TERM}echo 'rsync(1): error: poll: timeout' >&2\nexec /bin/sleep 100037\n"
            ),
        );
        let fake_sshpass = dir.path().join("sshpass");
        script(&fake_sshpass, FAKE_SSHPASS);
        run_stalled_send(StalledSend {
            sshpass: sshpass.then_some(fake_sshpass.as_path()),
            factor: 100,
            child: "stalled_receive_runs",
            ..StalledSend::new("hung", &program, dir.path())
        });
    }
}
#[test]
fn a_stalled_receive_that_exits_as_it_reports_leaves_nothing_d43() {
    // rsync reports and exits at once, so its exit can be seen before its report is read: the stall is decided afterwards.
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("rsync");
    script(
        &program,
        &format!(
            "#!/bin/bash\necho $$ > \"$MRS_D31_ROOT/pid\"\n{SSH_IGNORING_TERM}{}\n{PAST_THE_LIMIT}\necho 'rsync(1): error: poll: timeout' >&2\nexit 23\n",
            wait_till_listed("$!")
        ),
    );
    let bin = listing_ps(dir.path());
    run_stalled_send(StalledSend {
        extra_path: Some(&bin),
        child: "stalled_receive_runs",
        ..StalledSend::new("hung", &program, dir.path())
    });
}
#[test]
fn a_stalled_receive_under_sshpass_that_exits_leaves_no_ssh_d43() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("rsync");
    script(
        &program,
        &format!(
            "#!/bin/bash\necho $$ > \"$MRS_D31_ROOT/pid\"\n{SSH_IGNORING_TERM}{}\n{PAST_THE_LIMIT}\necho '[Receiver] io timeout after 300 seconds -- exiting' >&2\nexit 30\n",
            wait_till_listed("$!")
        ),
    );
    let bin = listing_ps(dir.path());
    let fake_sshpass = dir.path().join("sshpass");
    script(&fake_sshpass, FAKE_SSHPASS);
    run_stalled_send(StalledSend {
        sshpass: Some(&fake_sshpass),
        extra_path: Some(&bin),
        factor: 100,
        child: "stalled_receive_runs",
        ..StalledSend::new("hung", &program, dir.path())
    });
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
        match std::env::var("MRS_D54_SLOW") {
            Ok(slow) => Arc::new(SlowClock::new(slow.parse().unwrap())) as Arc<dyn marathon_core::runtime::VendorClock>,
            Err(_) => Arc::new(ssh::ScaledClock::lagging(
                factor,
                std::env::var("MRS_D31_ELAPSED_FACTOR").map_or(factor, |elapsed| elapsed.parse().unwrap()),
            )),
        },
        logging,
    )
    .await
    .unwrap();
    (bridge, silent_peer)
}
/// D43: a receive that reports a stall returns false, not an error, and the process it started is gone when it returns.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_receive_runs() {
    let Ok(mode) = std::env::var("MRS_D31_MODE") else {
        return;
    };
    let root = std::path::PathBuf::from(std::env::var_os("MRS_D31_ROOT").unwrap());
    let (logging, events) = ssh::logging(false);
    let (bridge, _silent_peer) = stalled_bridge(logging).await;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        bridge.receive("/tmp/marathon/stuck", &root.join("back")),
    )
    .await
    .unwrap();
    if mode == "moving" {
        assert_eq!(result, Ok(true));
        bridge.join_pending().await.unwrap();
        return;
    }
    assert_eq!(result, Ok(false));
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
    let lines = ssh::lines(&events);
    assert!(
        lines
            .iter()
            .any(|(_, level, message)| *level == jcompat::logging::Level::Error
                && message.starts_with(
                    "receive error: no progress in 300 s receiving 127.0.0.1:/tmp/marathon/stuck, rsync stopped: "
                )),
        "{lines:?}"
    );
    bridge.join_pending().await.unwrap();
}
/// D43: two pulls into one folder, `back` stalled and `front` writing until the test lets it finish. `back` is stopped at its
/// limit while `front` is still writing, and `front` then finishes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_receives_into_one_folder_runs() {
    if std::env::var("MRS_D31_MODE").is_err() {
        return;
    }
    let root = std::path::PathBuf::from(std::env::var_os("MRS_D31_ROOT").unwrap());
    let (logging, _events) = ssh::logging(false);
    let (bridge, _silent_peer) = stalled_bridge(logging).await;
    let bridge = Arc::new(bridge);
    let front = {
        let (bridge, root) = (bridge.clone(), root.clone());
        tokio::spawn(async move { bridge.receive("/tmp/marathon/front", &root.join("front")).await })
    };
    let back = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        bridge.receive("/tmp/marathon/back", &root.join("back")),
    )
    .await
    .expect("the stalled pull was kept going by the other one's writes");
    assert_eq!(back, Ok(false));
    assert!(!front.is_finished());
    std::fs::write(root.join("release"), b"").unwrap();
    let front = tokio::time::timeout(std::time::Duration::from_secs(60), front)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(front, Ok(true));
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
    let (logging, events) = ssh::logging(true);
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
    // Queued behind the stuck send by the per-bridge mutex, and in the queue before the stand-in may report.
    let queued = {
        let (bridge, source) = (bridge.clone(), source.clone());
        tokio::spawn(async move { bridge.send(&source, "/tmp/marathon/next").await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while bridge.retained_tasks() < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    std::fs::write(root.join("queued"), b"").unwrap();
    let finished = async { (first.await.unwrap(), queued.await.unwrap()) };
    let (first, queued) = tokio::time::timeout(std::time::Duration::from_secs(60), finished)
        .await
        .unwrap();
    if mode == "hung" {
        // U163: its own outcome, and what waited behind it gets the same without starting a process.
        for result in [&first, &queued] {
            let error = result.as_ref().unwrap_err();
            assert!(
                matches!(
                    error,
                    VendorError::LeavePool {
                        reason: LeavePoolReason::TransferStalled,
                        ..
                    }
                ),
                "{error:?}"
            );
            assert_eq!(
                error.failure().kind.legacy_class(),
                TRANSFER_STALLED.legacy_class().to_owned()
            );
        }
        assert_eq!(std::fs::read_to_string(root.join("order")).unwrap(), "stalled\n");
        // A send that starts later waited behind nothing, so it tries: the worker may have come back.
        assert_eq!(bridge.send(&source, "/tmp/marathon/next").await, Ok(true));
    } else {
        // Any other failure stays the plain `false` the installer retries, and the next send runs.
        assert_eq!((first, queued), (Ok(false), Ok(true)));
    }
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
    assert_eq!(
        lines
            .iter()
            .any(|(_, level, message)| *level == jcompat::logging::Level::Warn
                && message == "Not sending to /tmp/marathon/next: a send to this worker stalled while this one waited"),
        mode == "hung",
        "{lines:?}"
    );
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
        let said = std::fs::read_to_string(root.join("said")).unwrap_or_else(|_| "rsync: some other failure\n".into());
        assert_eq!(errors[0], format!("send error: {said}"));
    }
    bridge.join_pending().await.unwrap();
}

/// D43's real runs: real rsync and real ssh, through the fake worker's real `rsync --server`, behind a throttle in this test.
/// Under `sshpass` the worker's real password authentication is what it answers. `Throttle::stall_after` stops the bytes and
/// holds the sockets, so that nothing tells ssh the link is gone. No keep-alive here, so it is rsync's own limit that ends it.
#[derive(Clone, Copy)]
struct Throttle {
    bytes_per_tick: usize,
    stall_after: Option<usize>,
}
const THROTTLE_TICK: std::time::Duration = std::time::Duration::from_millis(100);
async fn forward(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    throttle: Option<Throttle>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buffer = vec![0; throttle.map_or(64 * 1024, |t| t.bytes_per_tick)];
    let mut total = 0;
    loop {
        if throttle.and_then(|t| t.stall_after).is_some_and(|limit| total >= limit) {
            // Holds both halves, so that the peer sees silence and not the end of the connection.
            std::future::pending::<()>().await;
        }
        let Ok(count) = from.read(&mut buffer).await else {
            return;
        };
        if count == 0 || to.write_all(&buffer[..count]).await.is_err() {
            return;
        }
        total += count;
        if throttle.is_some() {
            tokio::time::sleep(THROTTLE_TICK).await;
        }
    }
}
async fn throttled_listener(upstream: u16, throttle: Throttle) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((client, _)) = listener.accept().await {
            let server = tokio::net::TcpStream::connect(("127.0.0.1", upstream)).await.unwrap();
            let (client_read, client_write) = client.into_split();
            let (server_read, server_write) = server.into_split();
            tokio::spawn(forward(client_read, server_write, None));
            tokio::spawn(forward(server_read, client_write, Some(throttle)));
        }
    });
    port
}
/// One real pull in a child process, since rsync's path is process-wide. `io_timeout` is the host rsync's `--timeout` on a
/// stalled run: `bin/rsync` asks for it and `bin/ssh` gives the worker back the 300 an rsync 3 passes on, which is all its
/// router allows.
fn real_pull(sshpass: bool, io_timeout: Option<u32>) {
    let shims = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/fake-worker/shims")
        .canonicalize()
        .unwrap();
    let Some(real_rsync) = real_tool("rsync") else {
        eprintln!("skipped: this host has no rsync");
        return;
    };
    let real_ssh =
        real_tool("ssh").filter(|_| Path::new("/usr/bin/ssh").exists() && Path::new("/usr/bin/ssh-keygen").exists());
    let Some(real_ssh) = real_ssh else {
        eprintln!("skipped: this host has no /usr/bin/ssh and /usr/bin/ssh-keygen");
        return;
    };
    let real_password = real_sshpass();
    if sshpass && real_password.is_none() {
        eprintln!("skipped: no sshpass on this host");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let mut program = shims.join("rsync");
    if let Some(seconds) = io_timeout {
        program = bin.join("rsync");
        script(
            &program,
            &format!(
                r#"#!/bin/bash
echo $$ > "$MRS_D31_ROOT/pid"
args=()
for arg in "$@"; do
  case "$arg" in --timeout=300) args+=(--timeout={seconds});; *) args+=("$arg");; esac
done
exec {} "${{args[@]}}"
"#,
                shims.join("rsync").display()
            ),
        );
        script(
            &bin.join("ssh"),
            &format!(
                r#"#!/bin/bash
args=()
for arg in "$@"; do
  case "$arg" in --timeout={seconds}) args+=(--timeout=300);; *) args+=("$arg");; esac
done
exec {} "${{args[@]}}"
"#,
                shims.join("ssh").display()
            ),
        );
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "real_pull_child", "--nocapture"])
        .env("MRS_D43_ROOT", dir.path())
        .env("MRS_D31_ROOT", dir.path())
        .env("RSYNC4J_RSYNC", &program)
        .env("RSYNC4J_SSHPASS", shims.join("sshpass"))
        .env("FW_REAL_RSYNC", real_rsync)
        .env("FW_REAL_SSH", real_ssh)
        .env("FW_ARGV_LOG", dir.path().join("argv.jsonl"))
        .env(
            "PATH",
            format!(
                "{}:{}:{}",
                bin.display(),
                shims.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    if let Some(path) = real_password {
        command.env("FW_REAL_SSHPASS", path);
    }
    if sshpass {
        command.env("MRS_D43_PASSWORD", "1");
    }
    if io_timeout.is_some() {
        command.env("MRS_D43_STALL", "1");
    }
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let argv = std::fs::read_to_string(dir.path().join("argv.jsonl")).unwrap();
    assert!(argv.contains("--timeout="), "{argv}");
    assert_eq!(argv.contains("sshpass"), sshpass, "{argv}");
}
#[test]
fn a_pull_longer_than_the_old_deadline_finishes_while_it_moves_d43() {
    real_pull(false, None);
}
#[test]
fn a_pull_longer_than_the_old_deadline_finishes_under_sshpass_while_it_moves_d43() {
    real_pull(true, None);
}
/// x50: the host rsync's `--timeout` on a stalled pull. The worker's rsync is `/usr/bin/rsync`, which on macOS is
/// openrsync, and rsync 3 on the host reports against that, 1.5 times its `--timeout` after the bytes stop, as measured
/// (x50 findings). On the pull's clock, compressed by 20, the report has to come after the 285 s before it counts, and
/// the bridge's own look at the files stops the pull 15 s after they stopped. 20 s is past both. 2 s was enough only for
/// an rsync that never reports, or that reports and then waits for ssh, as openrsync does: an rsync 3 that reports and
/// exits after 2 s failed too soon to stall.
const STALLED_PULL_TIMEOUT: u32 = 20;
#[test]
fn a_pull_that_stops_moving_is_stopped_and_leaves_its_partial_file_d43() {
    real_pull(false, Some(STALLED_PULL_TIMEOUT));
}
#[test]
fn a_pull_that_stops_moving_under_sshpass_is_stopped_and_leaves_its_partial_file_d43() {
    real_pull(true, Some(STALLED_PULL_TIMEOUT));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_pull_child() {
    let Some(root) = std::env::var_os("MRS_D43_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let stall = std::env::var_os("MRS_D43_STALL").is_some();
    let fake = ssh::fake_with(ssh::suite_script(), 8, |config| {
        config.auth.keyboard_interactive = false
    })
    .await;
    // 320 KiB/s for 10.5 MiB is about 34 s, past the 30 s total a receive used to have. A stalled run gets a tenth of that.
    let tick = 32 * 1024;
    let size = 11 * 1024 * 1024;
    let payload: Vec<u8> = (0..size as u64)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    fake.worker.fs.mkdir("/tmp/marathon").await.unwrap();
    fake.worker
        .fs
        .write("/tmp/marathon/video.bin", payload.clone(), 0o600)
        .await
        .unwrap();
    let port = throttled_listener(
        fake.port(),
        Throttle {
            bytes_per_tick: tick,
            stall_after: stall.then_some(size / 10),
        },
    )
    .await;
    let auth = if std::env::var_os("MRS_D43_PASSWORD").is_some() {
        // No key is offered, so the password the fake worker checks is what `sshpass` types.
        SshAuthentication::Password {
            username: "fixture".into(),
            password: "fake-password".into(),
        }
    } else {
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
        key(private.to_str().unwrap())
    };
    let (logging, events) = ssh::logging(false);
    // A stalled run compresses the clock: between two rsync 3 installs, as on a Linux host, nothing but the 300 s with
    // nothing arriving ends it. It stops the transfer either way, and a run that moves stays on real time.
    let clock: Arc<dyn marathon_core::runtime::VendorClock> = if stall {
        Arc::new(ssh::ScaledClock::new(20))
    } else {
        Arc::new(Clock::default())
    };
    let bridge = RsyncFileBridge::new(
        RsyncTarget {
            addr: "127.0.0.1".into(),
            port: i32::from(port),
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
        Some(auth),
        Arc::new(Runtime),
        clock,
        logging,
    )
    .await
    .unwrap();
    let destination = root.join("video.bin");
    let started = std::time::Instant::now();
    let denied = || {
        ssh::lines(&events)
            .iter()
            .any(|(.., message)| message.contains("Permission denied"))
    };
    let mut result = bridge.receive("/tmp/marathon/video.bin", &destination).await;
    // `sshpass` on macOS sometimes leaves ssh without a terminal to ask on (`can't open /dev/tty`, see the findings), which is
    // an ordinary failed attempt and not what this test is about.
    for _ in 0..20 {
        if result != Ok(false) || !denied() {
            break;
        }
        events.lock().unwrap().clear();
        result = bridge.receive("/tmp/marathon/video.bin", &destination).await;
    }
    let elapsed = started.elapsed();
    bridge.join_pending().await.unwrap();
    let lines = ssh::lines(&events);
    let used_password = fake
        .transcript()
        .await
        .iter()
        .any(|event| event["detail"]["method"] == "password" && event["detail"]["accepted"] == true);
    assert_eq!(used_password, std::env::var_os("MRS_D43_PASSWORD").is_some());
    if stall {
        assert_eq!(result, Ok(false), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|(.., message)| message.starts_with("receive error: no progress in 300 s receiving ")),
            "{lines:?}"
        );
        assert!(!destination.exists());
        // rsync 3 saves its partial file on TERM. On this clock KILL can get there first, as it can't with the real 5 s.
        assert!(
            !cfg!(target_os = "macos") || root.join(".rsync-partial").is_dir(),
            "{lines:?}"
        );
        // rsync is gone: the child under `bin/rsync` keeps its pid through the `exec`s. An orphan under `sshpass` is reaped soon.
        let pid: i32 = std::fs::read_to_string(root.join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let pid = nix::unistd::Pid::from_raw(pid);
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            while nix::sys::signal::kill(pid, None) != Err(nix::errno::Errno::ESRCH) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    } else {
        assert_eq!(result, Ok(true), "{lines:?}");
        assert!(
            elapsed.as_secs() >= 30,
            "throttle too fast to mean anything: {elapsed:?}"
        );
        assert_eq!(std::fs::read(&destination).unwrap(), payload);
        assert!(!root.join(".rsync-partial").exists());
    }
}

/// D54: delays `slow` times longer than they say, on a real clock otherwise. A step the bridge waits out before it sends TERM
/// then takes far longer than the 60 s that catches a hang, so a stop that returns at all sent TERM at once.
struct SlowClock {
    slow: i64,
    inner: Clock,
    epoch: std::time::Instant,
}
impl SlowClock {
    fn new(slow: i64) -> Self {
        Self {
            slow,
            inner: Clock::default(),
            epoch: std::time::Instant::now(),
        }
    }
}
impl marathon_core::runtime::VendorClock for SlowClock {
    fn now_millis(&self) -> i64 {
        self.epoch.elapsed().as_millis() as i64
    }
    fn delay(
        &self,
        millis: i64,
        cancellation: Arc<dyn marathon_core::runtime::CancellationSignal>,
    ) -> marathon_core::runtime::VendorFuture<'_, ()> {
        self.inner.delay(millis.saturating_mul(self.slow), cancellation)
    }
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn marathon_core::runtime::CancellationSignal>,
        millis: i64,
        body: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> marathon_core::runtime::VendorFuture<'a, ()> {
        self.inner.with_timeout(parent, millis.saturating_mul(self.slow), body)
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn marathon_core::runtime::CancellationSignal>,
        millis: i64,
        body: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> marathon_core::runtime::VendorFuture<'a, Option<()>> {
        self.inner
            .with_timeout_or_null(parent, millis.saturating_mul(self.slow), body)
    }
}
/// D54: the 5 s step before TERM that D31 gives a stalled rsync would take 150 s on this clock.
const SLOW: i64 = 30;
/// D54: a stand-in that records its pid and starts `ssh`, a child standing in for rsync's ssh, which says `ssh-ready` once it
/// has set up its own signal handling. Then the stand-in sets up its own, `body.0`, says `ready`, and does `body.1`. The test
/// stops it only after `ready`, so the stop meets the handling each test is about (review round 2). A send to `.../next`
/// succeeds at once, so that a send queued behind a stopped one shows whether it went on.
fn stoppable_rsync(root: &Path, ssh: &str, body: (&str, &str)) -> std::path::PathBuf {
    let program = root.join("rsync");
    let (handling, then) = body;
    script(
        &program,
        &format!(
            r#"#!/bin/bash
case "$*" in *127.0.0.1:/tmp/marathon/next*) echo next >> "$MRS_D31_ROOT/order"; exit 0;; esac
echo $$ > "$MRS_D31_ROOT/pid"
echo stuck >> "$MRS_D31_ROOT/order"
{ssh} &
echo $! > "$MRS_D31_ROOT/ssh-pid"
until [ -e "$MRS_D31_ROOT/ssh-ready" ]; do /bin/sleep 0.01; done
{handling}
echo > "$MRS_D31_ROOT/ready"
{then}
"#
        ),
    );
    program
}
const SSH: &str = r#"(echo > "$MRS_D31_ROOT/ssh-ready"; exec /bin/sleep 100043)"#;
/// A child that ignores TERM, as an ssh can, so that only KILL ends it.
const SSH_IGNORING_TERM_TOO: &str = r#"(trap '' TERM; echo > "$MRS_D31_ROOT/ssh-ready"; exec /bin/sleep 100044)"#;
const WAITS: (&str, &str) = ("", "wait");
const IGNORES_TERM_AND_WAITS: (&str, &str) = ("trap '' TERM", "while :; do /bin/sleep 0.05; done");
/// A wrapper that reports success when it is stopped.
const EXITS_0_ON_TERM: (&str, &str) = ("trap 'exit 0' TERM", "while :; do /bin/sleep 0.05; done");
/// D54: a transfer stopped mid-way. An rsync that ends on TERM runs on the slow clock; one that ignores TERM on a real one, so
/// that its KILL comes after D31's real 5 s. The parent checks that nothing the transfer started is left.
fn stopped_transfer(mode: &str, ssh: &str, body: (&str, &str)) {
    stopped_transfer_under(None, mode, ssh, body);
}
/// `stopped_transfer` under a stand-in for `sshpass`, if one is given.
fn stopped_transfer_under(sshpass: Option<&str>, mode: &str, ssh: &str, body: (&str, &str)) {
    let dir = tempfile::tempdir().unwrap();
    let program = stoppable_rsync(dir.path(), ssh, body);
    let fake_sshpass = dir.path().join("sshpass");
    if let Some(text) = sshpass {
        script(&fake_sshpass, text);
    }
    run_stalled_send(StalledSend {
        factor: 1,
        slow: (ssh == SSH && body == WAITS).then_some(SLOW),
        sshpass: sshpass.is_some().then_some(fake_sshpass.as_path()),
        child: "stopped_transfer_runs",
        ..StalledSend::new(mode, &program, dir.path())
    });
}
#[test]
fn a_send_cancelled_mid_transfer_stops_rsync_at_once_and_the_send_behind_it_goes_on_d54() {
    stopped_transfer("cancel-send", SSH, WAITS);
}
#[test]
fn a_cancelled_send_whose_rsync_ignores_term_is_killed_d54() {
    stopped_transfer("cancel-send-stubborn", SSH, IGNORES_TERM_AND_WAITS);
}
/// Review round 1's witness: rsync's status after the stop doesn't decide whether what it started is ended.
#[test]
fn a_cancelled_send_whose_rsync_exits_0_on_term_still_ends_what_it_started_d54() {
    stopped_transfer("cancel-send-exits-0", SSH_IGNORING_TERM_TOO, EXITS_0_ON_TERM);
}
#[test]
fn a_send_whose_connection_is_lost_fails_stops_rsync_and_the_send_behind_it_goes_on_d54() {
    stopped_transfer("lost-send", SSH, WAITS);
}
#[test]
fn a_receive_whose_connection_is_lost_returns_false_and_stops_rsync_d54() {
    stopped_transfer("lost-receive", SSH, WAITS);
}
#[test]
fn a_cancelled_send_under_sshpass_also_stops_rsync_d54() {
    stopped_transfer_under(Some(FAKE_SSHPASS), "cancel-send", SSH, WAITS);
}
#[test]
fn a_cancelled_send_under_sshpass_1_10_is_cancelled_u181() {
    stopped_transfer_under(Some(SSHPASS_1_10), "cancel-send", SSH, WAITS);
}
#[test]
fn a_receive_under_sshpass_1_10_whose_connection_is_lost_returns_false_u181() {
    stopped_transfer_under(Some(SSHPASS_1_10), "lost-receive", SSH, WAITS);
}
/// D54's success for an rsync that exits 0 on TERM stays under `sshpass`, since `sshpass` now reports rsync's own status.
#[test]
fn a_cancelled_send_under_sshpass_1_10_whose_rsync_exits_0_on_term_keeps_its_success_u181() {
    stopped_transfer_under(
        Some(SSHPASS_1_10),
        "cancel-send-exits-0",
        SSH_IGNORING_TERM_TOO,
        EXITS_0_ON_TERM,
    );
}
/// D54's child: `MRS_D31_MODE` is `cancel` or `lost`, then `send` or `receive`, and `-stubborn` for an rsync that ignores
/// TERM or `-exits-0` for one that exits 0 on it. The transfer returns its stop: on the slow clock at all only if TERM came
/// at once, and after D31's 5 s and KILL for an rsync that ignores it. A send queued behind it on the bridge's lock then
/// runs. Receives aren't cancelled by the devices, but the bridge treats a loss on them as on a send.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopped_transfer_runs() {
    use marathon_apple::cmd::{TransferStop, failure, remote::ssh::sshj::error::CONNECTION_EXCEPTION};
    const HANG: std::time::Duration = std::time::Duration::from_secs(60);
    let Ok(mode) = std::env::var("MRS_D31_MODE") else {
        return;
    };
    let root = std::path::PathBuf::from(std::env::var_os("MRS_D31_ROOT").unwrap());
    let source = root.join("source");
    std::fs::write(&source, b"payload").unwrap();
    let (logging, events) = ssh::logging(false);
    let (bridge, _silent_peer) = stalled_bridge(logging).await;
    let bridge = Arc::new(bridge);
    let (lost, receive, stubborn, exits_0) = (
        mode.starts_with("lost"),
        mode.contains("receive"),
        mode.ends_with("stubborn"),
        mode.ends_with("exits-0"),
    );
    let signal = marathon_core::executor::shared::Signal::new();
    let (lose, loss) = tokio::sync::oneshot::channel::<marathon_core::error::Failure>();
    let mut stop = TransferStop::new(signal.clone());
    if lost {
        stop = stop.or_lost(async move { loss.await.unwrap() });
    }
    let first = {
        let (bridge, source, back) = (bridge.clone(), source.clone(), root.join("back"));
        tokio::spawn(async move {
            if receive {
                bridge.receive_with("/tmp/marathon/stuck", &back, stop).await
            } else {
                bridge.send_with(&source, "/tmp/marathon/stuck", stop).await
            }
        })
    };
    // Only once the stand-in and its child have both set up their signal handling.
    let ready = root.join("ready");
    tokio::time::timeout(HANG, async {
        while !ready.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let queued = (!receive).then(|| {
        let (bridge, source) = (bridge.clone(), source.clone());
        tokio::spawn(async move { bridge.send(&source, "/tmp/marathon/next").await })
    });
    tokio::time::timeout(HANG, async {
        while bridge.retained_tasks() < if receive { 1 } else { 2 } {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let reason = marathon_apple::cmd::cancelled("the caller was cancelled");
    let loss = failure(
        CONNECTION_EXCEPTION,
        "Lost the connection to the worker during a transfer",
    );
    let at = std::time::Instant::now();
    if lost {
        lose.send(loss.clone()).unwrap();
    } else {
        signal.cancel(reason.clone());
    }
    let result = tokio::time::timeout(HANG, first)
        .await
        .expect("the stop did not end the transfer, or waited before TERM")
        .unwrap();
    match (lost, receive) {
        // An rsync that says it finished keeps its success.
        _ if exits_0 => assert_eq!(result, Ok(true)),
        (false, _) => assert_eq!(result, Err(VendorError::Cancelled(reason))),
        (true, false) => assert_eq!(result, Err(VendorError::Other(loss))),
        (true, true) => assert_eq!(result, Ok(false)),
    }
    if stubborn || exits_0 {
        // KILL comes D31's 5 s after TERM, to rsync or to what it left.
        let took = at.elapsed();
        assert!(took >= std::time::Duration::from_millis(4_900), "{took:?}");
    }
    let pid = std::fs::read_to_string(root.join("pid")).unwrap();
    let pid = nix::unistd::Pid::from_raw(pid.trim().parse().unwrap());
    tokio::time::timeout(HANG, async {
        while nix::sys::signal::kill(pid, None) != Err(nix::errno::Errno::ESRCH) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    if let Some(queued) = queued {
        // Not failed with the stopped send's cancellation or loss, as D31's stall fails it: it runs.
        let queued = tokio::time::timeout(HANG, queued).await.unwrap().unwrap();
        assert_eq!(queued, Ok(true));
        assert_eq!(std::fs::read_to_string(root.join("order")).unwrap(), "stuck\nnext\n");
    }
    let errors: Vec<_> = ssh::lines(&events)
        .into_iter()
        .filter(|(_, level, _)| *level == jcompat::logging::Level::Error)
        .map(|(.., message)| message)
        .collect();
    let expected: Vec<String> = match (lost, receive) {
        (false, _) => vec![],
        (true, false) => {
            vec!["send error: the device's connection was lost while sending to 127.0.0.1:/tmp/marathon/stuck".into()]
        }
        (true, true) => {
            vec!["receive error: the device's connection was lost while receiving 127.0.0.1:/tmp/marathon/stuck".into()]
        }
    };
    assert_eq!(errors, expected);
    bridge.join_pending().await.unwrap();
}
/// D54: a transfer whose stop has come before it starts returns that stop.
#[tokio::test]
async fn a_transfer_stopped_before_it_starts_returns_its_stop_d54() {
    use marathon_apple::cmd::TransferStop;
    let (logging, events) = ssh::logging(false);
    let b = bridge(Some(key("/fixture/key")), false, false, 0, 22, logging).await;
    let signal = marathon_core::executor::shared::Signal::new();
    let reason = marathon_apple::cmd::cancelled("before");
    signal.cancel(reason.clone());
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        b.send_with(dir.path(), "/tmp/marathon/dst", TransferStop::new(signal.clone()))
            .await,
        Err(VendorError::Cancelled(reason.clone()))
    );
    assert_eq!(
        b.receive_with("/tmp/marathon/src", dir.path(), TransferStop::new(signal.clone()))
            .await,
        Err(VendorError::Cancelled(reason))
    );
    let loss = marathon_apple::cmd::failure(FailureKind::Injected(InjectedKind::Lost), "gone");
    let lost =
        TransferStop::new(marathon_core::executor::shared::Signal::new()).or_lost(std::future::ready(loss.clone()));
    assert_eq!(
        b.send_with(dir.path(), "/tmp/marathon/dst", lost).await,
        Err(VendorError::Other(loss))
    );
    // Only the loss says anything.
    assert_eq!(
        ssh::lines(&events)
            .into_iter()
            .filter(|(_, level, _)| *level == jcompat::logging::Level::Error)
            .count(),
        1
    );
    b.join_pending().await.unwrap();
}
