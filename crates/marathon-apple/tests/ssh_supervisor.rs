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
                    .starts_with(b"/usr/bin/perl -e ")
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
                ["/usr/bin/perl", "-e", _, "--", _] => Ok(Some(Action::output(&format!("MARATHON_D96_1 {root}\n"), 0))),
                ["/usr/bin/perl", program, "control", directory, _, control]
                    if *directory == root && *program == format!("{root}/supervisor.pl") =>
                {
                    let (token, operation) = control.split_once(':').unwrap();
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
                _ => Ok(None),
            }
        })
    }
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
        tokio::spawn(async move {
            device
                .execute(
                    CommandRequest::new(["echo", "fixture-no-launch-ack"], duration(30000)),
                    context(),
                )
                .await
        })
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
    assert!(executing.await.unwrap().is_err());
    let execs = fake.events("exec").await;
    let launch = wire(waiting);
    let token = launch.split_ascii_whitespace().nth(5).unwrap();
    let stopped: Vec<_> = execs
        .iter()
        .filter(|row| wire(row).ends_with(&format!("{token}:3")))
        .collect();
    assert_eq!(stopped.len(), 1);
    assert_ne!(stopped[0]["connection"], waiting["connection"]);
    assert!(device.handed_commands().is_empty());
    drop((device, sibling));
    fake.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn d30_escalation_joins_worker_proof_without_a_data_close_d96() {
    let fake = ssh::fake_with_handlers(
        ssh::script(vec![]),
        3,
        |_| {},
        vec![Arc::new(SupervisorWorker::default())],
    )
    .await;
    let (logging, _) = ssh::logging(false);
    let executor = ssh::factory(&logging)
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
            let events = fake.events("exec").await;
            if let Some(term) = events.into_iter().find(|row| matches_operation(row, ":2")) {
                break term;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!closing.is_finished());

    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(21)).await;
    tokio::time::resume();
    tokio::time::timeout(std::time::Duration::from_secs(5), closing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let execs = fake.events("exec").await;
    let kill = execs.iter().find(|row| matches_operation(row, ":3")).unwrap();
    let run = execs
        .iter()
        .find(|row| matches_operation(row, "echo resistant'"))
        .unwrap();
    assert_ne!(kill["connection"], run["connection"]);
    assert_ne!(term["connection"], run["connection"]);
    assert!(kill["seq"].as_u64().unwrap() > term["seq"].as_u64().unwrap());
    // Neither escalation request consumed the original SSH session's acknowledgement or CLOSE.
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
    let output = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "real_worker_commands_and_senders_child"])
        .env("PATH", path)
        .env("MRS_D96_REAL_WORKER", "1")
        .output()?;
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
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
    let config = server.path().join("sshd_config");
    std::fs::write(
        &config,
        format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nAuthorizedKeysFile {}\nPidFile {}\nUsePAM no\nStrictModes no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nMaxSessions 1\n",
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
