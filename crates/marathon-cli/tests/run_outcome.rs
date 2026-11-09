//! WP 2.5: `ApplicationView.execute` around the run seam (`ApplicationView.kt:79-102`), in process with stub runners:
//! every row of the run-outcome table, construction failure, the Stub/Empty gate, what `parse` hands the runner, and
//! the throwable rendering (`fixtures/oracle/logging/details.jsonl` `op: throwable`, D12).

use jcompat::{
    jtime::instant_of,
    logging::{Level, LogEvent, MarathonLogging, OutputTarget, Vendor},
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
use marathon_error::{FailureKind, FatalKind};
use serde_json::Value as Json;
use std::{
    fs,
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
};

const ANDROID: &str = "name: x\noutputDir: out\nuncompletedTestRetryQuota: 5\nvendorConfiguration:\n  type: Android\n  applicationApk: app.apk\n";
const ANDROID_IGNORE_FAILURES: &str = "name: x\noutputDir: out\nignoreFailures: true\nuncompletedTestRetryQuota: 5\nvendorConfiguration:\n  type: Android\n  applicationApk: app.apk\n";
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

fn failure(class: FailureKind, message: &str, cause: Option<Failure>) -> Failure {
    Failure {
        kind: class,
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
    // What an iOS or macOS bundle needs in order to bind.
    fs::write(tmp.path().join("Marathondevices"), "workers: []\n").unwrap();
    fs::create_dir_all(tmp.path().join("Sample.app")).unwrap();
    fs::create_dir_all(tmp.path().join("Sample.xctest")).unwrap();
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
        environment: vec![(String::from("ANDROID_HOME"), String::from("/android-sdk"))],
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
        FailureKind::IllegalState,
        "boom",
        Some(failure(FailureKind::Io, "inner", None)),
    );
    let r = marathon(&["run"], ANDROID, Ok(Outcome::Crash(e)));
    assert_eq!(
        r.stdout,
        format!(
            "{}12:34:56.789 [main] ERROR c.m.marathon.cli.ApplicationView - kotlin.Unit\n\
             invalid state: boom\nCaused by: I/O error: inner\n",
            start()
        )
    );
    assert_eq!((r.stderr.as_str(), r.exit), ("Marathon execution crashed\n", 1));
}

#[test]
fn run_crash_after_configure_logging_uses_the_runtime_layout() {
    let e = failure(FailureKind::Runtime, "late", None);
    let r = marathon(&["run"], ANDROID, Ok(Outcome::ConfigureThenCrash(e)));
    // Off a terminal Jansi strips the highlight; the runtime layout puts the level letter first.
    assert_eq!(
        r.stdout,
        format!(
            "{}E 12:34:56.789 [main] <c.malinskiy.marathon.cli.ApplicationView> kotlin.Unit\nruntime failure: late\n",
            start()
        )
    );
    assert_eq!((r.stderr.as_str(), r.exit), ("Marathon execution crashed\n", 1));
}

#[test]
fn run_events_and_the_crash_log_share_the_console() {
    let e = failure(FailureKind::Runtime, "late", None);
    let r = marathon(&["run"], ANDROID, Ok(Outcome::LogThenCrash(e)));
    assert_eq!(
        r.stdout,
        format!(
            "{}I 12:34:56.789 [main] <com.malinskiy.marathon.Marathon> from the run\n\
             E 12:34:56.789 [main] <c.malinskiy.marathon.cli.ApplicationView> kotlin.Unit\nruntime failure: late\n",
            start()
        )
    );
    assert_eq!((r.stderr.as_str(), r.exit), ("Marathon execution crashed\n", 1));
}

#[test]
fn construction_failure_escapes_execute() {
    let e = failure(
        FailureKind::DependencyCreation,
        "Could not create instance",
        Some(failure(FailureKind::IllegalArgument, "bad", None)),
    );
    let r = marathon(&["run"], ANDROID, Err(e));
    assert_eq!(r.stdout, start());
    assert_eq!(
        r.stderr,
        "Exception in thread \"main\" dependency creation failure: Could not create instance\n\
         Caused by: invalid argument: bad\n"
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
        "Exception in thread \"main\" unsupported operation: Running tests is not supported by this build yet\n"
    );
    assert_eq!(exit, 1);
}

#[test]
fn stub_vendor_is_gated_before_construction() {
    let r = marathon(&["run"], STUB, Ok(Outcome::Succeed(true)));
    assert_eq!(r.stdout, start());
    assert_eq!(
        r.stderr,
        "Exception in thread \"main\" configuration rejected: No vendor config present in <DIR>/Marathonfile\n"
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
        "Exception in thread \"main\" configuration rejected: No vendor configuration specified\n"
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

// ---- D47: the table in docs/wp/x30-findings.md ------------------------------------------------------------------------

const REFUSED: &str = "uncompletedTestRetryQuota is unlimited";
const IOS_HINT: &str = "maxBatchesWithoutProgress does not replace it";

fn vendor_config(top: &str, vendor: &str, extra: &str) -> String {
    let vendor = match vendor {
        "Android" => "type: Android\n  applicationApk: app.apk\n".to_owned(),
        apple => format!(
            "type: {apple}\n  devices: Marathondevices\n  bundle:\n    application: Sample.app\n    testApplication: Sample.xctest\n    testType: xctest\n{extra}"
        ),
    };
    format!("name: x\noutputDir: out\n{top}vendorConfiguration:\n  {vendor}")
}

fn assert_refused(r: &Run, case: &str) {
    assert_eq!(r.exit, 1, "{case}: {}", r.stderr);
    assert!(r.seen.is_empty(), "{case}: nothing may start");
    assert!(
        r.stderr
            .starts_with("Exception in thread \"main\" configuration rejected: uncompletedTestRetryQuota is unlimited"),
        "{case}: {}",
        r.stderr
    );
    assert!(
        r.stderr.contains("`uncompletedTestRetryQuota: 5`"),
        "{case}: {}",
        r.stderr
    );
    assert!(
        r.stderr.contains(marathon_config::loader::FINITE_RETRIES_MIGRATION_URL),
        "{case}: {}",
        r.stderr
    );
}

fn assert_accepted(r: &Run, case: &str) {
    assert!(!r.stderr.contains(REFUSED), "{case}: {}", r.stderr);
    assert_eq!(r.exit, 0, "{case}: {}", r.stderr);
    assert_eq!(r.seen.len(), 2, "{case}: the run must start");
}

#[test]
fn an_unlimited_quota_is_refused_on_every_vendor_whatever_else_is_set() {
    for vendor in ["Android", "iOS", "macOS"] {
        for top in [
            "",
            "uncompletedTestRetryQuota: 2147483647\n",
            "uncompletedTestRetryQuota: ${UNSET_QUOTA:-2147483647}\n",
            // Neither applies to an uncompleted result.
            "retryStrategy:\n  type: fixed-quota\n  totalAllowedRetryQuota: 1\n  retryPerTestQuota: 1\n",
            "testBatchTimeoutMillis: 1000\ntestOutputTimeoutMillis: 1000\ndeviceInitializationTimeoutMillis: 1000\n",
            "executionStrategy:\n  mode: ALL_SUCCESS\n  fast: true\n",
        ] {
            let case = format!("{vendor} {top:?}");
            let r = marathon(&["run"], &vendor_config(top, vendor, ""), Ok(Outcome::Succeed(true)));
            assert_refused(&r, &case);
            assert_eq!(r.stderr.contains(IOS_HINT), vendor == "iOS", "{case}: {}", r.stderr);
        }
    }
}

#[test]
fn max_batches_without_progress_does_not_excuse_an_unlimited_quota() {
    // Its witnesses: simulator_batch::an_unclassified_error_is_not_counted_as_a_batch_without_progress and
    // simulator_recovery::a_worker_that_keeps_dropping_its_connection_is_not_charged_to_the_limit_on_losses.
    for value in [0, 1, 3] {
        let extra = format!("  maxBatchesWithoutProgress: {value}\n");
        let r = marathon(&["run"], &vendor_config("", "iOS", &extra), Ok(Outcome::Succeed(true)));
        assert_refused(&r, &format!("iOS with {value}"));
        assert!(r.stderr.contains(IOS_HINT), "{}", r.stderr);
    }
}

#[test]
fn any_other_quota_is_accepted_and_max_batches_without_progress_is_not_required() {
    for vendor in ["Android", "iOS", "macOS"] {
        // A null binds to 0, as a Jackson primitive does.
        for quota in ["0", "1", "5", "-1", "2147483646", "${UNSET_QUOTA:-5}", "null"] {
            let top = format!("uncompletedTestRetryQuota: {quota}\n");
            let r = marathon(&["run"], &vendor_config(&top, vendor, ""), Ok(Outcome::Succeed(true)));
            assert_accepted(&r, &format!("{vendor} {quota}"));
        }
    }
    let top = "uncompletedTestRetryQuota: 5\n";
    let extra = "  maxBatchesWithoutProgress: 0\n";
    let r = marathon(&["run"], &vendor_config(top, "iOS", extra), Ok(Outcome::Succeed(true)));
    assert_accepted(&r, "iOS with the setting off");
}

#[test]
fn macos_has_no_max_batches_without_progress_to_offer() {
    let top = "uncompletedTestRetryQuota: 5\n";
    let extra = "  maxBatchesWithoutProgress: 3\n";
    let r = marathon(
        &["run"],
        &vendor_config(top, "macOS", extra),
        Ok(Outcome::Succeed(true)),
    );
    assert_eq!(r.exit, 1, "{}", r.stderr);
    assert!(r.stderr.contains("Error parsing config file"), "{}", r.stderr);
    assert!(r.seen.is_empty());
}

#[test]
fn parse_is_refused_too() {
    let r = marathon(
        &["parse"],
        &vendor_config("", "Android", ""),
        Ok(Outcome::Succeed(true)),
    );
    assert_refused(&r, "parse");
}

#[test]
fn a_stub_or_missing_vendor_keeps_its_own_error() {
    let r = marathon(&["run"], STUB, Ok(Outcome::Succeed(true)));
    assert_eq!(r.exit, 1);
    assert!(r.stderr.contains("No vendor config present"), "{}", r.stderr);
    assert!(!r.stderr.contains(REFUSED), "{}", r.stderr);
}

#[test]
fn commands_that_run_no_test_do_not_apply_the_check() {
    let unset = vendor_config("", "Android", "");
    // Kotlin's default stays in the binding, which is what the JVM goldens replay.
    let r = marathon(&["config-dump", "Marathonfile"], &unset, Ok(Outcome::Succeed(true)));
    assert_eq!(r.exit, 0, "{}", r.stderr);
    assert!(r.stdout.contains("2147483647"), "{}", r.stdout);
    for args in [&["--help"][..], &["version"][..]] {
        let r = marathon(args, &unset, Ok(Outcome::Succeed(true)));
        assert_eq!(r.exit, 0, "{args:?}: {}", r.stderr);
    }
}

#[test]
fn the_binary_exits_1_with_the_message_on_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("Marathonfile"), vendor_config("", "Android", "")).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_marathon"))
        .current_dir(tmp.path())
        .env("ANDROID_HOME", "/android-sdk")
        .args(["run", "-m", "Marathonfile"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains(REFUSED));
}

#[test]
fn the_message_links_to_a_section_that_exists() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/migration.md");
    let migration = fs::read_to_string(path).unwrap();
    let anchor = marathon_config::loader::FINITE_RETRIES_MIGRATION_URL
        .rsplit_once('#')
        .unwrap()
        .1;
    assert_eq!(anchor, "a-finite-uncompletedtestretryquota-d47");
    assert!(migration.contains("\n### A finite `uncompletedTestRetryQuota` (D47)\n"));
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
            kind: marathon_test_support::recorded_errors::failure_kind(
                t["class"].as_str().unwrap(),
                t["message"].as_str(),
            )
            .unwrap_or_else(|e| panic!("{e}")),
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
    let (recorded, plain) = marathon_test_support::recorded_errors::throwable_strings(&row["throwable"]);
    assert_eq!(
        expected, recorded,
        "structured fields preserve the recorded headers and messages"
    );
    assert_eq!(render_throwable(&convert(&row["throwable"]).unwrap()), plain);
}

#[test]
fn external_trace_text_and_null_messages_render_as_the_jvm_does() {
    let f = Failure {
        // The external runner supplies this class and trace verbatim.
        kind: FailureKind::External("com.example.TestFailure".into()),
        message: None,
        cause: Some(Box::new(Failure {
            kind: FailureKind::Fatal(FatalKind::General),
            message: Some("x".into()),
            cause: None,
            stacktrace: Some("\tat worker.Frame(Frame.java:1)".into()),
        })),
        stacktrace: Some("\tat remote.Test(Test.kt:3)\n".into()),
    };
    assert_eq!(
        render_uncaught(&f),
        "Exception in thread \"main\" com.example.TestFailure\n\tat remote.Test(Test.kt:3)\n\
         Caused by: fatal failure: x\n\tat worker.Frame(Frame.java:1)\n"
    );
}

/// A binding error from the 2.4 loader: the outer `ConfigurationException` and its cause chain, outermost first.
#[test]
fn configuration_error_chain_renders_outermost_first() {
    let context = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: std::path::PathBuf::from("bad.yaml"),
        analytics_tracking: true,
        bugsnag_reporting: true,
        marathonfile_dir: std::path::PathBuf::from("/w"),
        process_dir: std::path::PathBuf::from("/w"),
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
        "Exception in thread \"main\" configuration rejected: Error parsing config file /w/bad.yaml"
    );
    assert!(first_lines[1].starts_with("Caused by: unrecognized configuration property"));
    assert_eq!(rendered.matches("Caused by: ").count(), error.causes.len());
}
