//! Port of cli/src/main/kotlin/com/malinskiy/marathon/cli/ApplicationView.kt and
//! cli/src/main/kotlin/com/malinskiy/marathon/cli/MarathonStartConfiguration.kt.
//!
//! `execute` from the startup lines to the exit status. Errors that escape it in Kotlin (missing Marathonfile,
//! configuration errors, the vendor gate, construction) are rendered as the JVM's uncaught-exception output, without
//! stack frames (D12). Exceptions reporting is gone (D1): one INFO line after a successful parse replaces it.

use crate::args::CliConfiguration;
use jcompat::{
    file_manager::absolute_file_path,
    jtime::Instant,
    logging::{ConsoleOutput, JansiMode, Layout, Level, LogEvent, Logger, MarathonLogging, OutputTarget},
};
use marathon_config::{
    Configuration, VendorConfiguration,
    binding::{BindingContext, ConfigurationError, Profile},
    loader::{MarathonConfigurationLoader, validate_uncompleted_retries_bounded},
};
use marathon_core::{error::Failure, model::ExecutionCommand};
use marathon_error::FailureKind;
use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

/// `BuildConfig.VERSION`: the CI tag, else Kotlin's fallback (`buildSrc/.../Versions.kt`).
pub const VERSION: &str = match option_env!("GIT_TAG_NAME") {
    Some(tag) => tag,
    None => "0.10.1",
};

const LOGGER: &str = "com.malinskiy.marathon.cli.ApplicationView";
const D1_LINE: &str = "Crash reporting and usage analytics are not supported by this build; --bugsnag, --analyticsTracking, bugsnagReporting and analyticsTracking are ignored";

/// The run seam for WP 3.10: `marathonStartKoin(configuration, modules)` and `koin.get<Marathon>()`
/// (`ApplicationView.kt:82-83`). A construction error escapes `execute` (uncaught, exit 1). `logging` and `console` are
/// the registry and the console the startup lines used: logback has one `ConsoleAppender` on `System.out` and one
/// Jansi state per process, so the appenders 3.10's `configure` installs write through `console`, as the crash log does.
pub trait CliRunner {
    fn construct(
        &self,
        configuration: Arc<Configuration>,
        logging: Arc<Mutex<MarathonLogging>>,
        console: Arc<Console>,
    ) -> Result<Box<dyn MarathonRun>, Failure>;
}

/// `marathon.run(command)` (`ApplicationView.kt:85-90`): `Ok(success)`, or an error that takes the crash path.
pub trait MarathonRun {
    fn run(self: Box<Self>, command: ExecutionCommand) -> Result<bool, Failure>;
}

/// A runner whose construction always fails (tests of the uncaught path); the binary uses `runner::ProductionRunner`.
pub struct UnsupportedRunner;

impl CliRunner for UnsupportedRunner {
    fn construct(
        &self,
        _: Arc<Configuration>,
        _: Arc<Mutex<MarathonLogging>>,
        _: Arc<Console>,
    ) -> Result<Box<dyn MarathonRun>, Failure> {
        Err(failure(
            FailureKind::UnsupportedOperation,
            "Running tests is not supported by this build yet",
        ))
    }
}

/// What `execute` reads from the process: `user.dir`, the environment and the clock (one reading per process, P6).
pub struct Process {
    pub cwd: PathBuf,
    pub environment: Vec<(String, String)>,
    pub now: Instant,
}

pub type Sink = Box<dyn Write + Send>;

/// `System.out` with the console appender's Jansi state: plain text (`println`) and log events share one stream.
pub struct Console {
    stdout: Mutex<Sink>,
    output: Mutex<ConsoleOutput>,
}

fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Console {
    pub fn new(stdout: Sink, mode: JansiMode, target: OutputTarget) -> Self {
        Self {
            stdout: Mutex::new(stdout),
            output: Mutex::new(ConsoleOutput::new(mode, target)),
        }
    }

    /// Where stdout goes (a terminal or not), which decides what Jansi keeps.
    pub fn target(&self) -> OutputTarget {
        lock(&self.output).target
    }

    // The JVM's System.out swallows write errors (PrintStream.checkError); so do we.
    pub fn write_text(&self, text: &str) {
        let mut out = lock(&self.stdout);
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }

    /// One event in `layout` through the Jansi state; the state stays locked until its bytes are written, so events
    /// from several threads reach stdout in the order the parser saw them.
    pub fn write_event(&self, layout: Layout, event: &LogEvent<'_>) {
        let mut output = lock(&self.output);
        let bytes = output.format(layout, event);
        let mut out = lock(&self.stdout);
        let _ = out.write_all(&bytes);
        let _ = out.flush();
    }
}

