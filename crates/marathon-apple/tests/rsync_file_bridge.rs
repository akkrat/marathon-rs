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

/// D31, in a child process because rsync's path is process-wide. `program` stands in for rsync; a stalled send is stopped
/// in compressed time, whichever way the process has to be stopped.
fn stalled_send_child(mode: &str, program: &Path, extra_path: Option<&Path>, root: &Path) {
    stalled_send_child_with(mode, program, extra_path, root, None)
}
fn stalled_send_child_with(mode: &str, program: &Path, extra_path: Option<&Path>, root: &Path, sshpass: Option<&Path>) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "stalled_send_runs", "--nocapture"])
        .env("MRS_D31_MODE", mode)
        .env("MRS_D31_ROOT", root)
        .env("RSYNC4J_RSYNC", program);
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
            if file != "held" && nix::sys::signal::kill(pid, None).is_ok() {
                survivors.push(file);
            }
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
        }
    }
    let output = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(status.is_some_and(|status| status.success()), "{output}");
    assert_eq!(survivors, Vec::<&str>::new(), "{output}");
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
#[test]
fn a_stalled_send_under_sshpass_also_stops_rsync_d31() {
    // Measured with the real sshpass: TERM ends it at once, and rsync, in its own session, keeps waiting for ssh.
    let dir = tempfile::tempdir().unwrap();
    let program = fake_rsync(
        dir.path(),
        "echo 'rsync(1): error: poll: timeout' >&2\nexec /bin/sleep 100037",
    );
    let sshpass = dir.path().join("sshpass");
    script(
        &sshpass,
        "#!/bin/bash\nshift 2\ntrap 'exit 143' TERM\n\"$@\" &\nwait $!\n",
    );
    stalled_send_child_with("hung", &program, None, dir.path(), Some(&sshpass));
}
/// The real program with a short `--timeout` and an ssh that ignores the end of its input: the report's wording and the
/// wait after it are the installed rsync's own, openrsync on macOS and rsync 3 on Linux.
fn real_rsync_with_a_hung_ssh(sshpass: Option<&Path>) {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    script(
        &bin.join("ssh"),
        "#!/bin/sh\necho $$ > \"$MRS_D31_ROOT/ssh-pid\"\nexec /bin/sleep 100037\n",
    );
    let program = dir.path().join("rsync");
    script(
        &program,
        r#"#!/bin/bash
case "$*" in *127.0.0.1:/tmp/marathon/next*) echo next >> "$MRS_D31_ROOT/order"; exit 0;; esac
echo $$ > "$MRS_D31_ROOT/pid"
echo stalled >> "$MRS_D31_ROOT/order"
args=()
for arg in "$@"; do
  case "$arg" in --timeout=300) args+=(--timeout=1);; *) args+=("$arg");; esac
done
exec /usr/bin/rsync "${args[@]}"
"#,
    );
    stalled_send_child_with("hung", &program, Some(&bin), dir.path(), sshpass);
}
#[test]
fn the_real_rsync_gives_up_on_a_hung_ssh_d31() {
    real_rsync_with_a_hung_ssh(None);
}
#[test]
fn the_real_rsync_under_the_real_sshpass_gives_up_on_a_hung_ssh_d31() {
    let sshpass = ["/opt/homebrew/bin/sshpass", "/usr/bin/sshpass"]
        .into_iter()
        .map(Path::new)
        .find(|path| path.exists());
    match sshpass {
        Some(sshpass) => real_rsync_with_a_hung_ssh(Some(sshpass)),
        None => eprintln!("skipped: no sshpass on this host"),
    }
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
    let bridge = Arc::new(
        RsyncFileBridge::new(
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
            Some(if std::env::var_os("MRS_D31_PASSWORD").is_some() {
                SshAuthentication::Password {
                    username: "fixture".into(),
                    password: "fake-password".into(),
                }
            } else {
                key("/fixture/key")
            }),
            Arc::new(Runtime),
            Arc::new(ssh::ScaledClock::new(100)),
            logging,
        )
        .await
        .unwrap(),
    );
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
    // Reaped before the send returned, so a retry cannot overlap it, and the next send ran after it.
    assert_eq!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
        Err(nix::errno::Errno::ESRCH)
    );
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
