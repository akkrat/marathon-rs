//! WP 2.5: `ApplicationView.execute` around the run seam (`ApplicationView.kt:79-102`), in process with stub runners:
//! every row of the run-outcome table, construction failure, the Stub/Empty gate, what `parse` hands the runner, and
//! the throwable rendering (`fixtures/oracle/logging/details.jsonl` `op: throwable`, D12).

use jcompat::{
    jtime::instant_of,
    logging::{Level, LogEvent, MarathonLogging, OutputTarget, Vendor},
    writers::JavaString,
};
use marathon_cli::{
    application::{
        CliOutput, CliRunner, Console, MarathonRun, Process, UnsupportedRunner, configuration_failure,
        render_throwable, render_uncaught,
    },
    run_cli,
};
use marathon_config::{
    Configuration,
    binding::{BindingContext, Profile},
    loader::MarathonConfigurationLoader,
};
use marathon_core::{error::Failure, model::ExecutionCommand};
use serde_json::Value as Json;
use std::{
    fs,
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
};

const ANDROID: &str = "name: x\noutputDir: out\nvendorConfiguration:\n  type: Android\n  applicationApk: app.apk\n";
const ANDROID_IGNORE_FAILURES: &str =
    "name: x\noutputDir: out\nignoreFailures: true\nvendorConfiguration:\n  type: Android\n  applicationApk: app.apk\n";
