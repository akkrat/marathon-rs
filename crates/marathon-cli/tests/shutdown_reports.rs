//! Port of core/src/main/kotlin/com/malinskiy/marathon/Marathon.kt (`installShutdownHook`, `onFinish`) and ShutdownHook.kt.
//!
//! D52 and U161 through the built binary, with real signals: on fake adb, a cancel mid-test in both debug modes, and on
//! the fake worker, a cancel with a process that takes its time to stop. Every wait is on something observed, but for
//! the window in which a test that ends during the cleanup could be started again (U161), which a slow host can only
//! narrow.
#![cfg(unix)]
use base64::Engine;
use fake_worker::{
    Config,
    apple::{Scenario, UDID, World},
    script::{Action, Matcher, Rule, Script as WorkerScript},
    server::AuthPolicy,
};
use marathon_adb::server::{FakeAdbServer, Script, ServerOptions};
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use serde_json::Value;
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Only catches hangs (docs/testing.md).
const PATIENCE: Duration = Duration::from_secs(60);
/// The queue hands these out in this order. The name doesn't matter: the script says how each one ends.
const FINISHED: &str = "com.example.MainActivityAllureTest#testFailure";
const HANGING: &str = "com.example.MainActivityTest#testText";
/// Far beyond the time from seeing the request to sending the signal, or the test would retry for a reason of its own.
const HANGING_ENDS_MILLIS: u64 = 20_000;
/// The gate that ends `HANGING` in the test where it ends during the cleanup.
const CLEANUP_GATE: &str = "cleanup";
/// How long after its end the test has to come back. A live queue starts it again in milliseconds. Short of D52's 10 s
/// head start, so the device is up all the while.
const BOUNCE_WINDOW_MILLIS: u64 = 3_000;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

async fn until(what: &str, mut seen: impl FnMut() -> bool) -> Result {
    let deadline = Instant::now() + PATIENCE;
    while !seen() {
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {what}").into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(())
}

async fn exited(child: &mut tokio::process::Child) -> Result<ExitStatus> {
    Ok(tokio::time::timeout(PATIENCE, child.wait()).await??)
}

fn signal(child: &tokio::process::Child, signal: Signal) -> Result {
    let pid = child.id().ok_or("the marathon process has already exited")?;
    kill(Pid::from_raw(i32::try_from(pid)?), signal)?;
    Ok(())
}

fn marathon(cwd: &Path, stdout: &Path, stderr: &Path) -> Result<tokio::process::Child> {
    Ok(tokio::process::Command::new(env!("CARGO_BIN_EXE_marathon"))
        .current_dir(cwd)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(stdout)?)
        .stderr(std::fs::File::create(stderr)?)
        .kill_on_drop(true)
        .spawn()?)
}

