//! Port of cli/src/main/kotlin/com/malinskiy/marathon/cli/ApplicationView.kt (signal windows).
//!
//! Real SIGINT/SIGTERM delivered with kill(2) to a child process (this test binary re-run in
//! child mode with the Stub vendor injected). While the vendor is constructing, the default
//! disposition ends the process, so a shell sees 128 + the signal number; while the run is
//! suspended, the P3 relay exits with that status itself in both debug modes. A signal whose
//! registration fails keeps its default disposition; the other one is still relayed. A signal
//! the process started with ignored stays ignored, as on HotSpot. D52: the hook runs in both debug modes and is bounded,
//! and a second signal exits at once, however soon after the first it comes.
//!
//! Every read of the child's stderr has a deadline, and a test that fails kills and reaps its child.
#![cfg(unix)]
use marathon_cli::{
    VendorFactories,
    application::{CliOutput, CliRunner},
    runner::{ProductionRunner, missing_vendors},
};
use marathon_core::{
    executor::{SharedChannel, SharedDeferred},
    marathon::{SignalHooks, SignalStream, signal_ignored, tokio_registration},
    model::{ExecutionCommand, Test},
    runtime::{Deferred, VendorFuture},
    vendor::{LocalTestParser, TestParser, VendorContext, VendorServices},
};
use marathon_logging::OutputTarget;
use marathon_test_support::{
    factory::{ConfigurationFactory, MockLogConfigurator},
    stub_bundle::MockTestBundleIdentifier,
    stub_provider::StubDeviceProvider,
};
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use std::{
    io::{BufRead, BufReader, Write},
    ops::{Deref, DerefMut},
    os::unix::process::ExitStatusExt,
    process::{Child, ChildStderr, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const MODE: &str = "MARATHON_SIGNAL_CHILD";
/// Only catches hangs (docs/testing.md).
const PATIENCE: Duration = Duration::from_secs(60);
/// The status of a child whose run returned.
const RETURNED: i32 = 7;

/// Set once the run window opens: from then on `StuckConsole` stops taking output.
static CONSOLE_STOPS: AtomicBool = AtomicBool::new(false);

/// A stdout nobody reads, as a pipe that has filled up.
struct StuckConsole;
impl Write for StuckConsole {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if CONSOLE_STOPS.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_secs(3600));
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Announces the run window, then never finishes.
struct Suspended;
impl LocalTestParser for Suspended {
    fn extract(&self, _: VendorContext) -> VendorFuture<'_, Vec<Test>> {
        Box::pin(async {
            CONSOLE_STOPS.store(true, Ordering::SeqCst);
            let _ = writeln!(std::io::stderr(), "MARKER run");
            std::future::pending().await
        })
    }
}

/// The `slow-exit-prepare` run's parse, held until the provider terminates:
/// a run that *would* return right after the signal hook, as the production
/// binary's does, instead of suspending for ever like `Suspended`.
static PARSE_RELEASE: std::sync::OnceLock<Arc<SharedDeferred<()>>> = std::sync::OnceLock::new();
fn parse_release() -> Arc<SharedDeferred<()>> {
    PARSE_RELEASE.get_or_init(SharedDeferred::new).clone()
}
struct HeldParser;
impl LocalTestParser for HeldParser {
    fn extract(&self, _: VendorContext) -> VendorFuture<'_, Vec<Test>> {
        Box::pin(async {
            let _ = writeln!(std::io::stderr(), "MARKER run");
            let _ = Deferred::wait(parse_release().as_ref()).await;
            Ok(vec![Test {
                pkg: "com.example".into(),
                clazz: "SimpleTest".into(),
                method: "method1".into(),
                meta_properties: vec![],
            }])
        })
    }
}

/// Finds no tests, so the run ends by itself.
struct NoTests;
impl LocalTestParser for NoTests {
    fn extract(&self, _: VendorContext) -> VendorFuture<'_, Vec<Test>> {
        Box::pin(async { Ok(vec![]) })
    }
}

