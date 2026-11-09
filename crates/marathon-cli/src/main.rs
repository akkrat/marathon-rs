//! Port of cli/src/main/kotlin/com/malinskiy/marathon/cli/ApplicationView.kt (`main`).
//!
//! The `marathon` binary: the process's arguments, working directory, environment, clock and standard streams
//! around `marathon_cli::run_cli` (PLAN §3.1).

use marathon_logging::OutputTarget;

use jcompat::{jtime::instant_of, };
use marathon_cli::{
    application::{CliOutput, Process, failure_exit},
    run_cli,
    runner::{ProductionRunner, missing_vendors},
};
use marathon_error::FailureKind;
use std::{
    io::{self, IsTerminal},
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

/// Where stdout goes, for terminal colour.
fn stdout_target() -> OutputTarget {
    let stdout = io::stdout();
    if stdout.is_terminal() {
        return OutputTarget::Terminal;
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        if let Ok(fd) = stdout.as_fd().try_clone_to_owned()
            && std::fs::File::from(fd).metadata().is_ok_and(|m| m.is_file())
        {
            return OutputTarget::File;
        }
    }
    OutputTarget::Pipe
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => return failure_exit(FailureKind::Io, &e.to_string()),
    };
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let Some(now) = instant_of(since_epoch.as_secs() as i64, i64::from(since_epoch.subsec_nanos())) else {
        return failure_exit(FailureKind::DateTime, "Instant exceeds minimum or maximum instant");
    };
    let process = Process {
        cwd,
        environment: std::env::vars_os()
            .map(|(k, v)| {
                (
                    String::from(k.to_string_lossy().as_ref()),
                    String::from(v.to_string_lossy().as_ref()),
                )
            })
            .collect(),
        now,
    };
    let output = match CliOutput::new(
        Box::new(io::stdout()),
        Box::new(io::stderr()),
        stdout_target(),
        Box::new(|| chrono::Local::now().naive_local().and_utc().timestamp_millis()),
    ) { Ok(output) => output, Err(error) => return failure_exit(FailureKind::Io, &error.to_string()) };
    if let Err(error) = tracing::dispatcher::set_global_default(output.console().tracing_dispatch()) { return failure_exit(FailureKind::IllegalState, &error.to_string()); }
    let runner = ProductionRunner {
        factories: std::sync::Arc::new(missing_vendors()),
        cwd: process.cwd.clone(),
        environment: process.environment.clone(),
        signal_hooks: Default::default(),
    };
    let code = run_cli(&args, &process, &runner, &output);
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