/// Everything under `dir`, relative, sorted.
fn files(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(relative) = path.strip_prefix(root) {
                out.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// How `HANGING` ends: with no result, after a time or once the test opens a gate.
enum HangingEnds {
    After(u64),
    AtGate(&'static str),
}

/// The devices' script from the runner fixture, with `FINISHED` passing and `HANGING` starting and ending much later with
/// no result. That end is what U161 needs: a batch that ends while the provider closes, on a client that then stays quiet.
fn adb_script(hanging: HangingEnds) -> Result<marathon_adb::server::CompiledScript> {
    let end = match hanging {
        HangingEnds::After(millis) => serde_json::json!({"sleep_ms": millis}),
        HangingEnds::AtGate(name) => serde_json::json!({"gate": name}),
    };
    let dir = repo().join("fixtures/fake-adb/android-runner-cli/script");
    let mut script: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("devices.yaml"))?)?;
    let started = |class: &str, test: &str| {
        format!(
            "INSTRUMENTATION_STATUS: class={class}\nINSTRUMENTATION_STATUS: test={test}\nINSTRUMENTATION_STATUS: numtests=1\nINSTRUMENTATION_STATUS_CODE: 1\n"
        )
    };
    let passed = started("com.example.MainActivityAllureTest", "testFailure")
        + "INSTRUMENTATION_STATUS: class=com.example.MainActivityAllureTest\nINSTRUMENTATION_STATUS: test=testFailure\nINSTRUMENTATION_STATUS_CODE: 0\nINSTRUMENTATION_CODE: -1\n";
    let rules = script["shell"].as_array_mut().ok_or("no shell rules")?;
    rules.insert(
        0,
        serde_json::json!({
            "regex": "^am instrument.*MainActivityTest",
            "output": [{"stdout": started("com.example.MainActivityTest", "testText")}, end],
        }),
    );
    rules.insert(
        0,
        serde_json::json!({"regex": "^am instrument.*MainActivityAllureTest", "stdout": passed}),
    );
    // The fixture's logcat rule points outside the script's directory; the content doesn't matter here.
    for rule in rules.iter_mut() {
        if rule["exact"] == "logcat -v long" {
            rule["stdout"] = "".into();
            rule.as_object_mut().ok_or("not a rule")?.remove("stdout_file");
        }
    }
    Ok(Script::from_yaml(&serde_json::to_string(&script)?)?.compile(&dir)?)
}

fn instruments(server: &FakeAdbServer, test: &str) -> usize {
    let class = test.split('#').next().unwrap_or(test);
    server
        .transcript()
        .iter()
        .filter(|row| {
            row["type"] == "request"
                && row["service"]
                    .as_str()
                    .is_some_and(|s| s.contains("am instrument") && s.contains(class))
        })
        .count()
}

/// The APKs, an adb that must not be called, and a Marathonfile for `FINISHED` and `HANGING` on the fake adb server at
/// `port`, with `extra` appended.
fn android_run(root: &Path, debug: bool, port: u16, extra: &str) -> Result {
    let apks = repo().join("fixtures/vendor-android/apk");
    for apk in ["app-debug.apk", "app-debug-androidTest.apk"] {
        std::fs::copy(apks.join(apk), root.join(apk))?;
    }
    // Never to be called: marathon must find the fake server up.
    let shim = root.join("sdk/platform-tools/adb");
    std::fs::create_dir_all(shim.parent().ok_or("no parent")?)?;
    std::fs::write(&shim, "#!/bin/sh\nexit 70\n")?;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))?;
    std::fs::write(
        root.join("Marathonfile"),
        format!(
            "name: shutdown\noutputDir: output\ndebug: {debug}\nanalyticsTracking: false\nbugsnagReporting: false\nuncompletedTestRetryQuota: 1000\nfilteringConfiguration:\n  allowlist:\n    - type: fully-qualified-test-name\n      values: [\"{FINISHED}\", \"{HANGING}\"]\nvendorConfiguration:\n  type: Android\n  androidSdk: sdk\n  applicationApk: app-debug.apk\n  testApplicationApk: app-debug-androidTest.apk\n  autoGrantPermission: false\n  applicationPmClear: false\n  testApplicationPmClear: false\n  waitForDevicesTimeoutMillis: 15000\n  adbServers:\n    - host: 127.0.0.1\n      port: {port}\n  screenRecordConfiguration:\n    videoConfiguration: {{enabled: false}}\n    screenshotConfiguration: {{enabled: false}}\n{extra}"
        ),
    )?;
    Ok(())
}