/// Announces the provider's termination; `run-stuck` and `run-repeat` never end it, and `blocked-thread` leaves a
/// blocking call behind, as a stuck name lookup would.
fn on_terminate(provider: &StubDeviceProvider, mode: &str) {
    let (stuck, blocked) = (matches!(mode, "run-stuck" | "run-repeat"), mode == "blocked-thread");
    let release_parse = mode == "slow-exit-prepare";
    *provider.on_terminate.lock().unwrap() = Some(Arc::new(move || {
        let _ = writeln!(std::io::stderr(), "MARKER terminate");
        if release_parse {
            Deferred::complete(parse_release().as_ref(), Ok(()));
        }
        Box::pin(async move {
            if blocked {
                tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_secs(3600)));
            }
            if stuck {
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }));
}

fn factories(mode: &str) -> VendorFactories {
    let mode = mode.to_owned();
    let mut factories = missing_vendors();
    factories.stub = Box::new(move |_| {
        let mode = mode.clone();
        Box::pin(async move {
            if mode == "construction" {
                let _ = writeln!(std::io::stderr(), "MARKER construction");
                std::future::pending::<()>().await;
            }
            let provider = StubDeviceProvider::new(SharedChannel::new());
            on_terminate(&provider, &mode);
            let parser: Arc<dyn LocalTestParser> = match mode.as_str() {
                "blocked-thread" | "after-run" => Arc::new(NoTests),
                "slow-exit-prepare" => Arc::new(HeldParser),
                _ => Arc::new(Suspended),
            };
            Ok(VendorServices {
                provider,
                parser: Some(TestParser::Local(parser)),
                bundle_identifier: Some(Arc::new(MockTestBundleIdentifier)),
                log_configurator: Some(Arc::new(MockLogConfigurator)),
            })
        })
    });
    factories
}

/// SIGTERM's stream for `run-repeat`: on its first delivery it sends the process a second SIGTERM, and hands the first
/// on only once a stream of its own has seen the second. So the second comes after the relay's stream has yielded the
/// first and before the relay could ask for the next, which is where a relay that registers again loses it.
fn repeating(number: i32) -> std::io::Result<SignalStream> {
    use futures_util::StreamExt;
    let (inner, observer) = (tokio_registration(number)?, tokio_registration(number)?);
    Ok(Box::pin(futures_util::stream::unfold(
        (inner, Some(observer)),
        |(mut inner, observer)| async move {
            inner.next().await?;
            if let Some(mut observer) = observer {
                observer.next().await?;
                kill(Pid::this(), Signal::SIGTERM).ok()?;
                observer.next().await?;
                let _ = writeln!(std::io::stderr(), "MARKER repeated");
            }
            Some(((), (inner, None)))
        },
    )))
}

/// Announces the registrations; `run-fail-<signal>` makes that signal's registration fail.
fn hooks(mode: &str) -> SignalHooks {
    let failing = match mode {
        "run-fail-int" => Some(2),
        "run-fail-term" => Some(15),
        _ => None,
    };
    let repeat = mode == "run-repeat";
    SignalHooks {
        immediate_exit: None,
        registration: Some(Arc::new(move |number| {
            if Some(number) == failing {
                return Err(std::io::Error::other("injected registration failure"));
            }
            if repeat && number == 15 {
                return repeating(number);
            }
            tokio_registration(number)
        })),
        ready: Some(Arc::new(|| {
            let _ = writeln!(std::io::stderr(), "MARKER ready");
        })),
    }
}

/// Child mode: construct (and run) until killed. A no-op in the ordinary test run.
#[test]
fn signal_child() {
    let Ok(mode) = std::env::var(MODE) else { return };
    let mut factory = ConfigurationFactory::new();
    factory.configuration.debug = mode == "run-debug";
    let stdout: Box<dyn Write + Send> = if mode.starts_with("stuck-console") {
        Box::new(StuckConsole)
    } else {
        Box::new(std::io::sink())
    };
    let stamp = Box::new(|wall: Option<i64>| marathon_cli::application::day_stamp(wall.unwrap_or(0)));
    let cli = if mode == "stuck-console-long-watchdog" {
        // Holds the first signal's watchdog open far beyond the test: only the
        // second signal's immediate exit can end this child, at any host speed.
        CliOutput::new_with_exit_flush(
            stdout,
            Box::new(std::io::sink()),
            OutputTarget::Pipe,
            stamp,
            Duration::from_secs(3600),
        )
    } else {
        CliOutput::new(stdout, Box::new(std::io::sink()), OutputTarget::Pipe, stamp)
    }
    .unwrap_or_else(|error| panic!("test console: {error}"));
    let runner = ProductionRunner {
        factories: Arc::new(factories(&mode)),
        cwd: std::env::current_dir().unwrap(),
        environment: vec![],
        signal_hooks: hooks(&mode),
        exit_prepare: (mode == "slow-exit-prepare").then(|| {
            // Preparation never finishes (it runs on the exit-output thread):
            // the process can only end through the exit watchdog.
            Arc::new(|| std::thread::sleep(Duration::from_secs(3600))) as Arc<dyn Fn() + Send + Sync>
        }),
    };
    let marathon = runner
        .construct(Arc::new(factory.configuration.clone()), cli.logging(), cli.console())
        .unwrap_or_else(|e| panic!("{e:?}"));
    let _ = marathon.run(ExecutionCommand::Parse {
        output_file_name: None,
        include_flaky_tests: false,
    });
    if mode == "after-run" {
        let _ = writeln!(std::io::stderr(), "MARKER finished");
        let mut input = String::new();
        if let Err(error) = std::io::stdin().read_line(&mut input) {
            panic!("post-run gate: {error}");
        }
        let _ = writeln!(std::io::stderr(), "MARKER alive");
        std::process::exit(RETURNED);
    }
    if mode == "blocked-thread" {
        std::process::exit(RETURNED);
    }
    panic!("the run finished");
}

/// A child that is killed and reaped when the test lets go of it, so that a failing test leaves nothing behind.
struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Deref for Running {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl DerefMut for Running {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

/// The child's stderr, read line by line on a thread, so that a wait for a line has a deadline.
struct Stderr(mpsc::Receiver<String>);
impl Stderr {
    fn new(stderr: ChildStderr) -> Self {
        let (lines, received) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    return;
                }
            }
        });
        Self(received)
    }
    /// Whether `marker` comes before the end of stderr and within `PATIENCE`.
    fn saw(&self, marker: &str) -> bool {
        self.saw_all(&[marker]).is_empty()
    }
    /// Waits for all of `markers`, in any order, until the end of stderr or `PATIENCE`; returns those not seen.
    fn saw_all<'a>(&self, markers: &[&'a str]) -> Vec<&'a str> {
        let mut awaited = markers.to_vec();
        let deadline = Instant::now() + PATIENCE;
        while !awaited.is_empty() {
            match self.0.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(line) => awaited.retain(|m| *m != line),
                Err(_) => break,
            }
        }
        awaited
    }
    /// The lines left, up to the end of stderr; `None` if it hasn't ended within `PATIENCE`.
    fn rest(&self) -> Option<Vec<String>> {
        let deadline = Instant::now() + PATIENCE;
        let mut lines = Vec::new();
        loop {
            match self.0.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(line) => lines.push(line),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Some(lines),
                Err(mpsc::RecvTimeoutError::Timeout) => return None,
            }
        }
    }
}

