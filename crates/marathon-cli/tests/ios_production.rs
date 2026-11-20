//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/IosVendor.kt
//! Built-binary loopback SSH smoke; component evidence, not a full differential manifest.
use base64::Engine;
use fake_worker::{
    Config,
    apple::{Scenario, UDID, World},
    server::AuthPolicy,
};
use std::{path::Path, sync::Arc};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
async fn key(path: &Path) -> Result {
    let result = tokio::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(path)
        .output()
        .await?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).into_owned().into());
    }
    Ok(())
}
fn bundle(root: &Path, name: &str, extension: &str) -> Result {
    let folder = root.join(format!("{name}.{extension}"));
    std::fs::create_dir_all(&folder)?;
    std::fs::write(
        folder.join("Info.plist"),
        format!(
            "<plist><dict><key>CFBundleIdentifier</key><string>example.{name}</string><key>CFBundleName</key><string>{name}</string><key>CFBundleExecutable</key><string>{name}</string><key>CFBundleSupportedPlatforms</key><array><string>iPhoneSimulator</string></array></dict></plist>"
        ),
    )?;
    std::fs::write(folder.join(name), [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0])?;
    Ok(())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn built_binary_runs_and_parses_ios_through_real_ssh_provider_parser_and_listeners() -> Result {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut evidence = Vec::new();
    for (kind, parse, password) in [
        ("iOS", false, false),
        ("iOS", true, false),
        ("watchOS", true, false),
        ("tvOS", true, false),
        ("visionOS", true, false),
        ("iOS", false, true),
    ] {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        let host = root.join("host");
        let auth = root.join("key");
        key(&host).await?;
        key(&auth).await?;
        bundle(root, "App", "app")?;
        bundle(root, "Tests", "xctest")?;
        let mut scenario: Scenario = serde_json::from_slice(&std::fs::read(
            repo.join("fixtures/fake-worker/apple/ios-smoke/script/world.json"),
        )?)?;
        scenario.expectations.clear();
        scenario.devices = vec![UDID.into()];
        scenario.target = "Tests".into();
        scenario.tests = vec![fake_worker::apple::Test {
            class: "SampleTests".into(),
            method: "test1".into(),
            status: "passed".into(),
        }];
        let world = World::new(scenario).map_err(|e| e.to_string())?;
        let worker = fake_worker::start(
            Config {
                listen: "127.0.0.1:0".parse()?,
                root: root.join("worker"),
                transcript: root.join("transcript.jsonl"),
                host_key: host,
                auth: AuthPolicy {
                    user: "fixture".into(),
                    password: password.then(|| "fake-password".into()),
                    authorized_keys: if password { vec![] } else { vec![root.join("key.pub")] },
                    keyboard_interactive: false,
                    password_auth: password,
                },
                max_sessions: 32,
                rsync_binary: "/usr/bin/rsync".into(),
                rsync_remote: "/usr/bin/rsync".into(),
                script: Default::default(),
                fixtures: repo.join("fixtures/fake-worker"),
                run: "ios-cli".into(),
            },
            vec![Arc::new(world.clone())],
        )
        .await
        .map_err(|e| e.to_string())?;
        let port = worker.ready.endpoint.port();
        std::fs::write(
            root.join("Marathondevices"),
            format!(
                "workers:\n- transport: {{type: ssh, addr: 127.0.0.1, port: {port}, checkReachability: false}}\n  devices: [{{type: simulator, udid: {UDID}}}]\n"
            ),
        )?;
        let authentication = if password {
            "{type: password, username: fixture, password: fake-password}".to_owned()
        } else {
            format!("{{type: publicKey, username: fixture, key: '{}'}}", auth.display())
        };
        std::fs::write(
            root.join("Marathonfile"),
            format!(
                "name: ios-cli\noutputDir: report\ndebug: true\nanalyticsTracking: false\nbugsnagReporting: false\nuncompletedTestRetryQuota: 0\nretryStrategy: {{type: no-retry}}\nvendorConfiguration:\n  type: {kind}\n  bundle:\n    application: App.app\n    testApplication: Tests.xctest\n    testType: xctest\n  ssh:\n    authentication: {authentication}\n    keepAliveInterval: PT0S\n  lifecycle:\n    shutdownUnused: false\n    onPrepare: []\n    onDispose: [SHUTDOWN]\n  deviceLog: true\n  xcresult:\n    pullingPolicy: NEVER\n  screenRecordConfiguration:\n    videoConfiguration:\n      enabled: false\n      transcoding: {{enabled: false}}\n    screenshotConfiguration: {{enabled: false}}\n",
            ),
        )?;
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_marathon"));
        command
            .current_dir(root)
            .kill_on_drop(true)
            .env("FW_REAL_SSH", "/usr/bin/ssh")
            .env("FW_ARGV_LOG", root.join("host-argv.jsonl"))
            .env_remove("FW_USE_FIXTURE_ASKPASS")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    repo.join("tools/fake-worker/shims").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        if password {
            command
                .env("RSYNC4J_SSHPASS", "/missing-fixture-sshpass")
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY");
        }
        if parse {
            command.args(["parse", "-o", "parsed"]);
        }
        let output = tokio::time::timeout(std::time::Duration::from_secs(40), command.output()).await??;
        let text = String::from_utf8_lossy(&output.stdout);
        let errors = String::from_utf8_lossy(&output.stderr);
        let transcript = std::fs::read_to_string(root.join("transcript.jsonl"))?;
        let commands: Vec<String> = transcript
            .lines()
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|v| v["event"] == "exec")
            .map(|v| {
                base64::engine::general_purpose::STANDARD
                    .decode(v["detail"]["base64"].as_str().unwrap_or_default())
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let commands = commands.join("\n");
        let state = world.final_state(&worker.fs).await.map_err(|e| e.to_string())?;
        worker.shutdown().await.map_err(|e| e.to_string())?;
        assert!(
            output.status.success(),
            "parse={parse}; password={password}; status={:?}\n{text}\n{errors}\n{transcript}",
            output.status.code()
        );
        assert!(!errors.contains("not supported by this build yet"));
        assert!(!transcript.contains("\"handler\":\"unmatched\""), "{transcript}");
        if parse {
            let tests = std::fs::read_to_string(root.join("report/parsed.yml"))?;
            assert!(tests.contains("test1"));
            assert!(
                !commands.contains("simctl shutdown"),
                "C26: successful parse skips provider termination"
            );
        } else {
            assert!(commands.contains("test-without-building"), "{commands}");
            assert!(text.contains("test1"));
            assert!(
                commands.contains("log stream --type log --color none --style compact"),
                "production DeviceLogListener missing"
            );
            assert!(commands.contains("simctl shutdown"));
            assert_eq!(state["devices"][UDID]["state"], "Shutdown");
            assert!(root.join("report").is_dir());
        }
        if !parse {
            world.validate().map_err(|e| e.to_string())?;
        } else {
            assert!(state["sessions"].as_array().is_some_and(Vec::is_empty));
        }
        if !password {
            evidence.push(serde_json::json!({"vendor":kind,"argv":if parse{vec!["parse","-o","parsed"]}else{vec![]},"exit":output.status.code(),"defaultNm":commands.contains("nm -gUj"),"batch":commands.contains("test-without-building"),"deviceLog":commands.contains("log stream --type log"),"shutdown":commands.contains("simctl shutdown"),"unmatched":0}));
        }
    }
    if let Some(path) = std::env::var_os("MRS55B_SMOKE_EVIDENCE") {
        std::fs::write(path, serde_json::to_vec_pretty(&evidence)?)?;
    }
    Ok(())
}

const SUPERVISOR_ROOT: &str = "/tmp/marathon-supervisor-0123456789";

fn supervised_token<'a>(command: &'a str, mode: &str) -> Option<&'a str> {
    let wire = command.strip_prefix("exec ").unwrap_or(command);
    wire.strip_prefix(&format!(
        "/usr/bin/perl {SUPERVISOR_ROOT}/supervisor.pl {mode} {SUPERVISOR_ROOT} "
    ))?
    .split_ascii_whitespace()
    .nth(1)
}