const STUB: &str = "name: x\noutputDir: out\nvendorConfiguration:\n  type: stub\n";
const EMPTY: &str = "name: x\noutputDir: out\nvendorConfiguration:\n  type: empty\n";

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Buffer {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

fn failure(class: &str, message: &str, cause: Option<Failure>) -> Failure {
    Failure {
        class: class.into(),
        message: Some(message.into()),
        cause: cause.map(Box::new),
        stacktrace: None,
    }
}

/// What the stub's `run` does.
#[derive(Clone)]
enum Outcome {
    Succeed(bool),
    Crash(Failure),
    /// Switch the registry to the Android runtime layout (`configureLogging()`), then crash.
    ConfigureThenCrash(Failure),
    /// Configure, log one runtime event through the console `construct` received (a 3.10 appender), then crash.
    LogThenCrash(Failure),
}

/// Records what `execute` hands to construction and run.
struct StubRunner {
    construct: Result<Outcome, Failure>,
    seen: Arc<Mutex<Vec<String>>>,
}

struct StubRun {
    outcome: Outcome,
    logging: Arc<Mutex<MarathonLogging>>,
    console: Arc<Console>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl CliRunner for StubRunner {
    fn construct(
        &self,
        configuration: Arc<Configuration>,
        logging: Arc<Mutex<MarathonLogging>>,
        console: Arc<Console>,
    ) -> Result<Box<dyn MarathonRun>, Failure> {
        self.seen.lock().unwrap().push(format!(
            "construct analyticsTracking={} bugsnagReporting={}",
            configuration.analytics_tracking, configuration.bugsnag_reporting
        ));
        let outcome = self.construct.clone()?;
        Ok(Box::new(StubRun {
            outcome,
            logging,
            console,
            seen: Arc::clone(&self.seen),
        }))
    }
}

impl MarathonRun for StubRun {
    fn run(self: Box<Self>, command: ExecutionCommand) -> Result<bool, Failure> {
        self.seen.lock().unwrap().push(format!("run {command:?}"));
        match self.outcome {
            Outcome::Succeed(success) => Ok(success),
            Outcome::Crash(e) => Err(e),
            Outcome::ConfigureThenCrash(e) => {
                self.logging.lock().unwrap().configure(false, Vendor::Android);
                Err(e)
            }
            Outcome::LogThenCrash(e) => {
                let (layout, logger) = {
                    let mut logging = self.logging.lock().unwrap();
                    logging.configure(false, Vendor::Android);
                    (
                        logging.layout(),
                        logging.logger("com.malinskiy.marathon.Marathon", None),
                    )
                };
                let event = LogEvent {
                    level: Level::Info,
                    logger: logger.name(),
                    thread: "main",
                    local_time_millis: 45_296_789,
                    message: "from the run",
                    throwable: "",
                };
                self.console.write_event(layout, &event);
                Err(e)
            }
        }
    }
}

struct Run {
    stdout: String,
    stderr: String,
    exit: i32,
    seen: Vec<String>,
}

/// `marathon <args>` in a temp dir holding `Marathonfile` with `content`, ANDROID_HOME set, clock at 12:34:56.789.
fn marathon(args: &[&str], content: &str, construct: Result<Outcome, Failure>) -> Run {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("Marathonfile"), content).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let runner = StubRunner {
        construct,
        seen: Arc::clone(&seen),
    };
    let (stdout, stderr) = (Buffer::default(), Buffer::default());
    let run = invoke(args, tmp.path(), &runner, &stdout, &stderr);
    let seen = seen.lock().unwrap().clone();
    Run {
        stdout: stdout.text().replace(&*tmp.path().to_string_lossy(), "<DIR>"),
        stderr: stderr.text().replace(&*tmp.path().to_string_lossy(), "<DIR>"),
        exit: run,
        seen,
    }
}

fn invoke(args: &[&str], cwd: &Path, runner: &dyn CliRunner, stdout: &Buffer, stderr: &Buffer) -> i32 {
    let process = Process {
        cwd: cwd.to_path_buf(),
        environment: vec![(JavaString::from("ANDROID_HOME"), JavaString::from("/android-sdk"))],
        now: instant_of(1_000_000, 0).unwrap(),
    };
    let output = CliOutput::new(
        Box::new(stdout.clone()),
        Box::new(stderr.clone()),
        OutputTarget::Pipe,
        Box::new(|| 45_296_789),
    );
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    run_cli(&args, &process, runner, &output)
}

const START: &str = "12:34:56.789 [main] INFO  c.m.marathon.cli.ApplicationView - Starting marathon v0.10.1, CliConfiguration: MarathonRunCommandCliConfiguration(marathonfile=Marathonfile, analyticsTracking=true, bugsnagReporting=true)\n\
12:34:56.789 [main] INFO  c.m.marathon.cli.ApplicationView - Checking Marathonfile config\n\
12:34:56.789 [main] INFO  c.m.marathon.cli.ApplicationView - Crash reporting and usage analytics are not supported by this build; --bugsnag, --analyticsTracking, bugsnagReporting and analyticsTracking are ignored\n";

fn start() -> String {
    START.replace("v0.10.1", &format!("v{}", marathon_cli::application::VERSION))
}

#[test]
fn run_success_prints_finished_on_stdout() {
    let r = marathon(&["run"], ANDROID, Ok(Outcome::Succeed(true)));
    assert_eq!(r.stdout, format!("{}Marathon execution finished\n", start()));
    assert_eq!((r.stderr.as_str(), r.exit), ("", 0));
    assert_eq!(
        r.seen,
        ["construct analyticsTracking=true bugsnagReporting=true", "run Run"]
    );
}

#[test]
fn run_failures_with_ignore_failures_exit_0_on_stderr() {
    let r = marathon(&[], ANDROID_IGNORE_FAILURES, Ok(Outcome::Succeed(false)));
    assert_eq!(r.stdout, start());
    assert_eq!(
        r.stderr,
        "Marathon execution finished with failures (Failures suppressed because ignoreFailures is `true`)\n"
    );
    assert_eq!(r.exit, 0);
}

#[test]
fn run_failures_exit_1() {
    let r = marathon(&["run"], ANDROID, Ok(Outcome::Succeed(false)));
    assert_eq!(r.stdout, start());
    assert_eq!((r.stderr.as_str(), r.exit), ("Marathon execution failed\n", 1));
}

#[test]
fn run_crash_logs_kotlin_unit_and_the_throwable() {
    let e = failure(
        "java.lang.IllegalStateException",
        "boom",
        Some(failure("java.io.IOException", "inner", None)),
    );
    let r = marathon(&["run"], ANDROID, Ok(Outcome::Crash(e)));
    assert_eq!(
        r.stdout,
        format!(
            "{}12:34:56.789 [main] ERROR c.m.marathon.cli.ApplicationView - kotlin.Unit\n\
             java.lang.IllegalStateException: boom\nCaused by: java.io.IOException: inner\n",
            start()
        )
    );
    assert_eq!((r.stderr.as_str(), r.exit), ("Marathon execution crashed\n", 1));
}

#[test]
fn run_crash_after_configure_logging_uses_the_runtime_layout() {
    let e = failure("java.lang.RuntimeException", "late", None);
    let r = marathon(&["run"], ANDROID, Ok(Outcome::ConfigureThenCrash(e)));
    // Off a terminal Jansi strips the highlight; the runtime layout puts the level letter first.
    assert_eq!(
        r.stdout,
        format!(
            "{}E 12:34:56.789 [main] <c.malinskiy.marathon.cli.ApplicationView> kotlin.Unit\njava.lang.RuntimeException: late\n",
            start()
        )
    );
    assert_eq!((r.stderr.as_str(), r.exit), ("Marathon execution crashed\n", 1));
}

#[test]
fn run_events_and_the_crash_log_share_the_console() {
    let e = failure("java.lang.RuntimeException", "late", None);
    let r = marathon(&["run"], ANDROID, Ok(Outcome::LogThenCrash(e)));
    assert_eq!(
        r.stdout,
        format!(
            "{}I 12:34:56.789 [main] <com.malinskiy.marathon.Marathon> from the run\n\
             E 12:34:56.789 [main] <c.malinskiy.marathon.cli.ApplicationView> kotlin.Unit\njava.lang.RuntimeException: late\n",
            start()
        )
    );
    assert_eq!((r.stderr.as_str(), r.exit), ("Marathon execution crashed\n", 1));
}

#[test]
fn construction_failure_escapes_execute() {
    let e = failure(
        "org.koin.core.error.InstanceCreationException",
        "Could not create instance",
        Some(failure("java.lang.IllegalArgumentException", "bad", None)),
    );
    let r = marathon(&["run"], ANDROID, Err(e));
    assert_eq!(r.stdout, start());
    assert_eq!(
        r.stderr,
        "Exception in thread \"main\" org.koin.core.error.InstanceCreationException: Could not create instance\n\
         Caused by: java.lang.IllegalArgumentException: bad\n"
    );
    assert_eq!(r.exit, 1);
    assert_eq!(r.seen.len(), 1, "run is never called");
}

#[test]
fn unsupported_runner_fails_construction_until_wp_3_10() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("Marathonfile"), ANDROID).unwrap();
    let (stdout, stderr) = (Buffer::default(), Buffer::default());
    let exit = invoke(&["run"], tmp.path(), &UnsupportedRunner, &stdout, &stderr);
    assert_eq!(stdout.text(), start());
    assert_eq!(
        stderr.text(),
        "Exception in thread \"main\" java.lang.UnsupportedOperationException: Running tests is not supported by this build yet\n"
    );
    assert_eq!(exit, 1);
}

