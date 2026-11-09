//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/FakeSimulatorWorker.kt
//! Contracts for the transport-independent 0.4b world adapter.
mod support;
use anyhow::{Result, ensure};
use fake_worker::{
    apple::{Scenario, UDID, World},
    fs::VirtualFs,
    router::{Request, Router},
    script::{Action, Chunk, Matcher, Rule, Script},
};
use marathon_apple::{
    bin::{AppleBinaryEnvironment, Tools},
    cmd::{
        CommandExecutor, CommandRequest, CommandResult, FileBridge,
        remote::ssh::sshj::{SshAuthentication, SshjCommandExecutorFactory, config::SshLogging},
    },
    model::Sdk,
    test::TestRequest,
};
use marathon_config::vendor::{AppleTimeoutParameters, apple_timeout_configuration};
use marathon_core::error::VendorResult;
use marathon_test_support::apple::{
    WorldExecutor,
    fake_simulator_worker::{connection_exception, ok},
};
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{Arc, Mutex},
};
use support::{Runtime, context, duration};
fn v<T>(r: VendorResult<T>) -> Result<T> {
    r.map_err(|e| anyhow::anyhow!("{e:?}"))
}
fn scenario(name: &str) -> Result<Scenario> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fake-worker/apple")
        .join(name)
        .join("script/world.json");
    let mut scenario: Scenario = serde_json::from_slice(&std::fs::read(path)?)?;
    scenario.expectations.clear();
    Ok(scenario)
}
fn router(w: &World, script: Script) -> Router {
    Router {
        script,
        rsync_remote: "/usr/bin/rsync".into(),
        rsync_binary: "/usr/bin/rsync".into(),
        extensions: vec![Arc::new(w.clone())],
    }
}
async fn world_executor(s: Scenario, script: Script) -> Result<(tempfile::TempDir, World, WorldExecutor)> {
    let dir = tempfile::tempdir()?;
    let world = World::new(s)?;
    let fs = VirtualFs::create(dir.path().join("root")).await?;
    let e = WorldExecutor::new(
        Arc::new(Runtime),
        &mut jcompat::logging::MarathonLogging::default(),
        router(&world, script),
        fs,
        dir.path().to_owned(),
    );
    Ok((dir, world, e))
}
async fn run(e: &dyn CommandExecutor, args: impl IntoIterator<Item = impl Into<String>>) -> Result<CommandResult> {
    let c = context();
    let s = v(e.execute(CommandRequest::new(args, duration(5000)), c.clone()).await)?;
    let result = v(s.await_result(&c).await);
    v(s.close().await)?;
    result
}
fn env(executor: Arc<dyn CommandExecutor>) -> AppleBinaryEnvironment {
    AppleBinaryEnvironment::with_tools(
        Tools {
            executor,
            timeouts: Arc::new(apple_timeout_configuration(AppleTimeoutParameters::empty())),
            runtime: Arc::new(Runtime),
            logging: SshLogging::new(
                Arc::new(Mutex::new(jcompat::logging::MarathonLogging::default())),
                Arc::new(|_, _, _, _| {}),
            ),
        },
        5000,
        5000,
    )
}
#[tokio::test]
async fn script_streams_status_signal_control_drop_and_cancellation() -> Result<()> {
    support::bounded(10, async {
        let mut hang = Action::output("started\n", 0);
        if let Action::Output { hang, .. } = &mut hang {
            *hang = true;
        }
        let script = Script {
            rules: vec![
                Rule {
                    name: "hang".into(),
                    matcher: Matcher::Exact("hang".into()),
                    action: hang,
                },
                Rule {
                    name: "chunked".into(),
                    matcher: Matcher::Exact("env".into()),
                    action: Action::Output {
                        chunks: vec![
                            Chunk {
                                base64: Some("4g==".into()),
                                ..Default::default()
                            },
                            Chunk {
                                base64: Some("gqwNCg==".into()),
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
                        hang: false,
                        ignore_signals: vec![],
                        eof_before_exit: false,
                        close_before_eof: false,
                        withhold_close: false,
                    },
                },
            ],
            ..Default::default()
        };
        let (_dir, _world, e) = world_executor(scenario("ios-smoke")?, script).await?;
        let output = run(&e, ["env"]).await?;
        assert_eq!(output.exit_code, Some(7));
        assert_eq!(output.stderr, ["err"]);
        assert_eq!(output.stdout, ["€"]);
        for (signal, code) in [("TERM", Some(143)), ("KILL", None)] {
            let c = context();
            let s = v(e
                .execute(CommandRequest::new(["hang"], duration(1000)), c.clone())
                .await)?;
            assert_eq!(v(s.base.stdout.receive().await)?.as_deref(), Some("started"));
            assert!(s.is_alive());
            if signal == "TERM" {
                v(s.terminate())?;
            } else {
                v(s.kill())?;
            }
            assert_eq!(v(s.await_result(&c).await)?.exit_code, code);
            v(s.close().await)?;
            assert!(!s.is_alive());
        }
        let c = context();
        let session = v(e.execute(CommandRequest::new(["hang"], duration(1000)), c).await)?;
        let completed = session.base.exit_code.clone();
        drop(session);
        v(completed.wait().await)?;
        let signal = Arc::new(marathon_apple::cmd::SessionScope::default());
        let mut c = context();
        c.cancellation = signal.clone();
        let s = v(e
            .execute(CommandRequest::new(["hang"], duration(1000)), c.clone())
            .await)?;
        signal.cancel(marathon_apple::cmd::cancelled("fixture"));
        assert!(s.await_result(&c).await.is_err());
        let _ = s.close().await;
        v(e.close().await)?;
        assert!(e.processes().active().await.is_empty());
        Ok(())
    })
    .await
}
#[tokio::test]
async fn files_connections_faults_and_independent_worlds() -> Result<()> {
    let (dir, _, e) = world_executor(scenario("ios-smoke")?, Script::default()).await?;
    let second = e.new_connection("other");
    assert!(run(&e, ["xcrun", "simctl", "boot", UDID]).await?.successful());
    let list = run(&second, ["xcrun", "simctl", "list", "--json"]).await?;
    assert!(list.stdout[0].contains("Booted"));
    let (_dir, _, independent) = world_executor(scenario("ios-smoke")?, Script::default()).await?;
    assert!(run(&independent, ["xcrun", "simctl", "list", "--json"]).await?.stdout[0].contains("Shutdown"));
    *e.before.lock().unwrap() =
        Arc::new(|_| Box::pin(async { Err(marathon_core::error::VendorError::Other(connection_exception("lost"))) }));
    assert!(
        v(e.safe_execute(CommandRequest::new(["env"], duration(1000)), context())
            .await)?
        .is_none()
    );
    *e.before.lock().unwrap() = Arc::new(|_| Box::pin(async { Ok(Some(ok(["override"]))) }));
    assert_eq!(run(&e, ["env"]).await?.stdout, ["override"]);
    *e.before.lock().unwrap() = Arc::new(|_| Box::pin(async { Ok(None) }));
    assert!(
        e.execute(
            CommandRequest::new(
                ["/usr/bin/rsync", "--server", "-r", ".", "/tmp/marathon"],
                duration(1000)
            ),
            context()
        )
        .await
        .is_err()
    );
    let src = dir.path().join("local");
    tokio::fs::create_dir(&src).await?;
    tokio::fs::write(src.join("data"), [0, 255, 13, 10]).await?;
    tokio::fs::set_permissions(src.join("data"), std::fs::Permissions::from_mode(0o751)).await?;
    let bridge = e.file_bridge();
    assert!(v(bridge.send(&src, "/tmp/marathon/copy").await)?);
    assert_eq!(e.fs().read("/tmp/marathon/copy/data").await?, [0, 255, 13, 10]);
    assert_eq!(e.fs().stat("/tmp/marathon/copy/data").await?.mode, 0o751);
    let dst = dir.path().join("download");
    assert!(v(bridge.receive("/tmp/marathon/copy", &dst).await)?);
    assert_eq!(tokio::fs::read(dst.join("data")).await?, [0, 255, 13, 10]);
    assert_eq!(
        tokio::fs::metadata(dst.join("data")).await?.permissions().mode() & 0o777,
        0o751
    );
    assert!(bridge.send(&src, "/etc/escape").await.is_err());
    v(e.close().await)?;
    assert!(!e.connected());
    assert!(matches!(
        e.safe_execute(CommandRequest::new(["env"], duration(1000)), context()).await,
        Err(marathon_core::error::VendorError::Other(f))
            if f.kind == marathon_error::FailureKind::IllegalState
                && f.message.as_ref().map(|m| m.to_owned()) == Some("Not connected".into())
    ));
    assert!(second.connected());
    v(second.close().await)?;
    v(independent.close().await)?;
    Ok(())
}
async fn products(fs: &VirtualFs, discovery: bool) -> Result<()> {
    fs.mkdir("/tmp/marathon/app.app").await?;
    fs.write("/tmp/marathon/app.app/Info.plist",b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>org.marathon.AppleWorld</string><key>CFBundleExecutable</key><string>app</string></dict></plist>".to_vec(),0o600).await?;
    fs.write("/tmp/marathon/app.app/app", b"binary".to_vec(), 0o700).await?;
    fs.write("/tmp/marathon/libxctest-parser.dylib", b"parser".to_vec(), 0o600)
        .await?;
    let env = if discovery {
        "<key>TestingEnvironmentVariables</key><dict><key>DYLD_INSERT_LIBRARIES</key><string>/tmp/marathon/libxctest-parser.dylib</string></dict>"
    } else {
        ""
    };
    let plist = format!(
        "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>TestConfigurations</key><array><dict><key>TestTargets</key><array><dict><key>TestHostPath</key><string>__TESTROOT__/app.app</string>{env}</dict></array></dict></array></dict></plist>"
    );
    fs.write("/tmp/marathon/run.xctestrun", plist.into_bytes(), 0o600)
        .await?;
    Ok(())
}
#[tokio::test]
async fn two_arbitrary_udids_discover_run_and_capture_independently() -> Result<()> {
    let test = async {
        let ids = [
            "11111111-2222-3333-4444-555555555555",
            "99999999-8888-7777-6666-555555555555",
        ];
        let mut s = scenario("ios-media")?;
        s.devices = ids.iter().map(|s| s.to_string()).collect();
        let (_dir, world, e) = world_executor(s, Script::default()).await?;
        let e = Arc::new(e);
        let other = e.new_connection("second");
        let env = env(e.clone());
        let c = context();
        assert_eq!(v(env.system_profiler.get_provisioning_udid(&c).await)?, ids[0]);
        for id in ids {
            v(env.xcrun.simctl.simulator.boot(id, &[], &c).await)?;
        }
        products(e.fs(), true).await?;
        for id in ids {
            let request = TestRequest::new("/tmp/marathon", "/tmp/marathon/run.xctestrun", false);
            let s = v(env
                .xcrun
                .xcodebuild
                .test_without_building(id, Sdk::IphoneSimulator, &request, &[], &c)
                .await)?;
            assert_eq!(v(s.await_result(&c).await)?.exit_code, Some(65));
            v(s.close().await)?;
        }
        products(e.fs(), false).await?;
        run(
            e.as_ref(),
            [
                "/usr/libexec/PlistBuddy",
                "-c",
                &format!("Set :DevicePreferences:{}:ConnectHardwareKeyboard false", ids[0]),
                "/Users/worker/Library/Preferences/com.apple.iphonesimulator.plist",
            ],
        )
        .await?;
        assert_eq!(world.snapshot()?["devices"][ids[0]]["keyboard"], false);
        assert_eq!(world.snapshot()?["devices"][ids[1]]["keyboard"], true);
        for (i, id) in ids.iter().enumerate() {
            let mut request = TestRequest::new("/tmp/marathon", "/tmp/marathon/run.xctestrun", false);
            request.xcresult = Some(format!("/tmp/marathon/result-{i}"));
            let batch = v(env
                .xcrun
                .xcodebuild
                .test_without_building(id, Sdk::IphoneSimulator, &request, &[], &c)
                .await)?;
            let record = format!(
                "xcrun simctl io {id} recordVideo --codec h264 --display internal --mask black --force /tmp/marathon/testAlwaysFailing-{i}.mp4"
            );
            let pidfile = format!("/tmp/marathon/{i}.pid");
            v(e.safe_execute_nohup(
                &pidfile,
                CommandRequest::new(record.split(' '), duration(1000)),
                context(),
            )
            .await)?;
            let marker = format!("/tmp/marathon/.apple-world/{id}-testAlwaysFailing.ready");
            e.fs().wait_for_file(&marker).await?;
            if i == 0 {
                assert!(
                    e.fs()
                        .stat(&format!(
                            "/tmp/marathon/.apple-world/{}-testAlwaysFailing.ready",
                            ids[1]
                        ))
                        .await
                        .is_err()
                );
            }
            assert!(v(batch.await_result(&c).await)?.exit_code.is_some());
            v(batch.close().await)?;
            assert_eq!(
                run(&other, ["pkill", "-f", &format!("simctl io {id} recordVideo")])
                    .await?
                    .exit_code,
                Some(1)
            );
            let stop = format!("kill -s INT $(cat {pidfile}) && lsof -p $(cat {pidfile}) +r 1 &>/dev/null");
            assert!(run(&other, ["sh", "-c", &stop]).await?.successful());
            assert_eq!(
                e.fs().read(&format!("/tmp/marathon/testAlwaysFailing-{i}.mp4")).await?,
                include_bytes!("../../../fixtures/fake-worker/apple/media/recording.mp4")
            );
        }
        let log = format!(
            "xcrun simctl spawn {} log stream --type log --color none --style compact 2>/dev/null > /tmp/marathon/device.log & echo $!",
            ids[0]
        );
        let pid = run(e.as_ref(), ["sh", "-c", &log]).await?.stdout[0].clone();
        assert!(
            run(&other, ["sh", "-c", &format!("kill -s INT {pid}")])
                .await?
                .successful()
        );
        assert!(!e.fs().read("/tmp/marathon/device.log").await?.is_empty());
        let state = world.final_state(e.fs()).await?;
        assert_eq!(state["sessions"].as_array().map(Vec::len), Some(4));
        world.validate()?;
        assert!(e.processes().active().await.is_empty());
        v(other.close().await)?;
        v(e.close().await)?;
        Ok(())
    };
    support::bounded(10, test).await
}

struct Capture {
    executor: Arc<dyn CommandExecutor>,
    results: Mutex<Vec<CommandResult>>,
    commands: Mutex<Vec<Vec<String>>>,
}
impl CommandExecutor for Capture {
    fn host(&self) -> &marathon_apple::cmd::CommandHost {
        self.executor.host()
    }
    fn logger(&self) -> &jcompat::logging::Logger {
        self.executor.logger()
    }
    fn connected(&self) -> bool {
        self.executor.connected()
    }
    fn log_error(&self, _: String) {}
    fn close(&self) -> marathon_core::runtime::VendorFuture<'_, ()> {
        self.executor.close()
    }
    fn execute(
        &self,
        r: CommandRequest,
        c: marathon_core::vendor::VendorContext,
    ) -> marathon_core::runtime::VendorFuture<'_, marathon_apple::cmd::CommandSession> {
        Box::pin(async move {
            self.commands.lock().unwrap().push(r.command.clone());
            let s = self.executor.execute(r, c.clone()).await?;
            let result = s.await_result(&c).await?;
            s.close().await?;
            self.results.lock().unwrap().push(result.clone());
            Ok(marathon_apple::cmd::CommandSession::finished(Arc::new(Runtime), result))
        })
    }
}
async fn wrapper(id: &str, argv: &[String], e: Arc<dyn CommandExecutor>) -> Result<Option<CommandResult>> {
    let capture = Arc::new(Capture {
        executor: e,
        results: Mutex::default(),
        commands: Mutex::default(),
    });
    let env = env(capture.clone());
    let c = context();
    match id {
        "inventory" => {
            env.xcrun
                .simctl
                .device
                .list(&c)
                .await
                .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        }
        "boot" => {
            v(env.xcrun.simctl.simulator.boot(UDID, &[], &c).await)?;
        }
        "shutdown" => {
            v(env.xcrun.simctl.simulator.shutdown(UDID, &c).await)?;
        }
        id if id.starts_with("getenv-") => {
            v(env.xcrun.simctl.simulator.getenv(UDID, &argv[4], &c).await)?;
        }
        "springboard-16plus" => {
            // The form is the probe of runtimes 16 and 17; D46 gives 18 and later `print user/foreground`.
            v(env.xcrun.simctl.simulator.is_running(UDID, "17.5", &c).await)?;
        }
        "xcode-version" => {
            v(env.xcrun.xcodebuild.get_version(&c).await)?;
        }
        "hardware" => {
            v(env.system_profiler.get_provisioning_udid(&c).await)?;
        }
        "ioreg-model" => {
            v(env.ioreg.get_model(&c).await)?;
        }
        "ioreg-manufacturer" => {
            v(env.ioreg.get_manufacturer(&c).await)?;
        }
        "os-version" => {
            v(env.swvers.get_version(&c).await)?;
        }
        "csrutil" => {
            v(env.csrutil.status(&c).await)?;
        }
        "sdk-iphonesimulator-platform-path" => {
            v(env.xcrun.get_sdk_platform_path(Sdk::IphoneSimulator, &c).await)?;
        }
        "keyboard-set" => {
            v(env
                .plist_buddy
                .set(
                    &argv[3],
                    &format!(":DevicePreferences:{UDID}:ConnectHardwareKeyboard"),
                    "false",
                    &c,
                )
                .await)?;
        }
        "keyboard-add" => {
            v(env
                .plist_buddy
                .add(
                    &argv[3],
                    &format!(":DevicePreferences:{UDID}:ConnectHardwareKeyboard"),
                    "bool",
                    "false",
                    &c,
                )
                .await)?;
        }
        "tcc-system" | "tcc-user" => {
            v(env.sqlite3.query(&argv[1], &argv[2], false, &c).await)?;
        }
        _ => return Ok(None),
    }
    let commands = capture.commands.lock().unwrap();
    assert_eq!(commands.len(), 1, "{id}");
    // Wrapper requests retain shell quotes; catalogue argv is the decoded wire form.
    let wire = marathon_apple::cmd::remote::ssh::sshj::executor::shell_command(&commands[0]);
    assert_eq!(Request::parse(wire.as_bytes())?.argv, argv, "{id}");
    Ok(capture.results.lock().unwrap().pop())
}
async fn ssh_worker(world: World) -> Result<(tempfile::TempDir, fake_worker::RunningWorker, Arc<dyn CommandExecutor>)> {
    let dir = tempfile::tempdir()?;
    let key = russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)?;
    let host = dir.path().join("host");
    tokio::fs::write(&host, key.to_openssh(russh::keys::ssh_key::LineEnding::LF)?.as_bytes()).await?;
    let config = fake_worker::Config {
        listen: "127.0.0.1:0".parse()?,
        root: dir.path().join("root"),
        transcript: dir.path().join("events.jsonl"),
        host_key: host,
        auth: fake_worker::server::AuthPolicy {
            user: "fixture".into(),
            password: Some("fake-password".into()),
            authorized_keys: vec![],
            keyboard_interactive: true,
            password_auth: true,
        },
        max_sessions: 4,
        rsync_binary: "/usr/bin/rsync".into(),
        rsync_remote: "/usr/bin/rsync".into(),
        script: Script::default(),
        fixtures: dir.path().into(),
        run: "wp-5.1".into(),
    };
    let w = fake_worker::start(config, vec![Arc::new(world)]).await?;
    let logging = SshLogging::new(
        Arc::new(Mutex::new(jcompat::logging::MarathonLogging::default())),
        Arc::new(|_, _, _, _| {}),
    );
    let e = v(
        SshjCommandExecutorFactory::new(logging, Arc::new(Runtime), Arc::new(support::Clock::default()))
            .connect(
                "127.0.0.1",
                w.ready.endpoint.port(),
                SshAuthentication::Password {
                    username: "fixture".into(),
                    password: "fake-password".into(),
                },
                None,
                true,
            )
            .await
            .map_err(|e| e.into_vendor()),
    )?;
    Ok((dir, w, Arc::new(e)))
}
async fn seed(fs: &VirtualFs) -> Result<()> {
    fs.remove("/tmp/marathon/probe").await?;
    fs.mkdir("/tmp/marathon/probe/src").await?;
    fs.write("/tmp/marathon/probe/src/source", b"source".to_vec(), 0o700)
        .await?;
    fs.write("/tmp/marathon/probe/file", b"file".to_vec(), 0o600).await?;
    Ok(())
}
async fn catalogue_result(id: &str, argv: &[String], e: Arc<dyn CommandExecutor>) -> Result<CommandResult> {
    if let Some(result) = wrapper(id, argv, e.clone()).await? {
        return Ok(result);
    }
    if matches!(id, "record-h264" | "device-log-nested") {
        let c = context();
        let s = v(e
            .execute(CommandRequest::new(argv.to_vec(), duration(5000)), c.clone())
            .await)?;
        v(s.kill())?;
        let result = v(s.await_result(&c).await)?;
        v(s.close().await)?;
        return Ok(result);
    }
    run(e.as_ref(), argv.to_vec()).await
}
#[tokio::test]
async fn generated_catalogue_matches_the_ssh_executor_and_production_wrappers() -> Result<()> {
    support::bounded(60, async {
        let (_dir, _, e) = world_executor(scenario("ios-media")?, Script::default()).await?;
        let e = Arc::new(e);
        let (_sshdir, w, ssh) = ssh_worker(World::new(scenario("ios-media")?)?).await?;
        let catalogue: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/fake-worker/apple/command-catalogue.json"
        ))?;
        let mut exercised = vec![];
        for row in catalogue["forms"].as_array().ok_or_else(|| anyhow::anyhow!("forms"))? {
            if row["reachability"] == "deferred" {
                continue;
            }
            let id = row["id"].as_str().ok_or_else(|| anyhow::anyhow!("id"))?;
            // These require uploaded products/results, or the intentionally separate rsync transport.
            if matches!(
                id,
                "install"
                    | "xcode-test-ios"
                    | "xcode-test-macos"
                    | "xcode-discovery-macos"
                    | "xcresult-root"
                    | "xcresult-id"
                    | "nm-swift"
                    | "nm-objc"
                    | "lipo-archs"
                    | "chmod"
                    | "rsync-send"
                    | "rsync-receive"
            ) {
                continue;
            }
            seed(e.fs()).await?;
            seed(&w.fs).await?;
            if id == "transcode-h264" {
                for fs in [e.fs(), &w.fs] {
                    fs.write(
                        "/tmp/marathon/probe/input.mp4",
                        include_bytes!("../../../fixtures/fake-worker/apple/media/recording.mp4").to_vec(),
                        0o600,
                    )
                    .await?;
                }
            }
            // Capture has no uploaded-product prerequisite; prepare both initial devices identically.
            run(e.as_ref(), ["xcrun", "simctl", "boot", UDID]).await?;
            run(ssh.as_ref(), ["xcrun", "simctl", "boot", UDID]).await?;
            if id == "record-stop" || id == "cat-pid" {
                // Both registries start with the same virtual PID and consume the same catalogue jobs.
                for fs in [e.fs(), &w.fs] {
                    fs.write("/tmp/marathon/probe/video.pid", b"1000\n".to_vec(), 0o600)
                        .await?;
                }
            }
            let argv: Vec<String> = serde_json::from_value(row["argv"].clone())?;
            let parsed = Request::parse(row["wire"].as_str().ok_or_else(|| anyhow::anyhow!("wire"))?.as_bytes())?;
            assert_eq!(argv, parsed.argv);
            let a = catalogue_result(id, &argv, e.clone())
                .await
                .map_err(|err| anyhow::anyhow!("{id} in process: {err:#}"))?;
            let b = catalogue_result(id, &argv, ssh.clone())
                .await
                .map_err(|err| anyhow::anyhow!("{id} SSH: {err:#}"))?;
            assert_eq!(a, b, "{id}");
            exercised.push(id);
        }
        ensure!(
            exercised.len() == 46,
            "catalogue coverage changed: {} forms",
            exercised.len()
        );
        v(e.close().await)?;
        v(ssh.close().await)?;
        w.shutdown().await?;
        assert!(e.processes().active().await.is_empty());
        Ok(())
    })
    .await
}