struct LatencySupervisor {
    plain: fake_worker::router::Router,
    movies: tokio::sync::Mutex<std::collections::HashMap<String, Vec<fake_worker::script::PublishedFile>>>,
    /// D98: every recorder exits 16 at once with this stderr, as simctl does on a simulator whose video service is
    /// busy (or with some other stderr, for a status 16 that is not that refusal).
    failing: Option<&'static str>,
    failed: tokio::sync::Mutex<std::collections::HashSet<String>>,
    /// The world's per-test recorder barriers, released at the first failed launch: a simulator with video off
    /// launches no recorder for the tests after it. Empty where every test's recorder is expected to launch: each test
    /// then waits for its own recorder, so the launch is ordered before the test's end.
    barriers: Vec<String>,
}

impl fake_worker::router::Handler for LatencySupervisor {
    fn name(&self) -> &str {
        "latency-supervisor"
    }

    fn route<'a>(
        &'a self,
        request: &'a fake_worker::router::Request,
        fs: &'a fake_worker::fs::VirtualFs,
    ) -> fake_worker::router::HandlerFuture<'a> {
        use fake_worker::{
            router::{Plan, Request},
            script::Action,
        };
        Box::pin(async move {
            let argv: Vec<_> = request.argv.iter().map(String::as_str).collect();
            match argv.as_slice() {
                ["/usr/bin/perl", "-MMIME::Base64", "-e", _, "--", _, _] => {
                    Ok(Some(Action::output(&format!("MARATHON_D96_1 {SUPERVISOR_ROOT}\n"), 0)))
                }
                ["/usr/bin/perl", _, "retire", SUPERVISOR_ROOT, _] => {
                    Ok(Some(Action::output("MARATHON_D96_1 retired\n", 0)))
                }
                ["/usr/bin/perl", _, "control", SUPERVISOR_ROOT, _, item] => {
                    let (token, operation) = item
                        .split_once(':')
                        .ok_or_else(|| std::io::Error::other("invalid item"))?;
                    if let Some(stderr) = self.failing
                        && self.failed.lock().await.contains(token)
                    {
                        let stderr = base64::engine::general_purpose::STANDARD.encode(stderr);
                        return Ok(Some(Action::output(&format!("{token} done:16 {stderr}\n"), 0)));
                    }
                    if operation == "1" {
                        let files = self
                            .movies
                            .lock()
                            .await
                            .remove(token)
                            .ok_or_else(|| std::io::Error::other("missing recorder"))?;
                        for file in files {
                            fs.write(
                                &file.path,
                                base64::engine::general_purpose::STANDARD.decode(file.base64)?,
                                0o600,
                            )
                            .await?;
                        }
                    }
                    // D98: a live recorder has printed simctl's start marker.
                    let status = if self.movies.lock().await.contains_key(token) {
                        "recording:1000000"
                    } else {
                        "done:0"
                    };
                    Ok(Some(Action::output(&format!("{token} {status}\n"), 0)))
                }
                [
                    "exec",
                    "/usr/bin/perl",
                    _,
                    mode @ ("run" | "detach"),
                    SUPERVISOR_ROOT,
                    _,
                    token,
                    payload @ ..,
                ] => {
                    let wire = match payload {
                        ["/bin/sh", "-c", command] => (*command).to_owned(),
                        _ => payload.join(" "),
                    };
                    let routed = self.plain.route(&Request::parse(wire.as_bytes())?, fs).await?;
                    if routed.unmatched {
                        return Err(std::io::Error::other(format!("unexpected supervised payload {wire}")).into());
                    }
                    match (mode, routed.plan) {
                        (&"run", Plan::Action(action)) => Ok(Some(action)),
                        (&"detach", Plan::Action(Action::Publish { started, .. })) if self.failing.is_some() => {
                            self.failed.lock().await.insert((*token).to_owned());
                            // The world's tests wait for their recorders' launch, not for their movies.
                            for file in started {
                                fs.write(
                                    &file.path,
                                    base64::engine::general_purpose::STANDARD.decode(file.base64)?,
                                    0o600,
                                )
                                .await?;
                            }
                            for barrier in &self.barriers {
                                fs.write(barrier, vec![], 0o600).await?;
                            }
                            Ok(Some(Action::output("", 0)))
                        }
                        (&"detach", Plan::Action(Action::Publish { files, started, .. })) => {
                            self.movies.lock().await.insert((*token).to_owned(), files);
                            for file in started {
                                fs.write(
                                    &file.path,
                                    base64::engine::general_purpose::STANDARD.decode(file.base64)?,
                                    0o600,
                                )
                                .await?;
                            }
                            Ok(Some(Action::output("", 0)))
                        }
                        _ => Err(std::io::Error::other(format!("unsupported supervised payload {wire}")).into()),
                    }
                }
                _ => Ok(None),
            }
        })
    }
}