/// D52: with either `debug`, a cancel while a test runs writes the reports of what finished, without the test in flight
/// (A99). U161: nothing is started on the closed client, whose batches ended at once and came back, adding INCOMPLETE results.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_mid_test_writes_the_reports_and_does_not_bounce_the_test_d52_u161() -> Result {
    for debug in [false, true] {
        for (number, code) in [(Signal::SIGTERM, 143), (Signal::SIGINT, 130)] {
            let dir = tempfile::tempdir()?;
            let root = dir.path();
            let mut server = FakeAdbServer::start(
                adb_script(HangingEnds::After(HANGING_ENDS_MILLIS))?,
                ServerOptions::default(),
            )
            .await?;
            android_run(root, debug, server.port(), "")?;
            let (stdout, stderr) = (root.join("stdout"), root.join("stderr"));
            let mut child = marathon(root, &stdout, &stderr)?;
            let context = format!("debug={debug} {number}");
            let hung = until("the hanging test to start", || instruments(&server, HANGING) > 0).await;
            if let Err(error) = hung {
                let _ = child.kill().await;
                return Err(format!(
                    "{context}: {error}\n{}\n{}\n{:#?}",
                    std::fs::read_to_string(&stdout)?,
                    std::fs::read_to_string(&stderr)?,
                    server.transcript()
                )
                .into());
            }
            signal(&child, number)?;
            let status = exited(&mut child).await?;
            let (out, err) = (std::fs::read_to_string(&stdout)?, std::fs::read_to_string(&stderr)?);
            assert_eq!(status.code(), Some(code), "{context}\n{out}\n{err}");

            assert_eq!(instruments(&server, HANGING), 1, "{context}: the test bounced\n{out}");
            let results = root.join("output");
            let written = files(&results);
            assert!(
                written.iter().any(|f| f == "html/index.html"),
                "{context}: {written:?}\n{out}\n{err}"
            );
            let raw = std::fs::read_to_string(results.join("test_result/raw.json"))?;
            let rows: Vec<Value> = serde_json::from_str(&raw)?;
            assert_eq!(
                rows.len(),
                1,
                "{context}: only the finished test is in the reports\n{raw}\n{written:?}"
            );
            assert_eq!(rows[0]["method"], "testFailure", "{context}: {raw}");
            assert_eq!(rows[0]["status"], "PASSED", "{context}: {raw}");
            let summary = "Raw: 1 passed, 0 failed, 0 ignored, 0 incomplete tests";
            assert!(out.contains(summary), "{context}: {out}");
            assert!(
                out.contains("stopping the tests, writing the reports"),
                "{context}\n{out}\n{err}"
            );
            server.shutdown().await?;
        }
    }
    Ok(())
}

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

fn events(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

struct Worker {
    root: tempfile::TempDir,
    worker: fake_worker::server::RunningWorker,
    stdout: PathBuf,
    stderr: PathBuf,
    transcript: PathBuf,
}

/// An iOS run on a worker whose `xcodebuild` ignores TERM, so that D30 takes its 20 s up to the KILL.
async fn slow_to_stop(debug: bool) -> Result<(Worker, tokio::process::Child)> {
    let repo = repo();
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    let (host, auth) = (root.join("host"), root.join("key"));
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
    let transcript = root.join("transcript.jsonl");
    let worker = fake_worker::start(
        Config {
            listen: "127.0.0.1:0".parse()?,
            root: root.join("worker"),
            transcript: transcript.clone(),
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
            script: WorkerScript {
                rules: vec![Rule {
                    name: "xcodebuild, slow to stop".into(),
                    matcher: Matcher::Prefix("env NSUnbufferedIO=YES xcrun xcodebuild test-without-building ".into()),
                    action: Action::Output {
                        chunks: vec![],
                        status: 0,
                        signal: None,
                        hang: true,
                        ignore_signals: vec!["TERM".into()],
                        eof_before_exit: false,
                        close_before_eof: false,
                        withhold_close: false,
                    },
                }],
                ..Default::default()
            },
            fixtures: repo.join("fixtures/fake-worker"),
            run: "shutdown".into(),
        },
        vec![Arc::new(world)],
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
            "name: shutdown\noutputDir: report\ndebug: {debug}\nanalyticsTracking: false\nbugsnagReporting: false\nuncompletedTestRetryQuota: 1000\nretryStrategy: {{type: no-retry}}\nvendorConfiguration:\n  type: iOS\n  maxBatchesWithoutProgress: 0\n  bundle:\n    application: App.app\n    testApplication: Tests.xctest\n    testType: xctest\n  ssh:\n    authentication: {{type: publicKey, username: fixture, key: '{}'}}\n    keepAliveInterval: PT0S\n  lifecycle:\n    shutdownUnused: false\n    onPrepare: []\n    onDispose: [SHUTDOWN]\n  deviceLog: false\n  xcresult:\n    pullingPolicy: NEVER\n  screenRecordConfiguration:\n    videoConfiguration:\n      enabled: false\n      transcoding: {{enabled: false}}\n    screenshotConfiguration: {{enabled: false}}\n",
            auth.display()
        ),
    )?;
    let (stdout, stderr) = (root.join("stdout"), root.join("stderr"));
    let child = marathon(root, &stdout, &stderr)?;
    Ok((
        Worker {
            root: dir,
            worker,
            stdout,
            stderr,
            transcript,
        },
        child,
    ))
}

