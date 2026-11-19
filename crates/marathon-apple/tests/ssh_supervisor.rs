mod support;

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
        let factory = ssh::factory(&logging);
        let warned = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        for _ in 0..2 {
            let executor = factory
                .connect("127.0.0.1", fake.port(), ssh::password(), None, false)
                .await
                .unwrap()
                .with_worker_supervisor(warned.clone(), duration(1000), 1);
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

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread")]
async fn worker_renews_and_expires_a_group_over_real_ssh_d96() -> Result<(), Box<dyn std::error::Error>> {
    use marathon_apple::cmd::remote::ssh::sshj::SshAuthentication;
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
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nAuthorizedKeysFile {}\nPidFile {}\nUsePAM no\nStrictModes no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n",
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
    let (logging, _) = ssh::logging(false);
    let mut factory = ssh::factory(&logging);
    factory.connection_attempts = 1;
    let executor = tokio::time::timeout(std::time::Duration::from_secs(6), async {
        loop {
            if let Some(status) = daemon.try_wait()? {
                return Err::<_, Box<dyn std::error::Error>>(format!("fixture SSH daemon exited with {status}").into());
            }
            match factory
                .connect(
                    "127.0.0.1",
                    port,
                    SshAuthentication::PublicKey {
                        username: user.clone(),
                        key: key.clone(),
                    },
                    None,
                    false,
                )
                .await
            {
                Ok(executor) => return Ok(executor),
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
    })
    .await??;
    let executor = executor.with_worker_supervisor(Arc::default(), duration(1000), 1);
    assert!(
        executor
            .worker_supervised()
            .await
            .map_err(|e| e.failure().to_string())?
    );
    let context = context();
    let healthy = executor
        .execute(
            CommandRequest::new(["/bin/sh", "-c", "sleep 7; exit 23"], duration(15000)),
            context.clone(),
        )
        .await
        .map_err(|e| e.failure().to_string())?;
    let ended = healthy
        .await_result(&context)
        .await
        .map_err(|e| e.failure().to_string())?;
    assert_eq!(ended.exit_code, Some(23));

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
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
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
