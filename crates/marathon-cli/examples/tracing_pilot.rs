//! Port of cli/src/main/kotlin/com/malinskiy/marathon/cli/ApplicationView.kt (`main`).
//! Milestone-1 executable: startup, version, and configuration/construction errors with the new subscriber.

use marathon_cli::{
    application::{CliOutput, Process, UnsupportedRunner},
    run_cli,
    tracing_output::TracingOutput,
};
use std::{
    io::{self, IsTerminal},
    process::ExitCode,
};

fn main() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let (tracing, guard) = TracingOutput::new(io::stdout(), io::stdout().is_terminal(), || {
        chrono::Local::now().format("%H:%M:%S%.3f").to_string()
    })?;
    let output = CliOutput::traced(tracing, Box::new(io::stderr()));
    let process = Process {
        cwd: std::env::current_dir()?,
        environment: std::env::vars().collect(),
        now: jcompat::jtime::instant_of(0, 0).ok_or("invalid pilot instant")?,
    };
    let args: Vec<_> = std::env::args().skip(1).collect();
    let code = run_cli(&args, &process, &UnsupportedRunner, &output);
    guard.shutdown()?;
    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
}
