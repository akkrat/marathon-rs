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
    worker_with(root, name, Default::default()).await
}
async fn worker_with(
    root: &Path,
    name: &str,
    script: fake_worker::script::Script,
) -> Result<fake_worker::server::RunningWorker> {
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
            script,
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
    files(
        root,
        ports,
        "deviceInitializationTimeoutMillis: 1200000\n",
        "    keepAliveInterval: PT15S\n",
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
/// The devices file for one simulator on each port, and a Marathonfile with the given top-level and `ssh` lines.
fn files(root: &Path, ports: &[u16], top: &str, ssh: &str) -> Result {
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
            "name: stall\noutputDir: report\nanalyticsTracking: false\nbugsnagReporting: false\nuncompletedTestRetryQuota: 0\nretryStrategy: {{type: no-retry}}\n{top}vendorConfiguration:\n  type: iOS\n  bundle:\n    application: App.app\n    testApplication: Tests.xctest\n    testType: xctest\n  ssh:\n    authentication: {{type: publicKey, username: fixture, key: '{}'}}\n{ssh}  lifecycle:\n    shutdownUnused: false\n    onPrepare: []\n    onDispose: [SHUTDOWN]\n  xcresult:\n    pullingPolicy: NEVER\n  screenRecordConfiguration:\n    videoConfiguration:\n      enabled: false\n      transcoding: {{enabled: false}}\n    screenshotConfiguration: {{enabled: false}}\n",
            root.join("key").display()
        ),
    )?;
    Ok(())
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
    // Core's three borrows, and not three more on each. With one device D53 changes nothing, so this is also A103's
    // witness: Kotlin's three attempts, all on the stuck worker.
    assert_eq!(output.status.code(), Some(1), "{text}\n{errors}");
    assert_eq!(sends(root), 3, "{text}\n{errors}");
    stuck.shutdown().await.map_err(|e| e.to_string())?;
    Ok(())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "each stall takes 295 s on the real clock (D31); run by hand with --ignored"]
