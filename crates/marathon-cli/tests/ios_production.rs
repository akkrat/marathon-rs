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
    for (kind, parse) in [
        ("iOS", false),
        ("iOS", true),
        ("watchOS", true),
        ("tvOS", true),
        ("visionOS", true),
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
                    password: None,
                    authorized_keys: vec![root.join("key.pub")],
                    keyboard_interactive: false,
                    password_auth: false,
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
        std::fs::write(
            root.join("Marathonfile"),
            format!(
                "name: ios-cli\noutputDir: report\ndebug: true\nanalyticsTracking: false\nbugsnagReporting: false\nuncompletedTestRetryQuota: 0\nretryStrategy: {{type: no-retry}}\nvendorConfiguration:\n  type: {kind}\n  bundle:\n    application: App.app\n    testApplication: Tests.xctest\n    testType: xctest\n  ssh:\n    authentication: {{type: publicKey, username: fixture, key: '{}'}}\n    keepAliveInterval: PT0S\n  lifecycle:\n    shutdownUnused: false\n    onPrepare: []\n    onDispose: [SHUTDOWN]\n  deviceLog: true\n  xcresult:\n    pullingPolicy: NEVER\n  screenRecordConfiguration:\n    videoConfiguration:\n      enabled: false\n      transcoding: {{enabled: false}}\n    screenshotConfiguration: {{enabled: false}}\n",
                auth.display()
            ),
        )?;
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_marathon"));
        command.current_dir(root).kill_on_drop(true);
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
            "parse={parse}; status={:?}\n{text}\n{errors}\n{transcript}",
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
        evidence.push(serde_json::json!({"vendor":kind,"argv":if parse{vec!["parse","-o","parsed"]}else{vec![]},"exit":output.status.code(),"defaultNm":commands.contains("nm -gUj"),"batch":commands.contains("test-without-building"),"deviceLog":commands.contains("log stream --type log"),"shutdown":commands.contains("simctl shutdown"),"unmatched":0}));
    }
    if let Some(path) = std::env::var_os("MRS55B_SMOKE_EVIDENCE") {
        std::fs::write(path, serde_json::to_vec_pretty(&evidence)?)?;
    }
    Ok(())
}