impl Worker {
    fn logs(&self) -> String {
        format!(
            "{}\n{}\n{}",
            std::fs::read_to_string(&self.stdout).unwrap_or_default(),
            std::fs::read_to_string(&self.stderr).unwrap_or_default(),
            std::fs::read_to_string(&self.transcript).unwrap_or_default()
        )
    }
    fn saw(&self, event: &str, field: &str, value: &str) -> bool {
        events(&self.transcript)
            .iter()
            .any(|row| row["event"] == event && row["detail"][field] == value)
    }
    /// How many times the batch has been started on the worker.
    fn started(&self) -> usize {
        events(&self.transcript)
            .iter()
            .filter(|row| {
                row["event"] == "exec"
                    && row["detail"]["base64"]
                        .as_str()
                        .and_then(|text| base64::engine::general_purpose::STANDARD.decode(text).ok())
                        .is_some_and(|bytes| String::from_utf8_lossy(&bytes).contains("test-without-building"))
            })
            .count()
    }
    fn reports_written(&self) -> bool {
        self.root.path().join("report/html/index.html").exists()
    }
}

/// D52: the reports are out before the process that won't stop has taken any of the time, D30 gets through TERM and KILL
/// within the limit, and the exit status is the signal's, in both debug modes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_writes_reports_first_and_stops_a_slow_worker_process_d52() -> Result {
    for debug in [false, true] {
        let (worker, mut child) = slow_to_stop(debug).await?;
        until("xcodebuild to start", || worker.started() > 0)
            .await
            .map_err(|e| format!("debug={debug}: {e}\n{}", worker.logs()))?;
        signal(&child, Signal::SIGTERM)?;
        until("the reports", || worker.reports_written())
            .await
            .map_err(|e| format!("debug={debug}: {e}\n{}", worker.logs()))?;
        assert!(
            !worker.saw("signal", "signal", "KILL"),
            "debug={debug}: reports should come first"
        );
        let status = exited(&mut child).await?;
        assert_eq!(status.code(), Some(143), "debug={debug}\n{}", worker.logs());
        // D30 got to the end within the limit: the process ignored TERM and was sent KILL.
        assert!(
            worker.saw("signal_ignored", "signal", "TERM"),
            "debug={debug}\n{}",
            worker.logs()
        );
        assert!(
            worker.saw("signal", "signal", "KILL"),
            "debug={debug}\n{}",
            worker.logs()
        );
        // U161: the KILL ended the batch with the provider closing, and nothing started it again.
        assert_eq!(worker.started(), 1, "debug={debug}\n{}", worker.logs());
        worker.worker.shutdown().await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// D52: a second signal doesn't sit out D30's 20 s, and has the status of its own number. It comes while the worker is
/// still ignoring TERM, and the exit comes before any KILL.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_signal_leaves_the_slow_worker_process_behind_d52() -> Result {
    let (worker, mut child) = slow_to_stop(false).await?;
    until("xcodebuild to start", || worker.started() > 0)
        .await
        .map_err(|e| format!("{e}\n{}", worker.logs()))?;
    signal(&child, Signal::SIGTERM)?;
    until("TERM to reach the worker", || {
        worker.saw("signal_ignored", "signal", "TERM")
    })
    .await
    .map_err(|e| format!("{e}\n{}", worker.logs()))?;
    signal(&child, Signal::SIGINT)?;
    let status = exited(&mut child).await?;
    assert_eq!(status.code(), Some(130), "{}", worker.logs());
    assert!(!worker.saw("signal", "signal", "KILL"), "{}", worker.logs());
    worker.worker.shutdown().await.map_err(|e| e.to_string())?;
    Ok(())
}

/// An InfluxDB v1 that answers the setup at once and holds every write until released: a metrics backend slow to take the
/// last points, which keeps the reports step, and with it the cleanup, going.
struct SlowInflux {
    port: u16,
    writes: Arc<AtomicUsize>,
    release: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}