/// The process's stdout console and stderr, and the logging registry with its stdout console appender.
pub struct CliOutput {
    console: Arc<Console>,
    stderr: Mutex<Sink>,
    logging: Arc<Mutex<MarathonLogging>>,
    logger: Logger,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl CliOutput {
    /// `clock` gives the local wall-clock time in milliseconds (only its time of day is printed). Jansi keeps its
    /// default mode (D2: no `jansi.mode` override).
    pub fn new(stdout: Sink, stderr: Sink, target: OutputTarget, clock: Box<dyn Fn() -> i64 + Send + Sync>) -> Self {
        let mut logging = MarathonLogging::default();
        let logger = logging.logger(LOGGER, None);
        Self {
            console: Arc::new(Console::new(stdout, JansiMode::Default, target)),
            stderr: Mutex::new(stderr),
            logging: Arc::new(Mutex::new(logging)),
            logger,
            clock,
        }
    }

    /// The per-process registry (later `VendorDependencies.logging`).
    pub fn logging(&self) -> Arc<Mutex<MarathonLogging>> {
        Arc::clone(&self.logging)
    }

    /// The per-process console (later the runtime appenders' stdout).
    pub fn console(&self) -> Arc<Console> {
        Arc::clone(&self.console)
    }

    pub fn out(&self, text: &str) {
        self.console.write_text(text);
    }

    // As System.err, write errors are swallowed.
    pub fn err(&self, text: &str) {
        let mut err = lock(&self.stderr);
        let _ = err.write_all(text.as_bytes());
        let _ = err.flush();
    }

    /// One event through the registry's current layout, on thread `main`.
    fn log(&self, level: Level, message: &str, throwable: &str) {
        let (layout, enabled) = {
            let logging = lock(&self.logging);
            (logging.layout(), logging.enabled(&self.logger, level))
        };
        if !enabled {
            return;
        }
        let event = LogEvent {
            level,
            logger: self.logger.name(),
            thread: "main",
            local_time_millis: (self.clock)(),
            message,
            throwable,
        };
        self.console.write_event(layout, &event);
    }
}

pub(crate) fn failure(kind: FailureKind, message: &str) -> Failure {
    Failure::from_kind(kind, Some(message.into()))
}

/// A `ConfigurationError` as the throwable it stands for: its `causes` are the cause chain, outermost first.
pub fn configuration_failure(error: &ConfigurationError) -> Failure {
    let cause = error.causes.iter().rev().fold(None, |inner, c| {
        Some(Box::new(Failure {
            kind: c.kind.clone(),
            message: c.message.to_owned(),
            cause: inner,
            stacktrace: None,
        }))
    });
    Failure {
        kind: error.kind.clone(),
        message: error.message.to_owned(),
        cause,
        stacktrace: None,
    }
}

/// `Throwable.printStackTrace` / logback's throwable converter without JVM frames (D12): `class: message` (`class`
/// alone for a null message), the failure's own trace text verbatim (external tools' output), then each cause as
/// `Caused by: ...`.
pub fn render_throwable(failure: &Failure) -> String {
    let mut out = String::new();
    let mut current = Some(failure);
    while let Some(f) = current {
        if !out.is_empty() {
            out.push_str("Caused by: ");
        }
        out.push_str(f.kind.legacy_class());
        if let Some(message) = &f.message {
            out.push_str(": ");
            out.push_str(&message.to_owned());
        }
        out.push('\n');
        if let Some(trace) = &f.stacktrace {
            let trace = trace.to_owned();
            out.push_str(&trace);
            if !trace.is_empty() && !trace.ends_with('\n') {
                out.push('\n');
            }
        }
        current = f.cause.as_deref();
    }
    out
}

/// An error before `main` reaches the command line: printed as uncaught, exit 1.
pub fn failure_exit(class: FailureKind, message: &str) -> std::process::ExitCode {
    let _ = io::stderr().write_all(render_uncaught(&failure(class, message)).as_bytes());
    std::process::ExitCode::from(1)
}

/// The JVM's default uncaught-exception handler for thread `main`.
pub fn render_uncaught(failure: &Failure) -> String {
    format!("Exception in thread \"main\" {}", render_throwable(failure))
}

/// `MarathonStartConfiguration`.
struct StartConfiguration {
    marathonfile: PathBuf,
    analytics_tracking: bool,
    bugsnag_reporting: bool,
    command: ExecutionCommand,
}

fn lossy(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

/// `File.isFile`: follows links; false for a missing file, a directory or a dangling link.
fn is_file(path: &str) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file())
}

/// The OS error text without Rust's ` (os error N)` suffix, as the JVM prints it.
pub(crate) fn os_error_text(error: &io::Error) -> String {
    let text = error.to_string();
    match text.rfind(" (os error ") {
        Some(at) if text.ends_with(')') => text[..at].to_owned(),
        _ => text,
    }
}

/// Reading an existing Marathonfile failed: the JVM's `FileNotFoundException: <path> (<reason>)` (P7 for the reason's
/// wording); a permission error keeps the NIO `AccessDeniedException` it wraps.
pub(crate) fn read_failure(given: &std::path::Path, error: &io::Error) -> Failure {
    let mut f = failure(
        FailureKind::FileNotFound,
        &format!("{} ({})", lossy(given), os_error_text(error)),
    );
    if error.kind() == io::ErrorKind::PermissionDenied {
        f.cause = Some(Box::new(failure(FailureKind::AccessDenied, &lossy(given))));
    }
    f
}