#[test]
fn stub_vendor_is_gated_before_construction() {
    let r = marathon(&["run"], STUB, Ok(Outcome::Succeed(true)));
    assert_eq!(r.stdout, start());
    assert_eq!(
        r.stderr,
        "Exception in thread \"main\" com.malinskiy.marathon.config.exceptions.ConfigurationException: No vendor config present in <DIR>/Marathonfile\n"
    );
    assert_eq!(r.exit, 1);
    assert!(r.seen.is_empty());
}

#[test]
fn empty_vendor_fails_the_parse_before_the_gate() {
    let r = marathon(&["run"], EMPTY, Ok(Outcome::Succeed(true)));
    assert!(
        !r.stdout.contains("Crash reporting"),
        "no D1 line without a parsed configuration"
    );
    assert_eq!(
        r.stderr,
        "Exception in thread \"main\" com.malinskiy.marathon.config.exceptions.ConfigurationException: No vendor configuration specified\n"
    );
    assert_eq!(r.exit, 1);
    assert!(r.seen.is_empty());
}

#[test]
fn parse_forces_tracking_off_and_passes_the_parse_command() {
    let r = marathon(
        &["parse", "-o", "tests", "-f", "TRUE", "--marathonfile=Marathonfile"],
        ANDROID,
        Ok(Outcome::Succeed(true)),
    );
    assert_eq!(
        r.seen,
        [
            "construct analyticsTracking=false bugsnagReporting=false".to_owned(),
            format!(
                "run {:?}",
                ExecutionCommand::Parse {
                    output_file_name: Some("tests".into()),
                    include_flaky_tests: true
                }
            )
        ]
    );
    assert!(r.stdout.contains(
        "CliConfiguration: ParseCommandCliConfiguration(marathonfile=Marathonfile, outputFileName=tests, includeFlakyTests=true)\n"
    ));
    assert_eq!(r.exit, 0);
}

