//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/IosVendor.kt
//! U163 through the built binary, so the real scheduler, pool, device actor and bridge, with fake workers. `RSYNC4J_RSYNC`
//! names a stand-in that counts the sends to one worker and fails them, and hands the other worker to the real rsync.
use fake_worker::{
    Config,
    apple::{Scenario, UDID, World},
    server::AuthPolicy,
};
use std::{os::unix::fs::PermissionsExt, path::Path, sync::Arc};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn key(path: &Path) -> Result {
    let result = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(path)
        .output()?;
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
/// One fake worker with one simulator and one passing test.
async fn worker(root: &Path, name: &str) -> Result<fake_worker::server::RunningWorker> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let host = root.join(format!("{name}-host"));
    key(&host)?;
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
    Ok(fake_worker::start(
        Config {
            listen: "127.0.0.1:0".parse()?,
            root: root.join(format!("{name}-root")),
            transcript: root.join(format!("{name}-transcript.jsonl")),
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
            run: format!("stall-{name}"),
        },
        vec![Arc::new(world)],
    )
    .await
    .map_err(|e| e.to_string())?)
}
/// A run on the given workers, its rsync failing to `failing` with `failure`, a piece of shell. The test bundle gets through
/// unless `parsing` says otherwise: remote parsing sends it from whichever device it borrows, and its failure is the run's.
/// The first send to `held`, if any, other than the test bundle that parsing needs, waits `HOLD` first, so that worker is
/// still busy when the stall is decided. Keep-alive is on, since the fake worker drops a connection idle for 300 s.
fn run(
    root: &Path,
    ports: &[u16],
    failing: u16,
    failure: &str,
    parsing: bool,
    held: Option<u16>,
) -> Result<tokio::process::Command> {
    let workers: String = ports
        .iter()
        .map(|port| {
            format!(
                "- transport: {{type: ssh, addr: 127.0.0.1, port: {port}, checkReachability: false}}\n  devices: [{{type: simulator, udid: {UDID}}}]\n"
            )
        })
        .collect();
    std::fs::write(root.join("Marathondevices"), format!("workers:\n{workers}"))?;
    std::fs::write(
        root.join("Marathonfile"),
        format!(
            "name: stall\noutputDir: report\nanalyticsTracking: false\nbugsnagReporting: false\nuncompletedTestRetryQuota: 0\nretryStrategy: {{type: no-retry}}\ndeviceInitializationTimeoutMillis: 1200000\nvendorConfiguration:\n  type: iOS\n  bundle:\n    application: App.app\n    testApplication: Tests.xctest\n    testType: xctest\n  ssh:\n    authentication: {{type: publicKey, username: fixture, key: '{}'}}\n    keepAliveInterval: PT15S\n  lifecycle:\n    shutdownUnused: false\n    onPrepare: []\n    onDispose: [SHUTDOWN]\n  xcresult:\n    pullingPolicy: NEVER\n  screenRecordConfiguration:\n    videoConfiguration:\n      enabled: false\n      transcoding: {{enabled: false}}\n    screenshotConfiguration: {{enabled: false}}\n",
            root.join("key").display()
        ),
    )?;
    let spared = if parsing { "" } else { "*.xctest*) ;; " };
    let hold = held.map_or(String::new(), |port| {
        let mark = root.join("held");
        format!(
            "case \"$*\" in *.xctest*) ;; *\" -p {port} \"*) [ -e \"{0}\" ] || {{ touch \"{0}\"; {HOLD}; }};; esac\n",
            mark.display()
        )
    });
    let rsync = root.join("rsync");
    std::fs::write(
        &rsync,
        format!(
            "#!/bin/bash\ncase \"$*\" in {spared}*\" -p {failing} \"*) echo send >> \"{}\"; {failure};; esac\n{hold}exec /usr/bin/rsync \"$@\"\n",
            root.join("sends").display()
        ),
    )?;
    std::fs::set_permissions(&rsync, std::fs::Permissions::from_mode(0o700))?;
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_marathon"));
    command
        .current_dir(root)
        .env("RSYNC4J_RSYNC", &rsync)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    Ok(command)
}
fn sends(root: &Path) -> usize {
    std::fs::read_to_string(root.join("sends")).map_or(0, |text| text.lines().count())
}
/// openrsync's report as it prints it, and the status it had here once its ssh ended. A stall needs 285 s with no progress
/// first (D31), so a stalled send waits that out on the real clock, with a margin.
const STALL: &str = "/bin/sleep 295; echo \"rsync($$): error: poll: timeout\" >&2; exit 16";
/// The same report at once, which is not a stall.
const QUICK_REPORT: &str = "echo \"rsync($$): error: poll: timeout\" >&2; exit 16";
const OTHER_FAILURE: &str = "echo 'rsync: some other failure' >&2; exit 23";
/// Longer than `STALL` and the run after it, so the held worker is still busy when the other's stall is decided.
const HOLD: &str = "/bin/sleep 340";
const CATCH: std::time::Duration = std::time::Duration::from_secs(120);
/// For the runs with real stalls, each of which is 295 s, three in a row on the parsing run.
const STALL_CATCH: std::time::Duration = std::time::Duration::from_secs(1500);
fn fixture(root: &Path) -> Result {
    key(&root.join("key"))?;
    bundle(root, "App", "app")?;
    bundle(root, "Tests", "xctest")
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "each stall takes 295 s on the real clock (D31); run by hand with --ignored"]
async fn a_stalled_send_costs_its_simulator_one_send_and_the_other_worker_finishes_the_run_u163() -> Result {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    fixture(root)?;
    let (stuck, healthy) = (worker(root, "stuck").await?, worker(root, "healthy").await?);
    let stuck_port = stuck.ready.endpoint.port();
    let healthy_port = healthy.ready.endpoint.port();
    let mut command = run(
        root,
        &[stuck_port, healthy_port],
        stuck_port,
        STALL,
        false,
        Some(healthy_port),
    )?;
    let output = tokio::time::timeout(STALL_CATCH, command.output()).await??;
    let text = String::from_utf8_lossy(&output.stdout);
    let errors = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{text}\n{errors}");
    assert_eq!(sends(root), 1, "{text}\n{errors}");
    assert!(text.contains("test1"), "{text}\n{errors}");
    assert!(
        text.contains("the device leaves its pool") || errors.contains("the device leaves its pool"),
        "{text}\n{errors}"
    );
    stuck.shutdown().await.map_err(|e| e.to_string())?;
    healthy.shutdown().await.map_err(|e| e.to_string())?;
    Ok(())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "each stall takes 295 s on the real clock (D31); run by hand with --ignored"]
