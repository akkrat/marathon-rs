use anyhow::{Result, ensure};
use fake_worker::{
    Config, RunningWorker,
    apple::{Scenario, UDID, World},
    fs::VirtualFs,
    router::{Handler, Request, Router},
    script::{Action, Chunk, Matcher, Rule, Script},
    server::AuthPolicy,
};
use russh::{
    ChannelMsg, Sig, client,
    keys::{Algorithm, PrivateKey, PublicKeyOrCertificate},
};
use std::{sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::time::timeout;
fn world(name: &str) -> Result<World> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fake-worker/apple");
    World::new(serde_json::from_slice(&std::fs::read(
        root.join(name).join("script/world.json"),
    )?)?)
}
async fn exec(c: &client::Handle<Client>, command: &str) -> Result<(Vec<u8>, Vec<u8>, Option<u32>, Option<Sig>)> {
    let mut ch = c.channel_open_session().await?;
    ch.exec(true, command).await?;
    collect(&mut ch).await
}
struct Client;
impl client::Handler for Client {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
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
    timeout(Duration::from_secs(5), async {
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
async fn catalogue_deferrals_are_explicit() -> Result<()> {
    let catalogue: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/fake-worker/apple/command-catalogue.json"
    ))?;
    let dir = tempfile::tempdir()?;
    let fs = VirtualFs::create(dir.path().join("root")).await?;
    let world = world("ios-smoke")?;
    let router = Router {
        script: Script::default(),
        rsync_remote: "/usr/bin/rsync".into(),
        rsync_binary: "/usr/bin/rsync".into(),
        extensions: vec![Arc::new(world.clone())],
    };
    for row in catalogue["forms"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("catalogue forms"))?
    {
        let wire = row["wire"].as_str().ok_or_else(|| anyhow::anyhow!("wire"))?;
        let request = Request::parse(wire.as_bytes())?;
        assert_eq!(serde_json::to_value(&request.argv)?, row["argv"], "{}", row["id"]);
        if row["reachability"] == "deferred" {
            let action = world
                .route(&request, &fs)
                .await?
                .ok_or_else(|| anyhow::anyhow!("unrecognised {}", row["id"]))?;
            let Action::Output { status, chunks, .. } = action else {
                anyhow::bail!("not explicit failure")
            };
            assert_eq!(status, 64, "{}", row["id"]);
            assert_eq!(chunks[0].extended, Some(1));
            assert!(chunks[0].data.contains("recognised but deferred"));
        } else {
            // These forms are exercised by complete recorded scenarios; the
            // catalogue-only world has no uploaded products or running jobs.
            let prerequisite = match row["id"].as_str() {
                Some("install") => Some("uploaded app with Info.plist"),
                Some("screenshot-png" | "record-h264") => Some("booted simulator"),
                Some("record-stop") => Some("recording pidfile"),
                Some("xcode-test-ios" | "xcode-test-macos" | "xcode-discovery-macos") => {
                    Some("platform-specific world and uploaded xctestrun/products")
                }
                Some("xcresult-root" | "xcresult-id") => Some("completed result bundle"),
                Some("nm-swift" | "nm-objc" | "lipo-archs" | "chmod") => Some("uploaded executable"),
                Some("transcode-h264") => Some("completed recording"),
                Some("fs-find") => Some("uploaded search directory"),
                _ => None,
            };
            if prerequisite.is_none() {
                let routed = router
                    .route(&request, &fs)
                    .await
                    .map_err(|error| anyhow::anyhow!("{}: {error:#}", row["id"]))?;
                assert!(!routed.unmatched, "{}", row["id"]);
            }
        }
    }
    assert!(
        world
            .route(&Request::parse(b"/usr/bin/unknown-apple-tool")?, &fs)
            .await?
            .is_none()
    );
    Ok(())
}
#[tokio::test]
async fn barriers_release_the_filesystem_and_reject_escaping_paths() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let fs = VirtualFs::create(dir.path().join("root")).await?;
    timeout(Duration::from_secs(2), async {
        let (a, b) = tokio::join!(
            fs.wait_for_file("/tmp/marathon/ready"),
            fs.write("/tmp/marathon/ready", vec![], 0o600)
        );
        a?;
        b?;
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    assert!(fs.wait_for_file("/tmp/marathon/../escape").await.is_err());
    assert!(fs.wait_for_file("/etc/hosts").await.is_err());
    assert!(fs.symlink("/etc/hosts", "/tmp/marathon/escape").await.is_err());
    fs.symlink("/tmp/marathon/ready", "/tmp/marathon/write-link").await?;
    assert!(fs.make_executable("/tmp/marathon/write-link").await.is_err());
    Ok(())
}
#[tokio::test]
async fn finite_expectations_reserve_atomically_and_roll_back_cancelled_routes() -> Result<()> {
    let mut scenario: Scenario = serde_json::from_str(include_str!(
        "../../../fixtures/fake-worker/apple/ios-smoke/script/world.json"
    ))?;
    scenario.expectations = vec![fake_worker::apple::Expectation {
        name: "once".into(),
        matcher: Matcher::Exact("chmod +x /tmp/marathon/binary".into()),
        min: 1,
        max: 1,
    }];
    let world = World::new(scenario)?;
    let dir = tempfile::tempdir()?;
    let fs = VirtualFs::create(dir.path().join("root")).await?;
    fs.write("/tmp/marathon/binary", b"fixture".to_vec(), 0o600).await?;
    let request = Request::parse(b"chmod +x /tmp/marathon/binary")?;
    let lease = fs.lease().await;
    // This deadline cancels an unfinished route while its filesystem lease is
    // deliberately unavailable, not an ordering sleep in a scenario.
    assert!(
        timeout(Duration::from_millis(20), world.route(&request, &fs))
            .await
            .is_err()
    );
    assert_eq!(world.snapshot()?["calls"]["once"], 0);
    drop(lease);
    let (a, b) = tokio::join!(world.route(&request, &fs), world.route(&request, &fs));
    assert!(a.is_ok() ^ b.is_ok());
    assert_eq!(world.snapshot()?["calls"]["once"], 1);
    assert_eq!(fs.stat("/tmp/marathon/binary").await?.mode & 0o111, 0o111);
    Ok(())
}
#[tokio::test]
async fn two_connections_share_devices_and_recording_publishes_only_after_int() -> Result<()> {
    let world = world("ios-media")?;
    let (_dir, w) = worker_with_handlers(Script::default(), 4, vec![Arc::new(world.clone())]).await?;
    let a = connect(&w).await?;
    let b = connect(&w).await?;
    assert_eq!(exec(&a, &format!("xcrun simctl boot {UDID}")).await?.2, Some(0));
    let inventory = exec(&b, "xcrun simctl list --json").await?.0;
    assert!(String::from_utf8(inventory)?.contains("Booted"));
    assert_eq!(exec(&b, "xcrun simctl getenv MISSING HOME").await?.2, Some(148));
    let video = "/tmp/marathon/testAlwaysFailing.mp4";
    let start = format!(
        "sh -c 'nohup xcrun simctl io {UDID} recordVideo --codec h264 --display internal --mask black --force {video} > /dev/null 2>&1 & echo $! > /tmp/marathon/record.pid'"
    );
    assert_eq!(exec(&a, &start).await?.2, Some(0));
    timeout(
        Duration::from_secs(2),
        w.fs.wait_for_file("/tmp/marathon/.apple-world/testAlwaysFailing.ready"),
    )
    .await??;
    assert!(w.fs.stat(video).await.is_err());
    let stop = "sh -c 'kill -s INT $(cat /tmp/marathon/record.pid) && lsof -p $(cat /tmp/marathon/record.pid) +r 1 &>/dev/null'";
    assert_eq!(exec(&b, stop).await?.2, Some(0));
    assert_eq!(
        w.fs.read(video).await?,
        include_bytes!("../../../fixtures/fake-worker/apple/media/recording.mp4")
    );
    assert_eq!(exec(&b, stop).await?.2, Some(1));
    timeout(Duration::from_secs(2), w.shutdown()).await??;
    Ok(())
}
#[tokio::test]
async fn device_log_wrapper_owns_jobs_and_joins_before_pull() -> Result<()> {
    let mut scenario: Scenario = serde_json::from_str(include_str!(
        "../../../fixtures/fake-worker/apple/ios-media/script/world.json"
    ))?;
    scenario.expectations.clear();
    let world = World::new(scenario)?;
    let (dir, w) = worker_with_handlers(Script::default(), 4, vec![Arc::new(world)]).await?;
    let a = connect(&w).await?;
    let start = format!(
        "sh -c 'xcrun simctl spawn {UDID} log stream --type log --color none --style compact 2>/dev/null > /tmp/marathon/device.log & echo $!'"
    );
    let pid = String::from_utf8(exec(&a, &start).await?.0)?.trim().parse::<u64>()?;
    assert!(w.fs.stat("/tmp/marathon/device.log").await.is_err());
    assert_eq!(exec(&a, &format!("sh -c 'kill -s INT {pid}'")).await?.2, Some(0));
    assert_eq!(
        w.fs.read("/tmp/marathon/device.log").await?,
        b"2000-01-01 00:00:00 AppleWorld scripted device log\n"
    );
    w.fs.remove("/tmp/marathon/device.log").await?;
    let lost_pid = String::from_utf8(exec(&a, &start).await?.0)?.trim().parse::<u64>()?;
    a.disconnect(russh::Disconnect::ByApplication, "fixture", "").await?;
    // Polls the worker process's event log; the deadline only catches a hang on a slow runner.
    timeout(Duration::from_secs(60), async {
        loop {
            let text = tokio::fs::read_to_string(dir.path().join("events.jsonl")).await?;
            if text
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .any(|r| r["event"] == "background_end" && r["detail"]["pid"] == lost_pid)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    assert!(w.fs.stat("/tmp/marathon/device.log").await.is_err());
    let b = connect(&w).await?;
    exec(&b, &start).await?;
    // Run cancellation joins the separate wrapper too and publishes no complete
    // artifact from an interrupted logging job.
    let fs = w.fs.clone();
    timeout(Duration::from_secs(2), w.shutdown()).await??;
    assert!(fs.stat("/tmp/marathon/device.log").await.is_err());
    let rows = std::fs::read_to_string(dir.path().join("events.jsonl"))?
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        rows.last().ok_or_else(|| anyhow::anyhow!("no cleanup"))?["detail"]["active_processes"],
        serde_json::json!([])
    );
    Ok(())
}
#[tokio::test]
async fn waiting_output_is_cancelled_on_channel_connection_and_run_loss() -> Result<()> {
    for mode in ["channel", "connection", "run"] {
        let mut action = Action::output("", 0);
        if let Action::Output { chunks, .. } = &mut action {
            *chunks = vec![
                Chunk {
                    data: "started".into(),
                    ..Default::default()
                },
                Chunk {
                    wait_for_file: Some("/tmp/marathon/never".into()),
                    ..Default::default()
                },
            ];
        }
        let script = Script {
            rules: vec![Rule {
                name: "barrier".into(),
                matcher: Matcher::Exact("barrier".into()),
                action,
            }],
            ..Default::default()
        };
        let (_dir, w) = worker(script, 2).await?;
        let c = connect(&w).await?;
        let mut ch = c.channel_open_session().await?;
        ch.exec(true, "barrier").await?;
        timeout(Duration::from_secs(2), async {
            loop {
                if matches!(ch.wait().await, Some(ChannelMsg::Data { .. })) {
                    break;
                }
            }
        })
        .await?;
        if mode == "channel" {
            ch.close().await?;
        } else if mode == "connection" {
            c.disconnect(russh::Disconnect::ByApplication, "fixture", "").await?;
        }
        // The old barrier implementation retained its lease here and hung both
        // this write and shutdown after cancellation (AW1).
        timeout(
            Duration::from_secs(2),
            w.fs.write("/tmp/marathon/progress", vec![], 0o600),
        )
        .await??;
        timeout(Duration::from_secs(2), w.shutdown()).await??;
    }
    Ok(())
}

#[tokio::test]
async fn unknown_xcodebuild_user_arguments_are_explicitly_deferred() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let fs = VirtualFs::create(dir.path().join("root")).await?;
    for argument in ["-unknown value", "-quiet"] {
        let world = world("ios-smoke")?;
        let request = Request::parse(
            format!("xcrun xcodebuild test-without-building -xctestrun /tmp/marathon/marathon.xctestrun {argument}")
                .as_bytes(),
        )?;
        let Some(Action::Output { status, chunks, .. }) = world.route(&request, &fs).await? else {
            anyhow::bail!("unknown user argument was not explicitly deferred")
        };
        assert_eq!(status, 64);
        assert_eq!(chunks[0].extended, Some(1));
        assert!(chunks[0].data.contains("recognised but deferred"));
        assert_eq!(world.snapshot()?["deferred"], serde_json::json!([request.text]));
        assert_eq!(world.snapshot()?["sessions"], serde_json::json!([]));
    }
    Ok(())
}

#[tokio::test]
async fn missing_xctestrun_products_fail_without_starting_a_session() -> Result<()> {
    let world = world("ios-smoke")?;
    let dir = tempfile::tempdir()?;
    let fs = VirtualFs::create(dir.path().join("root")).await?;
    let plist=b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>TestConfigurations</key><array><dict><key>TestTargets</key><array><dict><key>TestHostPath</key><string>__TESTROOT__/missing.app</string><key>TestBundlePath</key><string>__TESTHOST__/PlugIns/missing.xctest</string></dict></array></dict></array></dict></plist>";
    fs.write("/tmp/marathon/marathon.xctestrun", plist.to_vec(), 0o600)
        .await?;
    let request=Request::parse(format!("xcrun xcodebuild test-without-building -xctestrun /tmp/marathon/marathon.xctestrun -enableCodeCoverage NO -destination-timeout 30 -destination 'platform=iOS Simulator,id={UDID}'").as_bytes())?;
    assert!(world.route(&request, &fs).await.is_err());
    assert_eq!(world.snapshot()?["sessions"], serde_json::json!([]));
    assert_eq!(world.snapshot()?["calls"]["batch"], 0);
    Ok(())
}
#[tokio::test]
async fn initial_device_lifecycles_are_isolated_by_udid() -> Result<()> {
    let mut scenario: Scenario = serde_json::from_str(include_str!(
        "../../../fixtures/fake-worker/apple/ios-smoke/script/world.json"
    ))?;
    let second = "FFFFFFFF-BBBB-CCCC-DDDD-EEEEEEEEEEEE";
    scenario.devices.push(second.into());
    let world = World::new(scenario)?;
    let dir = tempfile::tempdir()?;
    let fs = VirtualFs::create(dir.path().join("root")).await?;
    world
        .route(&Request::parse(format!("xcrun simctl boot {UDID}").as_bytes())?, &fs)
        .await?;
    let state = world.snapshot()?;
    assert_eq!(state["devices"][UDID]["state"], "Booted");
    assert_eq!(state["devices"][second]["state"], "Shutdown");
    let action = world
        .route(
            &Request::parse(
                format!("cat /Users/worker/Library/Developer/CoreSimulator/Devices/{second}/device.plist").as_bytes(),
            )?,
            &fs,
        )
        .await?;
    let Some(Action::Output { chunks, .. }) = action else {
        anyhow::bail!("device descriptor unrecognised")
    };
    assert!(chunks[0].data.contains("<integer>1</integer>"));
    Ok(())
}

#[tokio::test]
async fn scripted_json_bytes_do_not_depend_on_serde_map_features() -> Result<()> {
    let world = world("ios-smoke")?;
    let state = serde_json::to_string(&world.snapshot()?)?;
    assert!(state.starts_with("{\"calls\":{},\"deferred\":[],\"devices\":"));
    let dir = tempfile::tempdir()?;
    let fs = VirtualFs::create(dir.path().join("root")).await?;
    let action = world.route(&Request::parse(b"xcrun simctl list --json")?, &fs).await?;
    let Some(Action::Output { chunks, .. }) = action else {
        anyhow::bail!("no inventory output")
    };
    let data = &chunks
        .first()
        .ok_or_else(|| anyhow::anyhow!("no inventory chunk"))?
        .data;
    assert!(data.starts_with("{\"devices\":"));
    assert!(data.find("\"devices\"") < data.find("\"devicetypes\""));
    assert!(data.find("\"devicetypes\"") < data.find("\"runtimes\""));
    Ok(())
}
