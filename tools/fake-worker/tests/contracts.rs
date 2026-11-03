use anyhow::{Result, ensure};
use base64::{Engine, prelude::BASE64_STANDARD};
use fake_worker::{
    Config, RunningWorker,
    fs::VirtualFs,
    router::{Request, Router},
    script::{Action, Chunk, Faults, Keepalive, Matcher, Rule, Script},
    server::AuthPolicy,
};
use russh::{
    ChannelMsg, Sig, client,
    keys::{Algorithm, PrivateKey, PublicKeyOrCertificate},
};
use std::{sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::time::timeout;

struct Client;
impl client::Handler for Client {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
}
fn rule(name: &str, action: Action) -> Rule {
    Rule {
        name: name.into(),
        matcher: Matcher::Exact(name.into()),
        action,
    }
}
fn output(hang: bool, ignore: Vec<String>, withhold: bool) -> Action {
    Action::Output {
        chunks: vec![
            Chunk {
                data: "out\n".into(),
                ..Default::default()
            },
            Chunk {
                data: "err\n".into(),
                extended: Some(1),
                ..Default::default()
            },
        ],
        status: 7,
        signal: None,
        hang,
        ignore_signals: ignore,
        eof_before_exit: false,
        close_before_eof: false,
        withhold_close: withhold,
    }
}
async fn worker(script: Script, limit: usize) -> Result<(TempDir, RunningWorker)> {
    worker_with_handlers(script, limit, vec![]).await
}
async fn worker_with_handlers(
    script: Script,
    limit: usize,
    handlers: Vec<Arc<dyn fake_worker::router::Handler>>,
) -> Result<(TempDir, RunningWorker)> {
    let dir = tempfile::tempdir()?;
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
    let host = dir.path().join("host");
    tokio::fs::write(&host, key.to_openssh(keys_line_ending())?.as_bytes()).await?;
    let config = Config {
        listen: "127.0.0.1:0".parse()?,
        root: dir.path().join("root"),
        transcript: dir.path().join("events.jsonl"),
        host_key: host,
        auth: AuthPolicy {
            user: "fixture".into(),
            password: Some("fake-password".into()),
            authorized_keys: vec![],
            keyboard_interactive: true,
            password_auth: true,
        },
        max_sessions: limit,
        rsync_binary: "/usr/bin/rsync".into(),
        rsync_remote: "/usr/bin/rsync".into(),
        script,
        fixtures: dir.path().into(),
        run: "contract".into(),
    };
    Ok((dir, fake_worker::start(config, handlers).await?))
}
fn keys_line_ending() -> russh::keys::ssh_key::LineEnding {
    russh::keys::ssh_key::LineEnding::LF
}
async fn connect(w: &RunningWorker) -> Result<client::Handle<Client>> {
    let mut c = client::connect(Arc::new(client::Config::default()), w.ready.endpoint, Client).await?;
    ensure!(c.authenticate_password("fixture", "fake-password").await?.success());
    Ok(c)
}
async fn collect(ch: &mut russh::Channel<client::Msg>) -> Result<(Vec<u8>, Vec<u8>, Option<u32>, Option<Sig>)> {
    timeout(Duration::from_secs(60), async {
        let (mut out, mut err, mut status, mut signal) = (vec![], vec![], None, None);
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::Data { data } => out.extend(data),
                ChannelMsg::ExtendedData { ext: 1, data } => err.extend(data),
                ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                ChannelMsg::ExitSignal { signal_name: s, .. } => signal = Some(s),
                ChannelMsg::Close => break,
                _ => (),
            }
        }
        Ok((out, err, status, signal))
    })
    .await?
}
#[tokio::test]
async fn streams_term_reuse_limits_and_local_close() -> Result<()> {
    let script = Script {
        rules: vec![
            rule("hello", output(false, vec![], false)),
            rule("held", output(true, vec![], false)),
            rule("cat", Action::EchoStdin),
        ],
        ..Default::default()
    };
    let (dir, w) = worker(script, 2).await?;
    let c = connect(&w).await?;
    let mut a = c.channel_open_session().await?;
    a.exec(true, "held").await?;
    let mut b = c.channel_open_session().await?;
    b.exec(true, "held").await?;
    let rejected = c.channel_open_session().await;
    assert!(matches!(
        rejected,
        Err(russh::Error::ChannelOpenFailure(
            russh::ChannelOpenFailure::ResourceShortage
        ))
    ));
    a.signal(Sig::TERM).await?;
    let result = collect(&mut a).await?;
    assert!(matches!(result.3, Some(Sig::TERM)));
    let mut d = c.channel_open_session().await?;
    d.exec(true, "hello").await?;
    let result = collect(&mut d).await?;
    assert_eq!(result.2, Some(7));
    assert_eq!(result.0, b"out\n");
    assert_eq!(result.1, b"err\n");
    b.close().await?;
    assert!(matches!(
        timeout(Duration::from_secs(60), async {
            loop {
                if matches!(b.wait().await, Some(ChannelMsg::Close) | None) {
                    break;
                }
            }
        })
        .await,
        Ok(())
    ));
    let mut cat = c.channel_open_session().await?;
    cat.exec(true, "cat").await?;
    cat.data_bytes(vec![0, 255, 1]).await?;
    cat.eof().await?;
    assert_eq!(collect(&mut cat).await?.0, [0, 255, 1]);
    c.disconnect(russh::Disconnect::ByApplication, "test", "").await?;
    timeout(Duration::from_secs(60), w.shutdown()).await??;
    let text = tokio::fs::read_to_string(dir.path().join("events.jsonl")).await?;
    assert!(text.contains("session_rejected"));
    assert!(text.contains("\"cleanup\""));
    assert!(!text.contains("fake-password"));
    for (i, line) in text.lines().enumerate() {
        let e: serde_json::Value = serde_json::from_str(line)?;
        assert_eq!(e["seq"], i as u64);
    }
    Ok(())
}
#[tokio::test]
async fn close_fault_is_per_channel_and_shutdown_is_bounded() -> Result<()> {
    let (dir, w) = worker(
        Script {
            rules: vec![
                rule("hang", output(true, vec![], true)),
                rule("ok", output(false, vec![], false)),
            ],
            ..Default::default()
        },
        2,
    )
    .await?;
    let c = connect(&w).await?;
    let mut a = c.channel_open_session().await?;
    a.exec(true, "hang").await?;
    a.signal(Sig::TERM).await?;
    timeout(Duration::from_secs(60), async {
        loop {
            if matches!(a.wait().await, Some(ChannelMsg::ExitSignal { .. })) {
                break;
            }
        }
    })
    .await?;
    a.close().await?;
    assert!(
        timeout(Duration::from_millis(150), async {
            loop {
                if matches!(a.wait().await, Some(ChannelMsg::Close) | None) {
                    break;
                }
            }
        })
        .await
        .is_err()
    );
    let mut b = c.channel_open_session().await?;
    b.exec(true, "ok").await?;
    assert_eq!(collect(&mut b).await?.2, Some(7));
    let c2 = connect(&w).await?;
    let mut d = c2.channel_open_session().await?;
    d.exec(true, "ok").await?;
    assert_eq!(collect(&mut d).await?.2, Some(7));
    timeout(Duration::from_secs(60), w.shutdown()).await??;
    let log = tokio::fs::read_to_string(dir.path().join("events.jsonl")).await?;
    assert!(log.contains("close_withheld"));
    Ok(())
}
#[tokio::test]
async fn ignored_signals_before_close_are_recorded_with_and_without_eof() -> Result<()> {
    let mut action = output(true, vec!["TERM".into()], false);
    if let Action::Output { chunks, .. } = &mut action {
        chunks.clear();
    }
    let (dir, w) = worker(
        Script {
            rules: vec![rule("ignore", action)],
            ..Default::default()
        },
        2,
    )
    .await?;
    let c = connect(&w).await?;
    for eof in [false, true] {
        let mut ch = c.channel_open_session().await?;
        ch.exec(true, "ignore").await?;
        assert!(matches!(
            timeout(Duration::from_secs(60), ch.wait()).await?,
            Some(ChannelMsg::Success)
        ));
        if eof {
            ch.eof().await?;
        }
        // Queue CLOSE immediately: both ignored signals must survive even if
        // the actor is first scheduled after the close handler cancels it.
        ch.signal(Sig::TERM).await?;
        ch.signal(Sig::TERM).await?;
        ch.close().await?;
        let (out, err, status, signal) = collect(&mut ch).await?;
        assert!(out.is_empty() && err.is_empty() && status.is_none() && signal.is_none());
    }
    timeout(Duration::from_secs(60), w.shutdown()).await??;
    let log = tokio::fs::read_to_string(dir.path().join("events.jsonl")).await?;
    let rows: Vec<serde_json::Value> = log.lines().map(serde_json::from_str).collect::<Result<_, _>>()?;
    assert_eq!(rows.iter().filter(|r| r["event"] == "signal_ignored").count(), 4);
    assert_eq!(rows.iter().filter(|r| r["event"] == "channel_end").count(), 2);
    Ok(())
}
#[tokio::test]
async fn ignored_term_custom_signal_exec_rejection_and_extended_data() -> Result<()> {
    let script = Script {
        rules: vec![
            rule("ignore", output(true, vec!["TERM".into()], false)),
            rule("reject", Action::Reject),
            rule(
                "odd",
                Action::Output {
                    chunks: vec![Chunk {
                        data: "partial".into(),
                        extended: Some(42),
                        ..Default::default()
                    }],
                    status: 3,
                    signal: None,
                    hang: false,
                    ignore_signals: vec![],
                    eof_before_exit: true,
                    close_before_eof: false,
                    withhold_close: false,
                },
            ),
        ],
        ..Default::default()
    };
    let (_dir, w) = worker(script, 3).await?;
    let c = connect(&w).await?;
    let mut a = c.channel_open_session().await?;
    a.exec(true, "ignore").await?;
    a.signal(Sig::TERM).await?;
    assert!(
        timeout(Duration::from_millis(100), async {
            loop {
                if matches!(a.wait().await, Some(ChannelMsg::ExitSignal { .. })) {
                    break;
                }
            }
        })
        .await
        .is_err()
    );
    a.signal(Sig::Custom("USR2".into())).await?;
    assert!(matches!(collect(&mut a).await?.3,Some(Sig::Custom(s)) if s=="USR2"));
    let mut b = c.channel_open_session().await?;
    b.exec(true, "reject").await?;
    assert!(matches!(
        timeout(Duration::from_secs(60), b.wait()).await?,
        Some(ChannelMsg::Failure)
    ));
    b.close().await?;
    let mut d = c.channel_open_session().await?;
    d.exec(true, "odd").await?;
    let mut saw = false;
    while let Some(m) = timeout(Duration::from_secs(60), d.wait()).await? {
        match m {
            ChannelMsg::ExtendedData { ext: 42, data } => {
                saw = true;
                assert_eq!(&data[..], b"partial");
            }
            ChannelMsg::Close => break,
            _ => (),
        }
    }
    assert!(saw);
    w.shutdown().await
}
#[tokio::test]
async fn auth_rejects_wrong_password_unknown_user_and_key() -> Result<()> {
    let (_dir, w) = worker(Script::default(), 2).await?;
    for (user, pwd) in [("fixture", "wrong"), ("unknown", "fake-password")] {
        let mut c = client::connect(Arc::new(client::Config::default()), w.ready.endpoint, Client).await?;
        assert!(!c.authenticate_password(user, pwd).await?.success());
        c.disconnect(russh::Disconnect::ByApplication, "test", "").await?;
    }
    let mut c = client::connect(Arc::new(client::Config::default()), w.ready.endpoint, Client).await?;
    let k = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
    assert!(
        !c.authenticate_publickey("fixture", russh::keys::PrivateKeyWithHashAlg::new(Arc::new(k), None))
            .await?
            .success()
    );
    w.shutdown().await
}
#[tokio::test]
async fn keepalive_suppression_does_not_stop_output() -> Result<()> {
    let chunks = (0..20)
        .map(|_| Chunk {
            data: "tick\n".into(),
            delay_ms: 100,
            ..Default::default()
        })
        .collect();
    let script = Script {
        rules: vec![rule(
            "busy",
            Action::Output {
                chunks,
                status: 0,
                signal: None,
                hang: false,
                ignore_signals: vec![],
                eof_before_exit: false,
                close_before_eof: false,
                withhold_close: false,
            },
        )],
        faults: Faults {
            keepalive: Keepalive::Withhold,
            ..Default::default()
        },
        ..Default::default()
    };
    let (dir, w) = worker(script, 1).await?;
    let mut c = client::connect(
        Arc::new(client::Config {
            keepalive_interval: Some(Duration::from_millis(150)),
            keepalive_max: 2,
            ..Default::default()
        }),
        w.ready.endpoint,
        Client,
    )
    .await?;
    assert!(c.authenticate_password("fixture", "fake-password").await?.success());
    let mut ch = c.channel_open_session().await?;
    ch.exec(true, "busy").await?;
    let probes = async {
        for _ in 0..4 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            c.send_keepalive(true).await?;
        }
        Ok::<_, anyhow::Error>(())
    };
    let (result, ()) = tokio::try_join!(collect(&mut ch), probes)?;
    assert_eq!(result.0.len(), 100);
    w.shutdown().await?;
    let log = tokio::fs::read_to_string(dir.path().join("events.jsonl")).await?;
    assert!(!log.contains("tick"));
    // Automatic probes can also fire when scheduling delays exceed the keepalive interval.
    assert!(
        log.lines()
            .filter(|line| line.contains("\"event\":\"keepalive\""))
            .count()
            >= 4
    );
    Ok(())
}
#[tokio::test]
async fn virtual_files_writefile_failure_snapshot_symlinks_and_paths() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let fs = VirtualFs::create(dir.path().join("root")).await?;
    fs.mkdir("/tmp/marathon/shared").await?;
    let data = (0..=255).collect::<Vec<u8>>();
    let path = "/tmp/marathon/shared/run.xctestrun";
    fs.write_file(path, data.clone(), 256).await?;
    assert_eq!(fs.read(path).await?, data);
    assert_eq!(fs.stat(path).await?.mode, 0o600);
    assert!(fs.write_file(path, b"short".to_vec(), 256).await.is_err());
    assert_eq!(fs.read(path).await?, data);
    assert_eq!(fs.list("/tmp/marathon/shared").await?.len(), 1);
    let snap = fs.snapshot().await?;
    fs.symlink(path, "/tmp/marathon/link").await?;
    assert_eq!(fs.read("/tmp/marathon/link").await?, data);
    assert_eq!(fs.stat("/tmp/marathon/link").await?.kind, "file");
    assert!(
        fs.list("/tmp/marathon")
            .await?
            .iter()
            .any(|e| e.link.as_deref() == Some(path))
    );
    assert!(fs.write("/tmp/marathon/link", vec![], 0o600).await.is_err());
    assert!(fs.mkdir("/tmp/marathon/../escape").await.is_err());
    assert!(fs.mkdir("/tmp/marathon-sibling").await.is_err());
    assert!(fs.symlink("/etc/passwd", "/tmp/marathon/bad").await.is_err());
    assert!(
        fs.resolve_xctestrun_path("__TESTROOT__/shared/run.xctestrun", "/tmp/marathon", None)
            .await?
            .starts_with(dir.path().canonicalize()?)
    );
    fs.reset(snap.clone()).await?;
    assert_eq!(fs.snapshot().await?.0, snap.0);
    let router = Router {
        script: Script::default(),
        rsync_remote: "/usr/bin/rsync".into(),
        rsync_binary: "/usr/bin/rsync".into(),
        extensions: vec![],
    };
    let encoded = BASE64_STANDARD.encode(&data);
    let tmp = format!("{path}.marathon-tmp");
    let command = format!(
        "sh -c 'umask 077 && echo {encoded} | base64 --decode > {tmp} && [ $(wc -c < {tmp}) -eq 256 ] && mv -f {tmp} {path} || {{ rm -f {tmp}; exit 1; }}'"
    );
    let route = router.route(&Request::parse(command.as_bytes())?, &fs).await?;
    assert!(!route.unmatched);
    assert_eq!(fs.read(path).await?, data);
    assert!(Request::parse(b"echo 'it\\'s'").is_err());
    for command in [
        "cp -R /tmp/marathon/shared /tmp/marathon/copy",
        "mv -f /tmp/marathon/copy /tmp/marathon/moved",
        "find /tmp/marathon -mindepth 1 -maxdepth 1 '!' -name shared -exec rm -rf '{}' +",
    ] {
        assert!(!router.route(&Request::parse(command.as_bytes())?, &fs).await?.unmatched);
    }
    assert_eq!(fs.list("/tmp/marathon").await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn virtual_background_pid_is_signalled_without_host_processes() -> Result<()> {
    let script = Script {
        rules: vec![rule("record", output(true, vec![], false))],
        ..Default::default()
    };
    let (dir, w) = worker(script, 2).await?;
    let c = connect(&w).await?;
    let mut a = c.channel_open_session().await?;
    a.exec(
        true,
        "sh -c 'nohup record > /dev/null 2>&1 & echo $! > /tmp/marathon/record.pid'",
    )
    .await?;
    assert_eq!(collect(&mut a).await?.2, Some(0));
    assert_eq!(w.fs.read("/tmp/marathon/record.pid").await?, b"1000\n");
    let mut b = c.channel_open_session().await?;
    b.exec(true,"sh -c 'kill -s INT $(cat /tmp/marathon/record.pid) && lsof -p $(cat /tmp/marathon/record.pid) +r 1 &>/dev/null'").await?;
    assert_eq!(collect(&mut b).await?.2, Some(0));
    w.shutdown().await?;
    let log = tokio::fs::read_to_string(dir.path().join("events.jsonl")).await?;
    assert!(log.contains("background_signal"));
    assert!(log.contains("background_end"));
    Ok(())
}

struct Capture(Arc<tokio::sync::Mutex<Vec<(u32, String)>>>);
impl client::Handler for Capture {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
    async fn disconnected(&mut self, reason: client::DisconnectReason<Self::Error>) -> Result<(), Self::Error> {
        match reason {
            client::DisconnectReason::ReceivedDisconnect(d) => {
                self.0.lock().await.push((d.reason_code as u32, d.message));
                Ok(())
            }
            client::DisconnectReason::Error(e) => Err(e),
        }
    }
}
#[tokio::test]
async fn wire_disconnect_reasons_kex_and_abrupt_loss() -> Result<()> {
    for stage in ["before_kex", "after_auth", "during_exec"] {
        for reason in [12, 2, 11] {
            let d = Some(fake_worker::script::Disconnect {
                reason,
                message: "fixture-wire-reason".into(),
            });
            let mut faults = Faults::default();
            match stage {
                "before_kex" => faults.before_kex = d,
                "after_auth" => faults.after_auth = d,
                _ => faults.during_exec = d,
            }
            let (_dir, w) = worker(
                Script {
                    faults,
                    ..Default::default()
                },
                2,
            )
            .await?;
            let captured = Arc::new(tokio::sync::Mutex::new(vec![]));
            let result = client::connect(
                Arc::new(client::Config::default()),
                w.ready.endpoint,
                Capture(captured.clone()),
            )
            .await;
            if let Ok(mut c) = result {
                let _ = c.authenticate_password("fixture", "fake-password").await;
                if stage == "during_exec" {
                    let ch = c.channel_open_session().await?;
                    ch.exec(true, "anything").await?;
                }
                let _ = timeout(Duration::from_secs(60), c).await;
            }
            assert_eq!(
                *captured.lock().await,
                vec![(reason, "fixture-wire-reason".into())],
                "{stage}"
            );
            w.shutdown().await?;
        }
    }
    let (_dir, w) = worker(
        Script {
            faults: Faults {
                no_common_kex: true,
                ..Default::default()
            },
            ..Default::default()
        },
        1,
    )
    .await?;
    assert!(
        client::connect(Arc::new(client::Config::default()), w.ready.endpoint, Client)
            .await
            .is_err()
    );
    w.shutdown().await?;
    let (_dir, w) = worker(
        Script {
            rules: vec![rule("drop", Action::DropTransport)],
            ..Default::default()
        },
        1,
    )
    .await?;
    let c = connect(&w).await?;
    let mut ch = c.channel_open_session().await?;
    ch.exec(true, "drop").await?;
    timeout(Duration::from_secs(60), async { while ch.wait().await.is_some() {} }).await?;
    w.shutdown().await
}

#[tokio::test]
async fn held_real_rsync_child_is_reaped_and_other_sessions_progress() -> Result<()> {
    if tokio::fs::metadata("/usr/bin/rsync").await.is_err() {
        eprintln!("MISSING prerequisite /usr/bin/rsync: real child cleanup not exercised");
        return Ok(());
    }
    let (dir, w) = worker(
        Script {
            rules: vec![rule("hello", output(false, vec![], false))],
            ..Default::default()
        },
        3,
    )
    .await?;
    w.fs.mkdir("/tmp/marathon/transfer").await?;
    let c = connect(&w).await?;
    let ch = c.channel_open_session().await?;
    ch.exec(true, "/usr/bin/rsync --server -r . /tmp/marathon/transfer/")
        .await?;
    let log_path = dir.path().join("events.jsonl");
    timeout(Duration::from_secs(60), async {
        loop {
            let log = tokio::fs::read_to_string(&log_path).await?;
            if log.contains("child_start") {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    let mut other = c.channel_open_session().await?;
    other.exec(true, "hello").await?;
    assert_eq!(collect(&mut other).await?.2, Some(7));
    timeout(Duration::from_secs(60), w.shutdown()).await??;
    let log = tokio::fs::read_to_string(log_path).await?;
    assert!(log.contains("child_reaped"));
    for row in log.lines().map(serde_json::from_str::<serde_json::Value>) {
        let row = row?;
        if row["event"] == "child_start" {
            let pid = row["detail"]["pid"]
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("missing pid"))?;
            assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_err());
        }
    }
    Ok(())
}

#[tokio::test]
async fn actual_kotlin_xctestrun_fields_use_the_shared_resolver() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let fs = VirtualFs::create(dir.path().join("root")).await?;
    let bytes = include_bytes!("../../../fixtures/fake-worker/generated.xctestrun").to_vec();
    fs.mkdir("/tmp/marathon/shared/tests.xctest").await?;
    fs.mkdir("/tmp/marathon/shared/runner.app").await?;
    fs.write_file("/tmp/marathon/shared/marathon.xctestrun", bytes.clone(), bytes.len())
        .await?;
    let text = std::str::from_utf8(&bytes)?;
    for (key, expected) in [
        ("TestBundlePath", "shared/tests.xctest"),
        ("TestHostPath", "shared/runner.app"),
    ] {
        let prefix = format!("<key>{key}</key><string>");
        let value = text
            .split_once(&prefix)
            .and_then(|(_, r)| r.split_once("</string>"))
            .ok_or_else(|| anyhow::anyhow!("missing fixture path"))?
            .0;
        let resolved = fs.resolve_xctestrun_path(value, "/tmp/marathon", None).await?;
        assert_eq!(resolved, dir.path().canonicalize()?.join("root").join(expected));
        assert!(tokio::fs::metadata(resolved).await?.is_dir());
    }
    assert_eq!(fs.read("/tmp/marathon/shared/marathon.xctestrun").await?, bytes);
    assert!(text.contains("do not replace /tmp/marathon in opaque text"));
    Ok(())
}

#[tokio::test]
async fn unsupported_requests_fail_and_fixture_output_survives_eof_fault() -> Result<()> {
    let script = Script {
        rules: vec![rule(
            "late",
            Action::Output {
                chunks: vec![
                    Chunk {
                        data: "first".into(),
                        ..Default::default()
                    },
                    Chunk {
                        fixture: Some("tail.bin".into()),
                        delay_ms: 25,
                        eof_before: true,
                        ..Default::default()
                    },
                ],
                status: 4,
                signal: None,
                hang: false,
                ignore_signals: vec![],
                eof_before_exit: false,
                close_before_eof: false,
                withhold_close: false,
            },
        )],
        ..Default::default()
    };
    let (dir, w) = worker(script, 2).await?;
    tokio::fs::write(dir.path().join("tail.bin"), [0, 255, 10]).await?;
    let c = connect(&w).await?;
    let mut ch = c.channel_open_session().await?;
    for _ in 0..40 {
        ch.set_env(true, "F", "value").await?;
        assert!(matches!(
            timeout(Duration::from_secs(60), ch.wait()).await?,
            Some(ChannelMsg::Failure)
        ));
    }
    ch.request_pty(true, "xterm", 80, 24, 0, 0, &[]).await?;
    assert!(matches!(
        timeout(Duration::from_secs(60), ch.wait()).await?,
        Some(ChannelMsg::Failure)
    ));
    ch.request_shell(true).await?;
    assert!(matches!(
        timeout(Duration::from_secs(60), ch.wait()).await?,
        Some(ChannelMsg::Failure)
    ));
    ch.request_subsystem(true, "sftp").await?;
    assert!(matches!(
        timeout(Duration::from_secs(60), ch.wait()).await?,
        Some(ChannelMsg::Failure)
    ));
    ch.exec(true, "late").await?;
    let result = collect(&mut ch).await?;
    assert_eq!(result.0, [b"first".as_slice(), &[0, 255, 10]].concat());
    assert_eq!(result.2, Some(4));
    assert!(
        c.channel_open_direct_tcpip("127.0.0.1", 9, "127.0.0.1", 0)
            .await
            .is_err()
    );
    w.shutdown().await
}

#[test]
fn rsync_feature_flags_cannot_enable_symlink_following() -> Result<()> {
    use fake_worker::router::rsync_args;
    for option in [
        "-L",
        "-rL",
        "--copy-links",
        "--keep-dirlinks",
        "--log-file=/tmp/escape",
        "--temp-dir=/tmp/escape",
    ] {
        assert!(rsync_args(&["--server", option, ".", "/tmp/marathon/file"]).is_err());
    }
    assert!(rsync_args(&["--server", "-logDtpre.iLsfxCIvu", ".", "/tmp/marathon/file"]).is_ok());
    assert!(rsync_args(&["--server", "--sender", "-logDtpre.LsfxCIvu", ".", "/tmp/marathon/file"]).is_ok());
    assert!(rsync_args(&["--server", "--partial-dir", "../escape", ".", "/tmp/marathon/file"]).is_err());
    Ok(())
}

struct PendingHandler {
    entered: Arc<tokio::sync::Notify>,
    dropped: Arc<tokio::sync::Notify>,
}
struct NotifyDrop(Arc<tokio::sync::Notify>);
impl Drop for NotifyDrop {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}
impl fake_worker::router::Handler for PendingHandler {
    fn name(&self) -> &str {
        "pending"
    }
    fn route<'a>(&'a self, request: &'a Request, _: &'a VirtualFs) -> fake_worker::router::HandlerFuture<'a> {
        Box::pin(async move {
            if request.text == "pending" {
                let _drop = NotifyDrop(self.dropped.clone());
                self.entered.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(None)
        })
    }
}
#[tokio::test]
async fn pending_extension_routing_is_dropped_on_channel_connection_and_run_cancellation() -> Result<()> {
    for command in [
        "pending",
        "sh -c 'nohup pending > /dev/null 2>&1 & echo $! > /tmp/marathon/job.pid'",
    ] {
        for cancellation in ["channel", "connection", "run"] {
            let entered = Arc::new(tokio::sync::Notify::new());
            let dropped = Arc::new(tokio::sync::Notify::new());
            let (_dir, w) = worker_with_handlers(
                Script::default(),
                2,
                vec![Arc::new(PendingHandler {
                    entered: entered.clone(),
                    dropped: dropped.clone(),
                })],
            )
            .await?;
            let c = connect(&w).await?;
            let ch = c.channel_open_session().await?;
            ch.exec(true, command).await?;
            timeout(Duration::from_secs(60), entered.notified()).await?;
            match cancellation {
                "channel" => ch.close().await?,
                "connection" => c.disconnect(russh::Disconnect::ByApplication, "test", "").await?,
                _ => (),
            }
            if cancellation != "run" {
                timeout(Duration::from_secs(60), dropped.notified()).await?;
                // Channel cancellation must leave the transport usable.
                if cancellation == "channel" {
                    let mut other = c.channel_open_session().await?;
                    other.exec(true, "mkdir -p /tmp/marathon/ok").await?;
                    assert_eq!(collect(&mut other).await?.2, Some(0));
                }
            }
            timeout(Duration::from_secs(60), w.shutdown()).await??;
            if cancellation == "run" {
                timeout(Duration::from_secs(60), dropped.notified()).await?;
            }
        }
    }
    Ok(())
}