#[test]
fn run_flags_reach_the_configuration_merge() {
    let r = marathon(
        &["run", "--bugsnag=false", "--analyticsTracking", "TRUE"],
        ANDROID,
        Ok(Outcome::Succeed(true)),
    );
    assert_eq!(r.seen[0], "construct analyticsTracking=true bugsnagReporting=false");
}

/// The 1.6 hand-off row: the JVM's rendering of a three-level chain, frames removed (D12).
#[test]
fn throwable_rendering_matches_the_logback_row_without_frames() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/oracle/logging/details.jsonl");
    let row: Json = fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Json>(l).unwrap())
        .find(|r| r["op"] == "throwable")
        .unwrap();
    fn convert(t: &Json) -> Option<Failure> {
        (!t.is_null()).then(|| Failure {
            class: t["class"].as_str().unwrap().into(),
            message: t["message"].as_str().map(Into::into),
            cause: convert(&t["cause"]).map(Box::new),
            stacktrace: None,
        })
    }
    let expected: String = row["output"]
        .as_str()
        .unwrap()
        .split_inclusive('\n')
        .filter(|l| !l.starts_with('\t'))
        .collect();
    assert_eq!(render_throwable(&convert(&row["throwable"]).unwrap()), expected);
}

#[test]
fn external_trace_text_and_null_messages_render_as_the_jvm_does() {
    let f = Failure {
        class: "com.example.TestFailure".into(),
        message: None,
        cause: Some(Box::new(Failure {
            class: "java.lang.Error".into(),
            message: Some("x".into()),
            cause: None,
            stacktrace: Some("\tat worker.Frame(Frame.java:1)".into()),
        })),
        stacktrace: Some("\tat remote.Test(Test.kt:3)\n".into()),
    };
    assert_eq!(
        render_uncaught(&f),
        "Exception in thread \"main\" com.example.TestFailure\n\tat remote.Test(Test.kt:3)\n\
         Caused by: java.lang.Error: x\n\tat worker.Frame(Frame.java:1)\n"
    );
}

/// A binding error from the 2.4 loader: the outer `ConfigurationException` and its cause chain, outermost first.
#[test]
fn configuration_error_chain_renders_outermost_first() {
    let context = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: jcompat::file_manager::JavaPath::from_text("bad.yaml"),
        analytics_tracking: true,
        bugsnag_reporting: true,
        marathonfile_dir: jcompat::file_manager::JavaPath::from_text("/w"),
        process_dir: jcompat::file_manager::JavaPath::from_text("/w"),
        environment: Vec::new(),
        now: instant_of(1_000_000, 0).unwrap(),
    };
    let error = MarathonConfigurationLoader
        .parse_sync(b"name: x\noutputDir: out\nbogusKey: 1\n", &context)
        .unwrap_err();
    let rendered = render_uncaught(&configuration_failure(&error));
    let first_lines: Vec<&str> = rendered.lines().filter(|l| !l.starts_with(' ')).collect();
    assert_eq!(
        first_lines[0],
        "Exception in thread \"main\" com.malinskiy.marathon.config.exceptions.ConfigurationException: Error parsing config file /w/bad.yaml"
    );
    assert!(first_lines[1].starts_with("Caused by: com.fasterxml.jackson.databind.exc.UnrecognizedPropertyException"));
    assert_eq!(rendered.matches("Caused by: ").count(), error.causes.len());
}