async fn a_stalled_send_while_parsing_leaves_the_parse_to_the_healthy_worker_d53() -> Result {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    fixture(root)?;
    let (stuck, healthy) = (worker(root, "stuck").await?, worker(root, "healthy").await?);
    let stuck_port = stuck.ready.endpoint.port();
    let healthy_port = healthy.ready.endpoint.port();
    let mut command = run(root, &[stuck_port, healthy_port], stuck_port, STALL, true, None)?;
    let output = tokio::time::timeout(STALL_CATCH, command.output()).await??;
    let text = String::from_utf8_lossy(&output.stdout);
    let errors = String::from_utf8_lossy(&output.stderr);
    // The first borrow is random. If it takes the stuck worker, its stall is one send and the second attempt takes the
    // healthy one; the stuck simulator's own prepare may send once more. Kotlin's random retry fails the run one time in
    // eight, and sends to the stuck worker three or more times one time in four.
    assert_eq!(output.status.code(), Some(0), "{text}\n{errors}");
    assert!(text.contains("test1"), "{text}\n{errors}");
    assert!(sends(root) <= 2, "{} sends\n{text}\n{errors}", sends(root));
    stuck.shutdown().await.map_err(|e| e.to_string())?;
    healthy.shutdown().await.map_err(|e| e.to_string())?;
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
/// The environment variable that turns `frozen_worker_process` into a worker for `a_worker_frozen_while_parsing_*`: the root
/// the worker lives in. A worker frozen with SIGSTOP has to be a process of its own, and this one is the test binary itself.
const FROZEN_ROOT: &str = "MARATHON_X45_FROZEN_ROOT";
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "a worker process for a_worker_frozen_while_parsing_*; does nothing unless that test starts it"]
async fn frozen_worker_process() -> Result {
    let Some(root) = std::env::var_os(FROZEN_ROOT) else {
        return Ok(());
    };
    let root = Path::new(&root);
    let worker = worker(root, "frozen").await?;
    let port = root.join("frozen-port");
    std::fs::write(root.join("frozen-port.tmp"), worker.ready.endpoint.port().to_string())?;
    std::fs::rename(root.join("frozen-port.tmp"), &port)?;
    // Until the test kills it.
    std::future::pending::<()>().await;
    Ok(())
}
/// Seconds since the Unix epoch with milliseconds, as the stand-in rsync below writes them.
fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}
/// What one run with a frozen worker showed.
struct Frozen {
    exit: Option<i32>,
    seconds: f64,
    /// When each rsync the run started began and ended, in seconds after the start, with the worker (`frozen`, `healthy`, or
    /// `probe` for the version check), whether it sent the test bundle, and its status. One that the bridge stopped with TERM
    /// says `stopped` and has no end.
    sends: Vec<String>,
    frozen_at: Option<f64>,
    stdout: String,
    stderr: String,
}
/// Two workers, the first a process of its own that is frozen with SIGSTOP as the first rsync to it starts, as x26 did, so
/// that nothing answers while its kernel still accepts connections. The healthy worker answers the first session of each
/// connection 5 s late, so that its simulator is set up after the frozen one and the parse's first borrow, which picks at
/// random among the simulators set up by then, takes the frozen one. Times are read from the wall clock, which goes on while
/// a Mac sleeps, so run it under `caffeinate -i`.
async fn frozen_run(keep_alive: &str) -> Result<Frozen> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    fixture(root)?;
    let mut child = tokio::process::Command::new(std::env::current_exe()?)
        .args([
            "frozen_worker_process",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(FROZEN_ROOT, root)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let pid = child.id().ok_or("the worker process has no pid")?;
    let frozen_port: u16 = tokio::time::timeout(CATCH, async {
        loop {
            if let Ok(text) = std::fs::read_to_string(root.join("frozen-port")) {
                return text.trim().parse::<u16>();
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await??;
    let healthy = worker_with(
        root,
        "healthy",
        fake_worker::script::Script {
            faults: fake_worker::script::Faults {
                late_open: Some(fake_worker::script::LateOpen {
                    delay_millis: 5_000,
                    reject: false,
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await?;
    let healthy_port = healthy.ready.endpoint.port();
    files(root, &[frozen_port, healthy_port], "", keep_alive)?;
    let log = root.join("sends");
    let mark = root.join("frozen");
    let rsync = root.join("rsync");
    std::fs::write(
        &rsync,
        format!(
            "#!/bin/bash\n\
             at() {{ /usr/bin/perl -MTime::HiRes=time -e 'printf \"%.3f\", time'; }}\n\
             case \"$*\" in *\" -p {frozen_port} \"*) who=frozen;; *\" -p \"*) who=healthy;; *) who=probe;; esac\n\
             case \"$*\" in *.xctest*) what=bundle;; *) what=other;; esac\n\
             trap 'echo \"$(at) stopped $who $what\" >> \"{log}\"; exit 143' TERM\n\
             if [ $who = frozen ] && [ ! -e \"{mark}\" ]; then at > \"{mark}\"; kill -STOP {pid}; fi\n\
             echo \"$(at) start $who $what\" >> \"{log}\"\n\
             /usr/bin/rsync \"$@\" &\n\
             wait $!\n\
             status=$?\n\
             echo \"$(at) end $who $what $status\" >> \"{log}\"\n\
             exit $status\n",
            mark = mark.display(),
            log = log.display(),
        ),
    )?;
    std::fs::set_permissions(&rsync, std::fs::Permissions::from_mode(0o700))?;
    let started = now();
    let output = tokio::time::timeout(
        STALL_CATCH,
        tokio::process::Command::new(env!("CARGO_BIN_EXE_marathon"))
            .current_dir(root)
            .env("RSYNC4J_RSYNC", &rsync)
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    let seconds = now() - started;
    // SIGKILL ends a stopped process too.
    child.kill().await?;
    healthy.shutdown().await.map_err(|e| e.to_string())?;
    let at = |text: &str| text.parse::<f64>().map_or(f64::NAN, |t| t - started);
    let sends = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|line| match line.split_once(' ') {
            Some((time, event)) => format!("{:.1} s {event}", at(time)),
            None => line.to_owned(),
        })
        .collect();
    Ok(Frozen {
        exit: output.status.code(),
        seconds,
        sends,
        frozen_at: std::fs::read_to_string(&mark).ok().map(|text| at(text.trim())),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}
impl std::fmt::Display for Frozen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "exit {:?} after {:.1} s, frozen at {:?} s",
            self.exit, self.seconds, self.frozen_at
        )?;
        for send in &self.sends {
            writeln!(f, "send: {send}")?;
        }
        write!(f, "--- stdout\n{}\n--- stderr\n{}", self.stdout, self.stderr)
    }
}
/// When the run's first `event` (`start`, `stopped` or `end`) for the bundle to `who` came, in seconds after the start.
fn bundle_at(run: &Frozen, event: &str, who: &str) -> Option<f64> {
    let suffix = format!(" {event} {who} bundle");
    run.sends
        .iter()
        .find(|send| send.ends_with(&suffix) || send.contains(&format!("{suffix} ")))
        .and_then(|send| send.split_once(" s ").and_then(|(at, _)| at.parse().ok()))
}
/// Either way, the parse's first borrow takes the frozen worker, and its bundle's send is what the freeze catches.
fn assert_frozen_first(run: &Frozen) {
    let first = run.sends.iter().find(|send| !send.contains(" probe "));
    assert!(first.is_some_and(|send| send.ends_with("start frozen bundle")), "{run}");
    // D31's limit plays no part.
    assert!(!run.stdout.contains("no progress in 300 s"), "{run}");
}
/// D54 with the default keep-alive: the frozen simulator's connection is lost at about 75 s, which stops the parse's send
/// to it and fails that attempt, and D53's next attempt parses on the healthy worker. Under x45 the send went on until D31
/// stopped it at 305 s, and the run failed at about 311 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "takes minutes on the real clock; run by hand with --ignored --nocapture"]
async fn a_worker_frozen_while_parsing_is_left_once_keep_alive_loses_it_d54() -> Result {
    let run = frozen_run("").await?;
    println!("{run}");
    assert_frozen_first(&run);
    assert!(
        run.stdout
            .contains("Did not receive any keep-alive response for 60 seconds"),
        "{run}"
    );
    assert!(
        run.stdout
            .contains("send error: the device's connection was lost while sending to 127.0.0.1:"),
        "{run}"
    );
    // Keep-alive can't give up before 60 s; D31 would stop the send at about 305 s.
    let stopped = bundle_at(&run, "stopped", "frozen").ok_or_else(|| format!("{run}"))?;
    assert!((60.0..240.0).contains(&stopped), "{run}");
    let second = bundle_at(&run, "start", "healthy").ok_or_else(|| format!("{run}"))?;
    assert!((stopped..240.0).contains(&second), "{run}");
    assert_eq!(run.exit, Some(0), "{run}");
    assert!(run.stdout.contains("test1"), "{run}");
    assert!(
        !run.stdout
            .contains("Timed out waiting for a temporary device for remote test parsing"),
        "{run}"
    );
    Ok(())
}
/// D54 with keep-alive off: nothing notices the frozen worker, and `deviceInitializationTimeoutMillis` (180 s) now ends the
/// parse's send at its limit, so the run fails with the timeout's error at about 180 s. Under x45 it failed at about 311 s
/// and exited at about 346 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "takes minutes on the real clock; run by hand with --ignored --nocapture"]
async fn a_worker_frozen_while_parsing_with_keep_alive_off_fails_at_the_limit_d54() -> Result {
    let run = frozen_run("    keepAliveInterval: PT0S\n").await?;
    println!("{run}");
    assert_frozen_first(&run);
    assert!(!run.stdout.contains("keep-alive response"), "{run}");
    assert!(!run.stdout.contains("connection was lost"), "{run}");
    // Not before the limit; D31 would stop the send at about 305 s.
    let stopped = bundle_at(&run, "stopped", "frozen").ok_or_else(|| format!("{run}"))?;
    assert!((179.0..270.0).contains(&stopped), "{run}");
    assert_eq!(run.exit, Some(1), "{run}");
    assert!(
        run.stdout
            .contains("Timed out waiting for a temporary device for remote test parsing"),
        "{run}"
    );
    // A cancellation marks no device as tried, and `withRetry` ends on it, so the healthy worker gets no attempt.
    assert_eq!(bundle_at(&run, "start", "healthy"), None, "{run}");
    Ok(())
}