impl SlowInflux {
    async fn start() -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let writes = Arc::new(AtomicUsize::new(0));
        let (release, released) = tokio::sync::watch::channel(false);
        let counted = writes.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(influx_connection(stream, counted.clone(), released.clone()));
            }
        });
        Ok(Self {
            port,
            writes,
            release,
            task,
        })
    }
    fn writes(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }
}
impl Drop for SlowInflux {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn influx_connection(
    stream: tokio::net::TcpStream,
    writes: Arc<AtomicUsize>,
    mut released: tokio::sync::watch::Receiver<bool>,
) -> std::io::Result<()> {
    let mut stream = tokio::io::BufReader::new(stream);
    loop {
        let mut request = String::new();
        if stream.read_line(&mut request).await? == 0 {
            return Ok(());
        }
        let mut length = 0;
        loop {
            let mut header = String::new();
            stream.read_line(&mut header).await?;
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0; length];
        stream.read_exact(&mut body).await?;
        let path = request.split_whitespace().nth(1).unwrap_or_default();
        let (status, reply) = if path.starts_with("/write") {
            writes.fetch_add(1, Ordering::SeqCst);
            let _ = released.wait_for(|released| *released).await;
            ("204 No Content", "")
        } else if String::from_utf8_lossy(&body).contains("SHOW+DATABASES") {
            (
                "200 OK",
                r#"{"results":[{"statement_id":0,"series":[{"name":"databases","columns":["name"],"values":[["db"]]}]}]}"#,
            )
        } else {
            ("200 OK", r#"{"results":[{"statement_id":0}]}"#)
        };
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
            reply.len()
        );
        stream.get_mut().write_all(reply.as_bytes()).await?;
    }
}

/// U161 (S-x35-2): the test in flight ends during the cleanup, on a device that is still up, while a metrics backend slow
/// to take the last points holds the reports step. With the actors left running, the queue starts it again at once, and
/// again on every end, until the process exits. With them stopped, it started once. Reverting the actors' cancellation
/// fails this test and no other. The test can't end before the signal (S-x35-5): its output waits on a gate, opened once
/// the cancel's line is out, which is logged just before the actors are stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_test_that_ends_during_the_cleanup_is_not_started_again_u161() -> Result {
    let influx = SlowInflux::start().await?;
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    let mut server =
        FakeAdbServer::start(adb_script(HangingEnds::AtGate(CLEANUP_GATE))?, ServerOptions::default()).await?;
    let analytics = format!(
        "analyticsConfiguration:\n  type: influxdb\n  url: http://127.0.0.1:{}\n  user: ''\n  password: ''\n  dbName: db\n",
        influx.port
    );
    android_run(root, false, server.port(), &analytics)?;
    let (stdout, stderr) = (root.join("stdout"), root.join("stderr"));
    let mut child = marathon(root, &stdout, &stderr)?;
    let logs = || {
        format!(
            "{}\n{}",
            std::fs::read_to_string(&stdout).unwrap_or_default(),
            std::fs::read_to_string(&stderr).unwrap_or_default()
        )
    };
    until("the hanging test to start", || instruments(&server, HANGING) > 0)
        .await
        .map_err(|e| format!("{e}\n{}", logs()))?;
    signal(&child, Signal::SIGTERM)?;
    until("the cancel's line", || {
        std::fs::read_to_string(&stdout).is_ok_and(|out| out.contains("stopping the tests, writing the reports"))
    })
    .await
    .map_err(|e| format!("{e}\n{}", logs()))?;
    until("a write to InfluxDB", || influx.writes() > 0)
        .await
        .map_err(|e| format!("{e}\n{}", logs()))?;
    assert_eq!(
        instruments(&server, HANGING),
        1,
        "started again before it ended\n{}",
        logs()
    );
    server.open_gate(CLEANUP_GATE);
    // The window: time for a live queue to start the test again. A slow host only makes it narrower.
    let window = Instant::now() + Duration::from_millis(BOUNCE_WINDOW_MILLIS);
    while Instant::now() < window && instruments(&server, HANGING) == 1 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let bounced = instruments(&server, HANGING);
    influx.release.send_replace(true);
    let status = exited(&mut child).await?;
    assert_eq!(status.code(), Some(143), "{}", logs());
    assert_eq!(bounced, 1, "the test was started again during the cleanup\n{}", logs());
    assert_eq!(instruments(&server, HANGING), 1, "{}", logs());
    let raw = std::fs::read_to_string(root.join("output/test_result/raw.json"))?;
    let rows: Vec<Value> = serde_json::from_str(&raw)?;
    assert_eq!(rows.len(), 1, "only the finished test is in the reports\n{raw}");
    server.shutdown().await?;
    Ok(())
}