#[tokio::test]
async fn the_fake_answers_the_runtime_18_readiness_probe_as_it_answers_dumpstate_d46() -> Result<()> {
    support::bounded(60, async {
        let (_dir, _, e) = world_executor(scenario("ios-media")?, Script::default()).await?;
        let e = Arc::new(e);
        let (_sshdir, w, ssh) = ssh_worker(World::new(scenario("ios-media")?)?).await?;
        for executor in [e.clone(), ssh.clone()] {
            let probes = [
                [
                    "xcrun",
                    "simctl",
                    "spawn",
                    UDID,
                    "launchctl",
                    "dumpstate",
                    "user/foreground/com.apple.SpringBoard",
                ],
                [
                    "xcrun",
                    "simctl",
                    "spawn",
                    UDID,
                    "launchctl",
                    "print",
                    "user/foreground",
                ],
            ];
            // A simulator that is not booted has no SpringBoard in either form, and a booted one reports the marker.
            let before = [
                run(executor.as_ref(), probes[0]).await?,
                run(executor.as_ref(), probes[1]).await?,
            ];
            assert_eq!(before[0], before[1]);
            assert_eq!(before[1].exit_code, Some(1));
            run(executor.as_ref(), ["xcrun", "simctl", "boot", UDID]).await?;
            let after = [
                run(executor.as_ref(), probes[0]).await?,
                run(executor.as_ref(), probes[1]).await?,
            ];
            assert_eq!(after[0], after[1]);
            assert_eq!(after[1].stdout, ["A com.apple.springboard.services"]);
            assert_eq!(after[1].exit_code, Some(0));
        }
        v(e.close().await)?;
        v(ssh.close().await)?;
        w.shutdown().await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn expectation_reservation_rollback_and_finite_counts_work_in_process() -> Result<()> {
    let mut s = scenario("ios-smoke")?;
    s.expectations = vec![fake_worker::apple::Expectation {
        name: "once".into(),
        matcher: Matcher::Exact("chmod +x /tmp/marathon/binary".into()),
        min: 1,
        max: 1,
    }];
    let (_dir, w, e) = world_executor(s, Script::default()).await?;
    e.fs().write("/tmp/marathon/binary", b"fixture".to_vec(), 0o600).await?;
    let request = || CommandRequest::new(["chmod", "+x", "/tmp/marathon/binary"], duration(1000));
    let lease = e.fs().lease().await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), e.execute(request(), context()))
            .await
            .is_err()
    );
    assert_eq!(w.snapshot()?["calls"]["once"], 0);
    drop(lease);
    let (a, b) = tokio::join!(e.execute(request(), context()), e.execute(request(), context()));
    assert!(a.is_ok() ^ b.is_ok());
    assert_eq!(w.snapshot()?["calls"]["once"], 1);
    for session in [a, b].into_iter().flatten() {
        v(session.await_result(&context()).await)?;
        v(session.close().await)?;
    }
    v(e.close().await)?;
    Ok(())
}
#[tokio::test]
async fn screenshot_markers_and_live_recording_cancellation_are_per_device() -> Result<()> {
    let test = async {
        let ids = [
            "11111111-2222-3333-4444-555555555555",
            "99999999-8888-7777-6666-555555555555",
        ];
        let mut spec = scenario("ios-screenshot")?;
        spec.devices = ids.iter().map(|s| s.to_string()).collect();
        let (_dir, _, e) = world_executor(spec, Script::default()).await?;
        for (i, id) in ids.iter().enumerate() {
            run(&e, ["xcrun", "simctl", "boot", id]).await?;
            assert!(
                run(
                    &e,
                    [
                        "xcrun",
                        "simctl",
                        "io",
                        id,
                        "screenshot",
                        "--type=png",
                        "--display=internal",
                        "--mask=black",
                        &format!("/tmp/marathon/{i}.png")
                    ]
                )
                .await?
                .successful()
            );
        }
        for id in ids {
            assert!(
                e.fs()
                    .stat(&format!("/tmp/marathon/.apple-world/{id}-testFailing.ready"))
                    .await
                    .is_err()
            );
        }
        v(e.close().await)?;
        let mut spec = scenario("ios-media")?;
        spec.devices = ids.iter().map(|s| s.to_string()).collect();
        let (_dir, _, e) = world_executor(spec, Script::default()).await?;
        run(&e, ["xcrun", "simctl", "boot", ids[0]]).await?;
        let c = context();
        let s = v(e
            .execute(
                CommandRequest::new(
                    [
                        "xcrun",
                        "simctl",
                        "io",
                        ids[0],
                        "recordVideo",
                        "--codec",
                        "h264",
                        "--display",
                        "internal",
                        "--mask",
                        "black",
                        "--force",
                        "/tmp/marathon/testAlwaysFailing.mp4",
                    ],
                    duration(1000),
                ),
                c.clone(),
            )
            .await)?;
        e.fs()
            .wait_for_file(&format!(
                "/tmp/marathon/.apple-world/{}-testAlwaysFailing.ready",
                ids[0]
            ))
            .await?;
        v(s.kill())?;
        assert_eq!(v(s.await_result(&c).await)?.exit_code, None);
        v(s.close().await)?;
        assert!(e.fs().stat("/tmp/marathon/testAlwaysFailing.mp4").await.is_err());
        let log = format!(
            "xcrun simctl spawn {} log stream --type log --color none --style compact 2>/dev/null > /tmp/marathon/lost.log & echo $!",
            ids[0]
        );
        run(&e, ["sh", "-c", &log]).await?;
        assert_eq!(e.processes().active().await.len(), 1);
        v(e.close().await)?;
        assert!(e.processes().active().await.is_empty());
        assert!(e.fs().stat("/tmp/marathon/lost.log").await.is_err());
        Ok(())
    };
    support::bounded(5, test).await
}