async fn delayed_ssh(port: u16) -> Result<(u16, tokio::task::JoinHandle<()>)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn relay(
        mut input: tokio::net::tcp::OwnedReadHalf,
        mut output: tokio::net::tcp::OwnedWriteHalf,
    ) -> std::io::Result<()> {
        let mut buf = [0u8; 32768];
        loop {
            let count = input.read(&mut buf).await?;
            if count == 0 {
                return output.shutdown().await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            output.write_all(&buf[..count]).await?;
        }
    }
    let listen = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let listen_port = listen.local_addr()?.port();
    Ok((
        listen_port,
        tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                    accepted = listen.accept() => {
                        let Ok((left, _)) = accepted else { break };
                        connections.spawn(async move {
                            let right = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
                            left.set_nodelay(true)?;
                            right.set_nodelay(true)?;
                            let (from_left, to_left) = left.into_split();
                            let (from_right, to_right) = right.into_split();
                            tokio::try_join!(relay(from_left, to_right), relay(from_right, to_left))
                        });
                    }
                }
            }
        }),
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn six_video_batches_keep_ssh_supervision_off_the_healthy_path_d96() -> Result {
    use std::time::{Duration, Instant};
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let scratch = tempfile::tempdir()?;
    let host = scratch.path().join("host");
    let auth = scratch.path().join("key");
    key(&host).await?;
    key(&auth).await?;
    let mut timings = Vec::new();
    for supervised in [false, true] {
        let dir = tempfile::tempdir_in(scratch.path())?;
        let root = dir.path();
        bundle(root, "App", "app")?;
        bundle(root, "Tests", "xctest")?;
        let mut scenario: Scenario = serde_json::from_slice(&std::fs::read(
            repo.join("fixtures/fake-worker/apple/ios-media/script/world.json"),
        )?)?;
        scenario.expectations.clear();
        scenario.target = "Tests".into();
        scenario.tests = (1..=6)
            .map(|index| fake_worker::apple::Test {
                class: "SampleTests".into(),
                method: format!("testCase{index}"),
                status: "passed".into(),
            })
            .collect();
        let world = World::new(scenario).map_err(|e| e.to_string())?;
        let mut handlers: Vec<Arc<dyn fake_worker::router::Handler>> = vec![Arc::new(world.clone())];
        if supervised {
            handlers.insert(
                0,
                Arc::new(LatencySupervisor {
                    plain: fake_worker::router::Router {
                        script: Default::default(),
                        rsync_remote: "/usr/bin/rsync".into(),
                        rsync_binary: "/usr/bin/rsync".into(),
                        extensions: handlers.clone(),
                    },
                    movies: Default::default(),
                    failing: None,
                    failed: Default::default(),
                    barriers: vec![],
                }),
            );
        }
        let transcript = root.join("transcript.jsonl");
        let worker = fake_worker::start(
            Config {
                listen: "127.0.0.1:0".parse()?,
                root: root.join("worker"),
                transcript: transcript.clone(),
                host_key: host.clone(),
                auth: AuthPolicy {
                    user: "fixture".into(),
                    password: None,
                    authorized_keys: vec![auth.with_extension("pub")],
                    keyboard_interactive: false,
                    password_auth: false,
                },
                max_sessions: 10,
                rsync_binary: "/usr/bin/rsync".into(),
                rsync_remote: "/usr/bin/rsync".into(),
                script: Default::default(),
                fixtures: repo.join("fixtures/fake-worker"),
                run: "ios-supervisor-latency".into(),
            },
            handlers,
        )
        .await
        .map_err(|e| e.to_string())?;
        let (port, proxy) = delayed_ssh(worker.ready.endpoint.port()).await?;
        std::fs::write(
            root.join("Marathondevices"),
            format!(
                "workers:\n- transport: {{type: ssh, addr: 127.0.0.1, port: {port}, checkReachability: false}}\n  devices: [{{type: simulator, udid: {UDID}}}]\n"
            ),
        )?;
        std::fs::write(
            root.join("Marathonfile"),
            format!(
                "name: ios-latency\noutputDir: report\ndebug: true\nanalyticsTracking: false\nbugsnagReporting: false\nuncompletedTestRetryQuota: 0\nretryStrategy: {{type: no-retry}}\nbatchingStrategy: {{type: fixed-size, size: 1, lastMileLength: 0}}\nscreenRecordingPolicy: ON_ANY\nvendorConfiguration:\n  type: iOS\n  bundle:\n    application: App.app\n    testApplication: Tests.xctest\n    testType: xctest\n  ssh:\n    authentication: {{type: publicKey, username: fixture, key: '{}'}}\n    keepAliveInterval: PT15S\n    shareWorkerConnection: true\n  lifecycle:\n    shutdownUnused: false\n    onPrepare: []\n    onDispose: [SHUTDOWN]\n  deviceLog: false\n  xcresult:\n    pullingPolicy: ALWAYS\n    remoteClean: true\n  screenRecordConfiguration:\n    preferableRecorderType: VIDEO\n    videoConfiguration:\n      enabled: true\n      transcoding: {{enabled: false}}\n    screenshotConfiguration: {{enabled: false}}\n",
                auth.display()
            ),
        )?;
        let started = Instant::now();
        let output = tokio::time::timeout(
            Duration::from_secs(120),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_marathon"))
                .current_dir(root)
                .kill_on_drop(true)
                .env("FW_REAL_SSH", "/usr/bin/ssh")
                .env("FW_ARGV_LOG", root.join("host-argv.jsonl"))
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        repo.join("tools/fake-worker/shims").display(),
                        std::env::var("PATH").unwrap_or_default()
                    ),
                )
                .output(),
        )
        .await??;
        let elapsed = started.elapsed();
        proxy.abort();
        let trace = std::fs::read_to_string(transcript)?;
        let rows: Vec<serde_json::Value> = trace
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
        let wire = |row: &serde_json::Value| {
            let raw = row["detail"]["base64"].as_str().unwrap();
            let wire = base64::engine::general_purpose::STANDARD.decode(raw).unwrap();
            String::from_utf8(wire).unwrap()
        };
        let execs = rows
            .iter()
            .filter(|row| row["event"] == "exec")
            .map(wire)
            .collect::<Vec<_>>();
        worker.shutdown().await.map_err(|e| e.to_string())?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}\n{}\n{}",
            String::from_utf8_lossy(&output.stderr),
            execs.join("\n")
        );
        assert_eq!(
            execs.iter().filter(|cmd| cmd.contains("test-without-building")).count(),
            6
        );
        let media = root
            .join("report/video")
            .read_dir()?
            .flat_map(|pool| pool.unwrap().path().read_dir().unwrap())
            .flat_map(|device| device.unwrap().path().read_dir().unwrap())
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "mp4")
            })
            .count();
        assert_eq!(media, 6, "{stdout}");
        for artifact in [".mp4", ".xcresult"] {
            assert_eq!(
                execs
                    .iter()
                    .filter(|cmd| cmd.contains("--sender") && cmd.contains(artifact))
                    .count(),
                6,
                "{artifact} sender execs"
            );
        }
        let before_batch = execs.iter().take_while(|cmd| !cmd.contains("test-without-building"));
        let preparation = before_batch.clone().count();
        if supervised {
            assert_eq!(execs.iter().filter(|cmd| cmd.contains(" control-stream ")).count(), 1);
            assert!(!execs.iter().any(|cmd| cmd.contains("/supervisor.pl control ")));
            let requests = rows
                .iter()
                .filter(|row| row["event"] == "supervisor_request")
                .map(wire)
                .collect::<Vec<_>>();
            let operations = |token| {
                requests
                    .iter()
                    .filter_map(|request| request.split_ascii_whitespace().last()?.split_once(':'))
                    .filter_map(|(owner, operation)| (owner == token).then_some(operation))
                    .collect::<Vec<_>>()
            };
            let foreground = execs
                .iter()
                .filter_map(|cmd| supervised_token(cmd, "run"))
                .collect::<Vec<_>>();
            assert_eq!(foreground.len(), 66, "foreground execs across the run");
            for token in foreground {
                assert!(operations(token).is_empty(), "foreground token {token}");
            }
            let prep_foreground = before_batch
                .clone()
                .filter(|cmd| supervised_token(cmd, "run").is_some())
                .count();
            assert_eq!(prep_foreground, 32, "foreground preparation execs");
            // Recorder launch and control setup can interleave with the first batch.
            assert_eq!(
                before_batch
                    .filter(|cmd| !cmd.contains(" control-stream ") && supervised_token(cmd, "detach").is_none())
                    .count(),
                36,
                "preparation execs including bootstrap and bundle staging"
            );
            let probes = execs.iter().filter(|cmd| cmd.starts_with("/usr/bin/perl -M")).count();
            assert_eq!(probes, 1);
            let senders = execs
                .iter()
                .filter_map(|cmd| supervised_token(cmd, "rsync"))
                .collect::<Vec<_>>();
            assert_eq!(senders.len(), 12, "one exec per pull");
            for token in senders {
                assert!(operations(token).is_empty(), "sender token {token}");
            }
            let recorders = execs
                .iter()
                .filter_map(|cmd| supervised_token(cmd, "detach"))
                .collect::<Vec<_>>();
            assert_eq!(recorders.len(), 6, "one exec per recorder start");
            // D98: at most one status at launch learns that the recorder confirmed its start (a stop that comes first
            // makes it unnecessary); then its stop.
            for token in &recorders {
                let operations = operations(token);
                assert!(
                    operations == ["status", "1"] || operations == ["1"],
                    "recorder start and stop for token {token}: {operations:?}"
                );
            }
            assert!((6..=12).contains(&requests.len()), "control requests across the run");
            assert_eq!(execs.len(), 90, "SSH execs across the run");
        } else {
            assert!(stdout.contains("has no usable /usr/bin/perl"));
        }
        println!(
            "50 ms RTT, supervised={supervised}, elapsed={elapsed:?}, {} SSH execs ({preparation} before the first batch)",
            execs.len()
        );
        timings.push(elapsed);
    }
    assert!(
        timings[1].as_secs_f64() <= timings[0].as_secs_f64() * 1.05,
        "plain {:?}, supervised {:?}",
        timings[0],
        timings[1]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_busy_simulator_on_an_ssh_worker_turns_video_off_once_d98() -> Result {
    use std::time::Duration;
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let scratch = tempfile::tempdir()?;
    let host = scratch.path().join("host");
    let auth = scratch.path().join("key");
    key(&host).await?;
    key(&auth).await?;
    // simctl's busy refusal as the supervisor's receipt carries it, and a status 16 without it.
    let busy = "Note: No display specified. Defaulting to display: 00000000-0000-0000-0000-000000000000 (screenID: 1, name: LCD)\n\
                Error starting video recorder: Error Domain=NSPOSIXErrorDomain Code=16 \"Resource busy\" \
                UserInfo={NSLocalizedFailureReason=A recording is already in progress}.\n\
                An error was encountered processing the command (domain=NSPOSIXErrorDomain, code=16):\n\
                The operation couldn’t be completed. A recording is already in progress\n\
                A recording is already in progress\n";
    let other = "An error was encountered processing the command (domain=NSPOSIXErrorDomain, code=16):\n";
    for stderr in [busy, other] {
        let dir = tempfile::tempdir_in(scratch.path())?;
        let root = dir.path();
        bundle(root, "App", "app")?;
        bundle(root, "Tests", "xctest")?;
        let mut scenario: Scenario = serde_json::from_slice(&std::fs::read(
            repo.join("fixtures/fake-worker/apple/ios-media/script/world.json"),
        )?)?;
        scenario.expectations.clear();
        scenario.target = "Tests".into();
        scenario.tests = (1..=3)
            .map(|index| fake_worker::apple::Test {
                class: "SampleTests".into(),
                method: format!("testCase{index}"),
                status: "passed".into(),
            })
            .collect();
        let world = World::new(scenario).map_err(|e| e.to_string())?;
        let mut handlers: Vec<Arc<dyn fake_worker::router::Handler>> = vec![Arc::new(world.clone())];
        handlers.insert(
            0,
            Arc::new(LatencySupervisor {
                plain: fake_worker::router::Router {
                    script: Default::default(),
                    rsync_remote: "/usr/bin/rsync".into(),
                    rsync_binary: "/usr/bin/rsync".into(),
                    extensions: handlers.clone(),
                },
                movies: Default::default(),
                failing: Some(stderr),
                failed: Default::default(),
                barriers: if stderr == busy {
                    (1..=3)
                        .map(|index| format!("/tmp/marathon/.apple-world/testCase{index}.ready"))
                        .collect()
                } else {
                    vec![]
                },
            }),
        );
        let transcript = root.join("transcript.jsonl");
        let worker = fake_worker::start(
            Config {
                listen: "127.0.0.1:0".parse()?,
                root: root.join("worker"),
                transcript: transcript.clone(),
                host_key: host.clone(),
                auth: AuthPolicy {
                    user: "fixture".into(),
                    password: None,
                    authorized_keys: vec![auth.with_extension("pub")],
                    keyboard_interactive: false,
                    password_auth: false,
                },
                max_sessions: 10,
                rsync_binary: "/usr/bin/rsync".into(),
                rsync_remote: "/usr/bin/rsync".into(),
                script: Default::default(),
                fixtures: repo.join("fixtures/fake-worker"),
                run: "ios-busy-recorder".into(),
            },
            handlers,
        )
        .await
        .map_err(|e| e.to_string())?;
        let port = worker.ready.endpoint.port();
        std::fs::write(
            root.join("Marathondevices"),
            format!(
                "workers:\n- transport: {{type: ssh, addr: 127.0.0.1, port: {port}, checkReachability: false}}\n  devices: [{{type: simulator, udid: {UDID}}}]\n"
            ),
        )?;
        std::fs::write(
            root.join("Marathonfile"),
            format!(
                "name: ios-busy\noutputDir: report\ndebug: true\nanalyticsTracking: false\nbugsnagReporting: false\nuncompletedTestRetryQuota: 0\nretryStrategy: {{type: no-retry}}\nbatchingStrategy: {{type: fixed-size, size: 1, lastMileLength: 0}}\nscreenRecordingPolicy: ON_ANY\nvendorConfiguration:\n  type: iOS\n  bundle:\n    application: App.app\n    testApplication: Tests.xctest\n    testType: xctest\n  ssh:\n    authentication: {{type: publicKey, username: fixture, key: '{}'}}\n    keepAliveInterval: PT15S\n    shareWorkerConnection: true\n  lifecycle:\n    shutdownUnused: false\n    onPrepare: []\n    onDispose: [SHUTDOWN]\n  deviceLog: false\n  xcresult:\n    pullingPolicy: ALWAYS\n    remoteClean: true\n  screenRecordConfiguration:\n    preferableRecorderType: VIDEO\n    videoConfiguration:\n      enabled: true\n      transcoding: {{enabled: false}}\n    screenshotConfiguration: {{enabled: false}}\n",
                auth.display()
            ),
        )?;
        let output = tokio::time::timeout(
            Duration::from_secs(120),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_marathon"))
                .current_dir(root)
                .kill_on_drop(true)
                .env("FW_REAL_SSH", "/usr/bin/ssh")
                .env("FW_ARGV_LOG", root.join("host-argv.jsonl"))
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        repo.join("tools/fake-worker/shims").display(),
                        std::env::var("PATH").unwrap_or_default()
                    ),
                )
                .output(),
        )
        .await??;
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(transcript)?
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
        let execs = rows
            .iter()
            .filter(|row| row["event"] == "exec")
            .map(|row| {
                let raw = row["detail"]["base64"].as_str().unwrap();
                String::from_utf8(base64::engine::general_purpose::STANDARD.decode(raw).unwrap()).unwrap()
            })
            .collect::<Vec<_>>();
        worker.shutdown().await.map_err(|e| e.to_string())?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr_text = String::from_utf8_lossy(&output.stderr);
        // Video never changes a test's outcome or the exit status.
        assert!(output.status.success(), "{stdout}\n{stderr_text}");
        assert_eq!(
            execs.iter().filter(|cmd| cmd.contains("test-without-building")).count(),
            3
        );
        let launches = execs
            .iter()
            .filter(|cmd| supervised_token(cmd, "detach").is_some())
            .count();
        let warnings = format!("{stdout}{stderr_text}")
            .matches("video service is busy")
            .count();
        if stderr == busy {
            assert_eq!(launches, 1, "no recorder after the busy refusal");
            assert_eq!(warnings, 1, "{stdout}");
            assert!(stdout.contains(&format!("xcrun simctl shutdown {UDID}")));
        } else {
            assert_eq!(launches, 3, "a status 16 without the busy refusal leaves video on");
            assert_eq!(warnings, 0, "{stdout}");
        }
    }
    Ok(())
}
