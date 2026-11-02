//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/rsync/RsyncFileBridge.kt
mod support;
use marathon_apple::cmd::{
    FileBridge,
    remote::{
        rsync::{RsyncFileBridge, RsyncTarget},
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
                    assert_eq!(&args[1..], expected);
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
                    .map(|v| v.as_str().unwrap())
                    .collect();
                assert_eq!(args[1..], expected);
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
fn send_serialization_unbounded_receives_and_a3_deadline_match_kotlin() {
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
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
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
