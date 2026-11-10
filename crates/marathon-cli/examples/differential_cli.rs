//! Hermetic paired CLI probe; real parsing, construction, vendors and reports.
use marathon_cli::{
    application::{CliOutput, CliRunner, Console, MarathonRun, Process},
    runner::{ProductionRunner, missing_vendors},
};
use marathon_config::Configuration;
use marathon_core::{
    error::Failure,
    executor::{
        CoreExecutor,
        hooks::{ClockMode, Draw, Ids, RuntimeHooks, Tape},
    },
    time::ReplayHooks,
};
use marathon_logging::{LogSettings, OutputTarget};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

const WALL: i64 = 1_577_836_800_000;
struct Probe {
    production: ProductionRunner,
    hooks: Arc<RuntimeHooks>,
}
impl CliRunner for Probe {
    fn construct(
        &self,
        config: Arc<Configuration>,
        log: Arc<Mutex<LogSettings>>,
        console: Arc<Console>,
    ) -> Result<Box<dyn MarathonRun>, Failure> {
        self.production
            .construct_with_hooks(config, log, console, self.hooks.clone(), Arc::new(|| WALL))
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut tape = Tape {
        wall: ClockMode::Frozen(WALL),
        monotonic: ClockMode::Frozen(0),
        ..Tape::default()
    };
    for (label, prefix, width, hex, start) in [
        ("core.TestBatch.id", "00000000-0000-0000-0000-", 12, false, 1),
        ("core.AllureReporter.uuid", "10000000-0000-0000-0000-", 12, false, 1),
    ] {
        tape.ids.insert(
            label.into(),
            Ids::Pattern {
                prefix: prefix.into(),
                width,
                hex,
                suffix: String::new(),
                start,
            },
        );
    }
    if let Ok(borrow) = std::env::var("MRS_DIFFERENTIAL_BORROW") {
        let label = std::env::var("MRS_DIFFERENTIAL_BORROW_LABEL")?;
        tape.draws.insert(label, vec![Draw::Key(borrow)]);
    }
    if let Ok(barriers) = std::env::var("MRS_DIFFERENTIAL_BARRIERS") {
        tape.barriers = serde_json::from_str(&barriers)?;
    }
    let hooks = CoreExecutor::run(move |core| Box::pin(async move { Ok(RuntimeHooks::taped(tape, &core.clock())) }))
        .map_err(|e| format!("{e:?}"))?;
    let cwd: PathBuf = std::env::current_dir()?;
    let environment = std::env::vars().collect();
    let process = Process {
        cwd: cwd.clone(),
        environment,
        now: jcompat::jtime::parse_instant("2020-01-01T00:00:00Z").ok_or("invalid probe instant")?,
    };
    let probe = Probe {
        production: ProductionRunner {
            factories: Arc::new(missing_vendors()),
            cwd,
            environment: process.environment.clone(),
            signal_hooks: Default::default(),
            #[cfg(feature = "test-seams")]
            exit_prepare: None,
        },
        hooks: hooks.clone(),
    };
    let output = CliOutput::new(
        Box::new(std::io::stdout()),
        Box::new(std::io::stderr()),
        OutputTarget::File,
        Box::new(|wall| marathon_cli::application::day_stamp(wall.unwrap_or(WALL))),
    )?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    let code = marathon_cli::run_cli(&args, &process, &probe, &output);
    hooks.verify_consumed().map_err(|e| format!("{e:?}"))?;
    let report = std::env::var_os("MRS_DIFFERENTIAL_HOOKS").ok_or("missing hook report path")?;
    std::fs::write(report, b"{\"active\":true,\"unused\":[],\"failures\":[]}")?;
    output.shutdown();
    std::process::exit(code);
}