#[tokio::test]
async fn barriers_release_the_filesystem_and_reject_escaping_paths() -> Result<()> {
    support::bounded(2, async {
        let (_dir, _, e) = world_executor(scenario("ios-smoke")?, Script::default()).await?;
        let (wait, write) = tokio::join!(
            e.fs().wait_for_file("/tmp/marathon/ready"),
            e.fs().write("/tmp/marathon/ready", vec![], 0o600)
        );
        wait?;
        write?;
        assert!(e.fs().wait_for_file("/tmp/marathon/../escape").await.is_err());
        assert!(e.fs().wait_for_file("/etc/hosts").await.is_err());
        assert!(e.fs().symlink("/etc/hosts", "/tmp/marathon/escape").await.is_err());
        e.fs()
            .symlink("/tmp/marathon/ready", "/tmp/marathon/write-link")
            .await?;
        assert!(e.fs().make_executable("/tmp/marathon/write-link").await.is_err());
        v(e.close().await)?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn unknown_xcodebuild_user_arguments_are_explicitly_deferred() -> Result<()> {
    for argument in [vec!["-unknown", "value"], vec!["-quiet"]] {
        let (_dir, world, e) = world_executor(scenario("ios-smoke")?, Script::default()).await?;
        let mut argv = vec![
            "xcrun",
            "xcodebuild",
            "test-without-building",
            "-xctestrun",
            "/tmp/marathon/marathon.xctestrun",
        ];
        argv.extend(argument);
        let result = run(&e, argv.clone()).await?;
        assert_eq!(result.exit_code, Some(64));
        assert!(result.stdout.is_empty());
        assert!(result.stderr[0].contains("recognised but deferred"));
        let wire = marathon_apple::cmd::remote::ssh::sshj::executor::shell_command(
            &argv.into_iter().map(String::from).collect::<Vec<_>>(),
        );
        let request = Request::parse(wire.as_bytes())?;
        assert_eq!(world.snapshot()?["deferred"], serde_json::json!([request.text]));
        assert_eq!(world.snapshot()?["sessions"], serde_json::json!([]));
        v(e.close().await)?;
    }
    Ok(())
}

#[tokio::test]
async fn missing_xctestrun_products_fail_without_starting_a_session() -> Result<()> {
    let mut spec = scenario("ios-smoke")?;
    spec.expectations = vec![fake_worker::apple::Expectation {
        name: "batch".into(),
        matcher: Matcher::Prefix("xcrun xcodebuild test-without-building ".into()),
        min: 0,
        max: 1,
    }];
    let (_dir, world, e) = world_executor(spec, Script::default()).await?;
    let plist = br#"<?xml version="1.0"?><plist version="1.0"><dict><key>TestConfigurations</key><array><dict><key>TestTargets</key><array><dict><key>TestHostPath</key><string>__TESTROOT__/missing.app</string><key>TestBundlePath</key><string>__TESTHOST__/PlugIns/missing.xctest</string></dict></array></dict></array></dict></plist>"#;
    e.fs()
        .write("/tmp/marathon/marathon.xctestrun", plist.to_vec(), 0o600)
        .await?;
    assert!(
        e.execute(
            CommandRequest::new(
                [
                    "xcrun",
                    "xcodebuild",
                    "test-without-building",
                    "-xctestrun",
                    "/tmp/marathon/marathon.xctestrun",
                    "-enableCodeCoverage",
                    "NO",
                    "-destination-timeout",
                    "30",
                    "-destination",
                    &format!("platform=iOS Simulator,id={UDID}")
                ],
                duration(1000)
            ),
            context()
        )
        .await
        .is_err()
    );
    assert_eq!(world.snapshot()?["sessions"], serde_json::json!([]));
    assert_eq!(world.snapshot()?["calls"]["batch"], 0);
    assert!(e.processes().active().await.is_empty());
    v(e.close().await)?;
    Ok(())
}
