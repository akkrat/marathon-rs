//! Port of cli/src/main/kotlin/com/malinskiy/marathon/cli/ApplicationView.kt (signal windows).
//!
//! Real SIGINT/SIGTERM delivered with kill(2) to a child process (this test binary re-run in
//! child mode with the Stub vendor injected). While the vendor is constructing, the default
//! disposition ends the process, so a shell sees 128 + the signal number; while the run is
//! suspended, the P3 relay exits with that status itself in both debug modes. A signal whose
//! registration fails keeps its default disposition; the other one is still relayed. A signal
//! the process started with ignored stays ignored, as on HotSpot.
#![cfg(unix)]
use jcompat::logging::OutputTarget;
use marathon_cli::{
    VendorFactories,
    application::{CliOutput, CliRunner},
    runner::{ProductionRunner, missing_vendors},
};
use marathon_core::{
    executor::SharedChannel,
    marathon::{SignalHooks, signal_ignored, tokio_registration},
    model::{ExecutionCommand, Test},
    runtime::VendorFuture,
    vendor::{LocalTestParser, TestParser, VendorContext, VendorServices},
};
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
    os::unix::process::ExitStatusExt,
    process::{Child, Command, ExitStatus, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

const MODE: &str = "MARATHON_SIGNAL_CHILD";

/// Announces the run window, then never finishes.
struct Suspended;
impl LocalTestParser for Suspended {
    fn extract(&self, _: VendorContext) -> VendorFuture<'_, Vec<Test>> {
        Box::pin(async {
            let _ = writeln!(std::io::stderr(), "MARKER run");
            std::future::pending().await
        })
    }
}

fn factories(construction: bool) -> VendorFactories {
    let mut factories = missing_vendors();
    factories.stub = Box::new(move |_| {
        Box::pin(async move {
            if construction {
                let _ = writeln!(std::io::stderr(), "MARKER construction");
                std::future::pending::<()>().await;
            }
            Ok(VendorServices {
                provider: StubDeviceProvider::new(SharedChannel::new()),
                parser: Some(TestParser::Local(Arc::new(Suspended))),
                bundle_identifier: Some(Arc::new(MockTestBundleIdentifier)),
                log_configurator: Some(Arc::new(MockLogConfigurator)),
            })
        })
    });
    factories
}

/// Announces the registrations; `run-fail-<signal>` makes that signal's registration fail.
fn hooks(mode: &str) -> SignalHooks {
    let failing = match mode {
        "run-fail-int" => Some(2),
        "run-fail-term" => Some(15),
        _ => None,
    };
    SignalHooks {
        registration: Some(Arc::new(move |number| {
            if Some(number) == failing {
                return Err(std::io::Error::other("injected registration failure"));
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
    let cli = CliOutput::new(
        Box::new(std::io::sink()),
        Box::new(std::io::sink()),
        OutputTarget::Pipe,
        Box::new(|| 0),
    );
    let runner = ProductionRunner {
        factories: Arc::new(factories(mode == "construction")),
        cwd: std::env::current_dir().unwrap(),
        environment: vec![],
        signal_hooks: hooks(&mode),
    };
    let marathon = runner
        .construct(Arc::new(factory.configuration.clone()), cli.logging(), cli.console())
        .unwrap_or_else(|e| panic!("{e:?}"));
    let _ = marathon.run(ExecutionCommand::Parse {
        output_file_name: None,
        include_flaky_tests: false,
    });
    panic!("the run finished");
}

/// Starts a child in `mode` and waits until it reaches the window under test. `ignored`
/// starts it with that signal ignored (through `sh`, as a shell background job would).
fn started(mode: &str, ignored: Option<&str>) -> Child {
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
    let mut child = command
        .env(MODE, mode)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Construction is announced by the vendor; the run window by the parser and, as both
    // registrations have been attempted, by the relay.
    let mut awaited: Vec<&str> = if mode == "construction" {
        vec!["MARKER construction"]
    } else {
        vec!["MARKER run", "MARKER ready"]
    };
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    while !awaited.is_empty() {
        match lines.next() {
            Some(Ok(line)) => awaited.retain(|m| *m != line),
            _ => panic!("{mode}: the child exited before {awaited:?}: {:?}", child.wait()),
        }
    }
    child
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

fn interrupted(mode: &str, signal: Signal) -> ExitStatus {
    let mut child = started(mode, None);
    // A process that survives the signal fails the test instead of hanging it.
    signalled(&mut child, signal, Duration::from_secs(10)).unwrap_or_else(|| {
        let _ = child.kill();
        panic!("{mode}: the child survived {signal}")
    })
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
        let status = signalled(&mut child, other, Duration::from_secs(10)).unwrap_or_else(|| {
            let _ = child.kill();
            panic!("{ignored}: the child survived {other}")
        });
        assert_eq!(status.code(), Some(code), "{ignored} then {other}: {status:?}");
    }
}