/// The Marathonfile's `canonicalFile.parentFile`.
pub(crate) fn canonical_parent(absolute: &str) -> Result<PathBuf, Failure> {
    let canonical = fs::canonicalize(absolute).map_err(|e| failure(FailureKind::Io, &os_error_text(&e)))?;
    let parent = canonical
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(PathBuf::from(&parent))
}

/// `ApplicationView.execute`: returns the exit status after printing everything the JVM would.
pub fn execute(configuration: CliConfiguration, process: &Process, runner: &dyn CliRunner, output: &CliOutput) -> i32 {
    output.log(
        Level::Info,
        &format!(
            "Starting marathon v{VERSION}, CliConfiguration: {}",
            configuration.to_kotlin_string()
        ),
        "",
    );
    let start = match configuration {
        CliConfiguration::Version => {
            output.out(&format!("{VERSION}\n"));
            return 0;
        }
        CliConfiguration::Run {
            marathonfile,
            analytics_tracking,
            bugsnag_reporting,
        } => StartConfiguration {
            marathonfile,
            analytics_tracking,
            bugsnag_reporting,
            command: ExecutionCommand::Run,
        },
        // `parse` turns both tracking flags off.
        CliConfiguration::Parse {
            marathonfile,
            output_file_name,
            include_flaky_tests,
        } => StartConfiguration {
            marathonfile,
            analytics_tracking: false,
            bugsnag_reporting: false,
            command: ExecutionCommand::Parse {
                output_file_name: output_file_name.map(|n| String::from(n.as_str())),
                include_flaky_tests,
            },
        },
    };
    match run(start, process, runner, output) {
        Ok(code) => code,
        Err(uncaught) => {
            output.err(&render_uncaught(&uncaught));
            1
        }
    }
}

/// The `try` block of `execute`; `Err` is an exception escaping `main`.
fn run(
    start: StartConfiguration,
    process: &Process,
    runner: &dyn CliRunner,
    output: &CliOutput,
) -> Result<i32, Failure> {
    let given = lossy(&start.marathonfile);
    output.log(Level::Info, &format!("Checking {given} config"), "");
    let cwd = PathBuf::from(process.cwd.to_string_lossy().into_owned());
    // `File.absolutePath`: joined to the working directory, not canonicalised.
    let absolute = lossy(&absolute_file_path(
        &start.marathonfile.to_string_lossy(),
        &cwd.to_string_lossy(),
    ));
    if !is_file(&absolute) {
        let message = format!("No config {absolute} present");
        output.log(Level::Error, &message, "");
        return Err(failure(
            FailureKind::Config(marathon_error::ConfigKind::Rejected),
            &message,
        ));
    }

    let bytes = fs::read(&absolute).map_err(|e| read_failure(&start.marathonfile, &e))?;
    let context = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: start.marathonfile.clone(),
        analytics_tracking: start.analytics_tracking,
        bugsnag_reporting: start.bugsnag_reporting,
        marathonfile_dir: canonical_parent(&absolute)?,
        process_dir: cwd,
        environment: process.environment.clone(),
        now: process.now,
    };
    let configuration = MarathonConfigurationLoader
        .parse_sync(&bytes, &context)
        .map_err(|e| configuration_failure(&e))?;

    output.log(Level::Info, D1_LINE, "");

    if matches!(
        configuration.vendor_configuration,
        VendorConfiguration::Stub | VendorConfiguration::Empty
    ) {
        return Err(failure(
            FailureKind::Config(marathon_error::ConfigKind::Rejected),
            &format!("No vendor config present in {absolute}"),
        ));
    }

    // D47. After the vendor gate, so that a stub or missing vendor keeps its own error; `run` and `parse` alike, since
    // a Marathonfile that `run` refuses should fail at a CI job's first step. `config-dump` runs no test and doesn't come here.
    validate_uncompleted_retries_bounded(&configuration).map_err(|e| configuration_failure(&e))?;

    let ignore_failures = configuration.ignore_failures;
    let marathon = runner.construct(Arc::new(configuration), output.logging(), output.console())?;

    // The final message is Clikt's `PrintMessage`: stdout for exit 0 without `printError`, stderr otherwise.
    match marathon.run(start.command) {
        Err(e) => {
            // `logger.error(e) {}`: the empty lambda's `Unit` is the message (C34).
            output.log(Level::Error, "kotlin.Unit", &render_throwable(&e));
            output.err("Marathon execution crashed\n");
            Ok(1)
        }
        Ok(true) => {
            output.out("Marathon execution finished\n");
            Ok(0)
        }
        Ok(false) if ignore_failures => {
            output.err(
                "Marathon execution finished with failures (Failures suppressed because ignoreFailures is `true`)\n",
            );
            Ok(0)
        }
        Ok(false) => {
            output.err("Marathon execution failed\n");
            Ok(1)
        }
    }
}