/// Starts a child in `mode` and waits until it reaches the window under test. `ignored`
/// starts it with that signal ignored (through `sh`, as a shell background job would).
fn started(mode: &str, ignored: Option<&str>) -> Running {
    started_with_stderr(mode, ignored).0
}
fn started_with_stderr(mode: &str, ignored: Option<&str>) -> (Running, Stderr) {
    let exe = std::env::current_exe().unwrap();
    let args = ["--exact", "signal_child", "--nocapture", "--test-threads=1"];
    let mut command = match ignored {
        Some(name) => {
            let mut command = Command::new("sh");
            command
                .arg("-c")
                .arg(format!("trap '' {name}; exec \"$0\" \"$@\""))
                .arg(exe)
                .args(args);
            command
        }
        None => {
            let mut command = Command::new(exe);
            command.args(args);
            command
        }
    };
    let mut child = Running(
        command
            .env(MODE, mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    // Construction is announced by the vendor; the run window by the parser and, as both
    // registrations have been attempted, by the relay.
    let awaited: &[&str] = if mode == "after-run" {
        &["MARKER finished"]
    } else if mode == "construction" {
        &["MARKER construction"]
    } else {
        &["MARKER run", "MARKER ready"]
    };
    let stderr = Stderr::new(child.stderr.take().unwrap());
    let missing = stderr.saw_all(awaited);
    assert!(
        missing.is_empty(),
        "{mode}: no {missing:?}, with the child {:?}",
        child.try_wait()
    );
    (child, stderr)
}

/// Sends `signal` and returns the exit status, or `None` if the child is still running
/// after `patience`.
fn signalled(child: &mut Child, signal: Signal, patience: Duration) -> Option<ExitStatus> {
    kill(Pid::from_raw(child.id() as i32), signal).unwrap();
    let deadline = Instant::now() + patience;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Waits for an exit that needs no signal; a child still running is killed, and the status is `None`.
fn signalled_wait(child: &mut Child, patience: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + patience;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn interrupted(mode: &str, signal: Signal) -> ExitStatus {
    let mut child = started(mode, None);
    // A process that survives the signal fails the test instead of hanging it.
    signalled(&mut child, signal, Duration::from_secs(10))
        .unwrap_or_else(|| panic!("{mode}: the child survived {signal}"))
}

/// A child inherits an ignored disposition from this process, and then the default-disposition
/// and relay assertions cannot hold (`sh` cannot reset a signal ignored on entry either).
fn inherited_ignored(signal: Signal) -> bool {
    let ignored = signal_ignored(signal as i32);
    if ignored {
        let _ = writeln!(std::io::stderr(), "skipped: this test process ignores {signal}");
    }
    ignored
}

#[test]
fn a_signal_during_construction_ends_the_process() {
    for signal in [Signal::SIGINT, Signal::SIGTERM] {
        if inherited_ignored(signal) {
            continue;
        }
        let status = interrupted("construction", signal);
        assert_eq!(status.signal(), Some(signal as i32), "{signal}: {status:?}");
    }
}

#[test]
fn a_signal_during_the_run_exits_with_128_plus_its_number() {
    for mode in ["run", "run-debug"] {
        for (signal, code) in [(Signal::SIGINT, 130), (Signal::SIGTERM, 143)] {
            if inherited_ignored(signal) {
                continue;
            }
            let status = interrupted(mode, signal);
            assert_eq!(status.code(), Some(code), "{mode} {signal}: {status:?}");
        }
    }
}

/// D52: `debug: false` used to exit without this, leaving worker processes behind.
#[test]
fn a_signal_terminates_the_provider_whatever_debug_says_d52() {
    for mode in ["run", "run-debug"] {
        for (signal, code) in [(Signal::SIGINT, 130), (Signal::SIGTERM, 143)] {
            if inherited_ignored(signal) {
                continue;
            }
            let (mut child, stderr) = started_with_stderr(mode, None);
            kill(Pid::from_raw(child.id() as i32), signal).unwrap();
            let terminated = stderr.saw("MARKER terminate");
            let status = signalled_wait(&mut child, PATIENCE);
            assert!(terminated, "{mode} {signal}: {status:?}");
            assert_eq!(status.and_then(|s| s.code()), Some(code), "{mode} {signal}");
        }
    }
}

/// D52: a provider that never ends can't keep the process from exiting, at the limit and with the signal's status.
#[test]
fn a_stuck_provider_holds_a_cancel_only_up_to_the_limit_d52() {
    if inherited_ignored(Signal::SIGTERM) {
        return;
    }
    let (mut child, stderr) = started_with_stderr("run-stuck", None);
    let signalled_at = Instant::now();
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    assert!(stderr.saw("MARKER terminate"));
    let status = signalled_wait(&mut child, Duration::from_secs(120));
    let waited = signalled_at.elapsed();
    assert_eq!(status.and_then(|s| s.code()), Some(143), "{status:?}");
    let limit = u64::try_from(marathon_core::marathon::CANCEL_LIMIT_MILLIS).unwrap();
    assert!(waited >= Duration::from_millis(limit), "it gave up early: {waited:?}");
}

/// D52: a console that has stopped reading holds up neither the cleanup nor the exit.
#[test]
fn a_stuck_console_does_not_hold_a_cancel_d52() {
    if inherited_ignored(Signal::SIGTERM) {
        return;
    }
    let (mut child, stderr) = started_with_stderr("stuck-console", None);
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    assert!(stderr.saw("MARKER terminate"));
    let status = signalled_wait(&mut child, PATIENCE);
    assert_eq!(status.and_then(|s| s.code()), Some(143), "{status:?}");
}

/// WP 8.8: with the console sink blocked and the first signal's exit watchdog held
/// open for an hour (test seam), a second signal exits at once with its own status,
/// without waiting for the blocked writer or the flush deadline. No outcome
/// depends on how fast the host delivers the signals: the watchdog cannot win.
#[test]
fn a_second_signal_exits_at_once_during_a_blocked_sink() {
    if inherited_ignored(Signal::SIGTERM) || inherited_ignored(Signal::SIGINT) {
        return;
    }
    let (mut child, stderr) = started_with_stderr("stuck-console-long-watchdog", None);
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    assert!(stderr.saw("MARKER terminate"));
    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).unwrap();
    let status = signalled_wait(&mut child, PATIENCE);
    assert_eq!(status.and_then(|s| s.code()), Some(130), "{status:?}");
}

/// WP 8.8 review (P1): exit preparation that never finishes must not let the run
/// return ahead of the queued exit. The child's parse is released when the
/// provider terminates (inside the hook's finish), so absent the fix the run
/// completes and returns at once — the "run finished" panic below, or exit 1 in
/// the real binary — well before the blocked exit's watchdog fires. The hook
/// now never completes after queueing the exit, so the watchdog ends the
/// process with the signal's status and the run never returns.
#[test]
fn a_slow_exit_preparation_still_exits_with_the_signal_status() {
    if inherited_ignored(Signal::SIGTERM) {
        return;
    }
    let (mut child, stderr) = started_with_stderr("slow-exit-prepare", None);
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    assert!(stderr.saw("MARKER terminate"));
    let status = signalled_wait(&mut child, PATIENCE);
    assert_eq!(status.and_then(|s| s.code()), Some(143), "{status:?}");
    assert!(
        !stderr.saw("the run finished"),
        "the run returned before the queued exit"
    );
}

/// D52: a second signal doesn't wait for the cleanup, or run it again. The status tells the two apart without a timing:
/// the first signal's limit would exit with 143.
#[test]
fn a_second_signal_exits_at_once_d52() {
    for (first, second, code) in [
        (Signal::SIGTERM, Signal::SIGINT, 130),
        (Signal::SIGINT, Signal::SIGTERM, 143),
        (Signal::SIGTERM, Signal::SIGTERM, 143),
    ] {
        if inherited_ignored(first) || inherited_ignored(second) {
            continue;
        }
        let (mut child, stderr) = started_with_stderr("run-stuck", None);
        kill(Pid::from_raw(child.id() as i32), first).unwrap();
        assert!(stderr.saw("MARKER terminate"));
        kill(Pid::from_raw(child.id() as i32), second).unwrap();
        let exited_at = Instant::now();
        let status = signalled_wait(&mut child, PATIENCE);
        assert_eq!(
            status.and_then(|s| s.code()),
            Some(code),
            "{first} then {second}: {status:?}"
        );
        if first == second {
            // The same signal has the same status at the limit, so here only the time can tell.
            let limit = u64::try_from(marathon_core::marathon::CANCEL_LIMIT_MILLIS).unwrap();
            assert!(
                exited_at.elapsed() < Duration::from_millis(limit),
                "{:?}",
                exited_at.elapsed()
            );
        }
        let rest = stderr.rest().expect("stderr ends with the child");
        assert_eq!(
            rest.iter().filter(|l| *l == "MARKER terminate").count(),
            0,
            "{first} then {second}: the provider was terminated once"
        );
    }
}

/// D52 (S-x35-1): a second SIGTERM that comes just after the first was delivered, before the relay waits for the next,
/// still exits at once. A relay that dropped the first one's stream and registered a new one missed it, and the process
/// went on to the limit. Only the time tells the two apart, since both exit with 143.
#[test]
fn a_second_signal_right_after_the_first_is_not_lost_d52() {
    if inherited_ignored(Signal::SIGTERM) {
        return;
    }
    let (mut child, stderr) = started_with_stderr("run-repeat", None);
    let signalled_at = Instant::now();
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    assert!(stderr.saw("MARKER repeated"), "the second SIGTERM was never sent");
    let status = signalled_wait(&mut child, PATIENCE);
    let waited = signalled_at.elapsed();
    assert_eq!(status.and_then(|s| s.code()), Some(143), "{status:?}");
    let limit = u64::try_from(marathon_core::marathon::CANCEL_LIMIT_MILLIS).unwrap();
    assert!(
        waited < Duration::from_millis(limit),
        "the second SIGTERM was lost: exit after {waited:?}"
    );
}

/// D52: once the run is over, a blocking call left on a vendor thread doesn't hold the process, which by then no longer
/// listens for signals. Without the limit this child would live for an hour.
#[test]
fn a_blocked_vendor_thread_does_not_hold_the_exit_after_the_run_d52() {
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(exe)
        .args(["--exact", "signal_child", "--nocapture", "--test-threads=1"])
        .env(MODE, "blocked-thread")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let status = signalled_wait(&mut child, PATIENCE);
    assert_eq!(status.and_then(|s| s.code()), Some(RETURNED), "{status:?}");
}

#[test]
fn a_signal_whose_registration_failed_keeps_its_default_disposition() {
    for (mode, failed, relayed, code) in [
        ("run-fail-int", Signal::SIGINT, Signal::SIGTERM, 143),
        ("run-fail-term", Signal::SIGTERM, Signal::SIGINT, 130),
    ] {
        if inherited_ignored(failed) || inherited_ignored(relayed) {
            continue;
        }
        let status = interrupted(mode, failed);
        assert_eq!(status.signal(), Some(failed as i32), "{mode} {failed}: {status:?}");
        let status = interrupted(mode, relayed);
        assert_eq!(status.code(), Some(code), "{mode} {relayed}: {status:?}");
    }
}

#[test]
fn a_signal_ignored_at_start_stays_ignored_during_the_run() {
    for (ignored, name, other, code) in [
        (Signal::SIGINT, "INT", Signal::SIGTERM, 143),
        (Signal::SIGTERM, "TERM", Signal::SIGINT, 130),
    ] {
        if inherited_ignored(other) {
            continue;
        }
        let mut child = started("run", Some(name));
        let status = signalled(&mut child, ignored, Duration::from_secs(1));
        assert_eq!(status, None, "{ignored}: the child should survive");
        // The other signal is still relayed.
        let status = signalled(&mut child, other, Duration::from_secs(10))
            .unwrap_or_else(|| panic!("{ignored}: the child survived {other}"));
        assert_eq!(status.code(), Some(code), "{ignored} then {other}: {status:?}");
    }
}

#[test]
fn sigquit_before_the_run_starts_does_not_end_the_process_d66() -> Result<(), Box<dyn std::error::Error>> {
    if inherited_ignored(Signal::SIGTERM) {
        return Ok(());
    }
    let mut child = started("construction", None);
    kill(Pid::from_raw(child.id() as i32), Signal::SIGQUIT)?;
    let status = signalled(&mut child, Signal::SIGTERM, PATIENCE).ok_or("SIGTERM should end construction")?;
    assert_eq!(
        status.signal(),
        Some(Signal::SIGTERM as i32),
        "SIGQUIT must survive the pre-run window: {status:?}"
    );
    Ok(())
}

#[test]
fn sigquit_after_the_run_finishes_does_not_end_the_process_d66() -> Result<(), Box<dyn std::error::Error>> {
    let (mut child, stderr) = started_with_stderr("after-run", None);
    kill(Pid::from_raw(child.id() as i32), Signal::SIGQUIT)?;
    child.stdin.take().ok_or("child stdin")?.write_all(b"continue\n")?;
    assert!(stderr.saw("MARKER alive"));
    let status = signalled_wait(&mut child, PATIENCE).ok_or("post-run child never returned")?;
    assert_eq!(status.code(), Some(RETURNED), "{status:?}");
    Ok(())
}