async fn a_stalled_send_on_the_only_worker_ends_the_run_after_one_send_u163() -> Result {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    fixture(root)?;
    let stuck = worker(root, "stuck").await?;
    let port = stuck.ready.endpoint.port();
    let mut command = run(root, &[port], port, STALL, false, None)?;
    let output = tokio::time::timeout(STALL_CATCH, command.output()).await??;
    let text = String::from_utf8_lossy(&output.stdout);
    let errors = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{text}\n{errors}");
    assert_eq!(sends(root), 1, "{text}\n{errors}");
    stuck.shutdown().await.map_err(|e| e.to_string())?;
    Ok(())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "each stall takes 295 s on the real clock (D31); run by hand with --ignored"]
async fn a_stalled_send_while_parsing_is_sent_once_per_borrowed_device_u163() -> Result {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    fixture(root)?;
    let stuck = worker(root, "stuck").await?;
    let port = stuck.ready.endpoint.port();
    let mut command = run(root, &[port], port, STALL, true, None)?;
    let output = tokio::time::timeout(STALL_CATCH, command.output()).await??;
    let text = String::from_utf8_lossy(&output.stdout);
    let errors = String::from_utf8_lossy(&output.stderr);
    // Core's three borrows, and not three more on each.
    assert_eq!(output.status.code(), Some(1), "{text}\n{errors}");
    assert_eq!(sends(root), 3, "{text}\n{errors}");
    stuck.shutdown().await.map_err(|e| e.to_string())?;
    Ok(())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_that_fails_any_other_way_keeps_both_retries_u163() -> Result {
    keeps_both_retries(OTHER_FAILURE).await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timeout_report_that_comes_at_once_keeps_both_retries_d31() -> Result {
    // rsync's own report, as a whole line, but long before 300 s could have passed: not a stall, so no `LeavePool`.
    keeps_both_retries(QUICK_REPORT).await
}
async fn keeps_both_retries(failure: &str) -> Result {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    fixture(root)?;
    let stuck = worker(root, "stuck").await?;
    let port = stuck.ready.endpoint.port();
    let mut child = run(root, &[port], port, failure, false, None)?.spawn()?;
    // More than one is the device's own retry, 10 s later, which is as long as this test waits. The 90 of the full chain were
    // counted once, in docs/wp/x26-findings.md, since they take six minutes.
    let reached = tokio::time::timeout(CATCH, async {
        while sends(root) <= 1 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
    let early_exit = child.try_wait()?;
    child.kill().await?;
    assert!(
        reached.is_ok() && early_exit.is_none(),
        "{} sends, {early_exit:?}",
        sends(root)
    );
    stuck.shutdown().await.map_err(|e| e.to_string())?;
    Ok(())
}
