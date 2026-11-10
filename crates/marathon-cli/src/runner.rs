//! Port of core/src/main/kotlin/com/malinskiy/marathon/di/Modules.kt
//! and cli/src/main/kotlin/com/malinskiy/marathon/cli/ApplicationView.kt (`marathonStartKoin` + `koin.get<Marathon>()`).
//!
//! The production `CliRunner`: one dedicated core executor per run. `construct` builds the run graph on it and reports
//! construction errors (the uncaught path); `run` hands it the command and waits for `Marathon.run`'s Boolean.

use crate::{
    VendorFactories,
    application::{CliRunner, Console, MarathonRun, VERSION, failure, render_throwable},
    core_output::RunConsoleSink,
};
use marathon_analytics::{AnalyticsClients, BackendClients};
use marathon_config::{AnalyticsConfiguration, Configuration};
use marathon_core::{
    construction::{MetricsBackends, RunInputs, construct},
    error::*,
    executor::{CoreExecutor, SharedDeferred, hooks::RuntimeHooks},
    logging::host_millis,
    model::ExecutionCommand,
};
use marathon_error::FailureKind;
use marathon_logging::LogSettings;
use std::{
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

/// SIGQUIT diagnostics are optional; failure must not prevent even help or vendor construction.
/// The startup entry point and embedded runners share one warning per process.
pub fn warn_quit_installation(error: &std::io::Error, console: &Console) {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::dispatcher::with_default(&console.tracing_dispatch(), || {
            tracing::warn!(target: "marathon_core::diagnostics", "SIGQUIT state dumps unavailable: {error}; continuing without SIGQUIT diagnostics");
        });
    }
}

/// The process facts a run needs beyond the configuration.
pub struct ProductionRunner {
    pub factories: Arc<VendorFactories>,
    pub cwd: PathBuf,
    pub environment: Vec<(String, String)>,
    /// Test seams for the signal relay; `main` leaves them empty.
    pub signal_hooks: marathon_core::marathon::SignalHooks,
    /// Test seam run at the start of exit preparation, on the exit-output
    /// thread (never core); `main` leaves it empty.
    #[cfg(feature = "test-seams")]
    pub exit_prepare: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Production vendor slots; vendors whose WPs have not landed fail construction.
pub fn missing_vendors() -> VendorFactories {
    fn missing(vendor: &'static str) -> Box<crate::VendorFactory> {
        Box::new(move |_| {
            Box::pin(async move {
                Err(VendorError::Other(failure(
                    FailureKind::UnsupportedOperation,
                    &format!("The {vendor} vendor is not supported by this build yet"),
                )))
            })
        })
    }
    VendorFactories {
        android: Box::new(|dependencies| Box::pin(marathon_android::services::create_services(dependencies))),
        ios: Box::new(|dependencies| Box::pin(marathon_ios::ios_vendor::construct(dependencies))),
        macos: Box::new(|dependencies| Box::pin(marathon_macos::construct(dependencies))),
        stub: missing("stub"),
        empty: missing("empty"),
    }
}

/// P1 (D12 extension): the facts logged under `System Information:`, in this order.
pub fn system_information(cwd: &std::path::Path) -> Vec<(String, String)> {
    let os_version = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".into());
    let processors = std::thread::available_parallelism().map_or(1, |n| n.get());
    let home = std::env::var_os("HOME")
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_default();
    [
        ("os.name", std::env::consts::OS.to_owned()),
        ("os.version", os_version),
        ("os.arch", std::env::consts::ARCH.to_owned()),
        ("available.processors", processors.to_string()),
        ("marathon.version", VERSION.to_owned()),
        ("user.dir", cwd.to_string_lossy().into_owned()),
        ("user.home", home),
    ]
    .into_iter()
    .map(|(k, v)| (String::from(k), v))
    .collect()
}

fn zone_offset_millis(wall: i64) -> i64 {
    use chrono::{Offset, TimeZone};
    chrono::Local
        .timestamp_millis_opt(wall)
        .single()
        .map_or(0, |time| i64::from(time.offset().fix().local_minus_utc()) * 1000)
}

struct Run {
    command: Arc<SharedDeferred<ExecutionCommand>>,
    thread: Option<JoinHandle<OperationResult<Option<bool>>>>,
    late: Option<marathon_core::logging::LateOutput>,
    console: Arc<Console>,
}

impl CliRunner for ProductionRunner {
    fn construct(
        &self,
        configuration: Arc<Configuration>,
        logging: Arc<Mutex<LogSettings>>,
        console: Arc<Console>,
    ) -> Result<Box<dyn MarathonRun>, Failure> {
        self.construct_with_hooks(
            configuration,
            logging,
            console,
            RuntimeHooks::live(),
            Arc::new(host_millis),
        )
    }
}

impl ProductionRunner {
    /// Paired CLI probes freeze wall/identity reads while retaining real vendor I/O
    /// and scheduler time. The public binary always supplies live hooks.
    pub fn construct_with_hooks(
        &self,
        configuration: Arc<Configuration>,
        logging: Arc<Mutex<LogSettings>>,
        console: Arc<Console>,
        hooks: Arc<RuntimeHooks>,
        wall: marathon_core::logging::WallSource,
    ) -> Result<Box<dyn MarathonRun>, Failure> {
        #[cfg(unix)]
        if let Err(error) = marathon_core::diagnostics::install_quit_handler() {
            warn_quit_installation(&error, &console);
        }
        let factories = self.factories.clone();
        let analytics = configuration.analytics_configuration.clone();
        let debug = configuration.debug;
        let barriers = hooks.barriers();
        let live_output = hooks.is_live();
        let exit_console = console.clone();
        let late_console = console.clone();
        let exit_tail = Arc::new(std::sync::OnceLock::<marathon_core::logging::LateOutput>::new());
        let publish_tail = exit_tail.clone();
        let mut signal_hooks = self.signal_hooks.clone();
        signal_hooks.immediate_exit = Some(Arc::new(|status| crate::tracing_output::exit_once(status)));
        #[cfg(feature = "test-seams")]
        let exit_prepare = self.exit_prepare.clone();
        #[cfg(not(feature = "test-seams"))]
        let exit_prepare: Option<Arc<dyn Fn() + Send + Sync>> = None;
        let inputs = RunInputs {
            configuration,
            logging: logging.clone(),
            console: Arc::new(RunConsoleSink::new(console, logging, Box::new(zone_offset_millis))),
            stderr: Arc::new(|f: &Failure| {
                let _ = std::io::stderr().write_all(render_throwable(f).as_bytes());
            }),
            cwd: PathBuf::from(self.cwd.to_string_lossy().into_owned()),
            environment: self.environment.clone(),
            system_information: system_information(&self.cwd),
            vendor: Box::new(move |dependencies| factories.construct(dependencies)),
            metrics: Box::new(move |context, io| {
                let clients = Arc::new(AnalyticsClients::new(context, io));
                let backend = (!matches!(analytics, AnalyticsConfiguration::Disabled)).then(|| {
                    Arc::new(BackendClients {
                        clients: clients.clone(),
                        config: analytics,
                        debug,
                    }) as Arc<dyn marathon_core::analytics::BackendFactory>
                });
                MetricsBackends {
                    setup: clients,
                    backend,
                }
            }),
            analytics: None,
            extra_reporters: Box::new(marathon_core::construction::standard_reporters),
            hooks,
            barriers,
            timer: None,
            wall,
            exit: Some(Arc::new(move |status| {
                let tail = exit_tail.get().cloned();
                let console = exit_console.clone();
                let exit_prepare = exit_prepare.clone();
                if exit_console
                    .exit_after_preparing(
                        status,
                        Box::new(move || {
                            if let Some(prepare) = &exit_prepare {
                                prepare();
                            }
                            if let Some(tail) = tail {
                                let _ = tail.close(Arc::new(move |record| match record {
                                    marathon_core::logging::OutputRecord::Raw(text) => console.write_text(&text),
                                    marathon_core::logging::OutputRecord::Log(record) => console.write_record(&record),
                                }));
                            }
                        }),
                    )
                    .is_err()
                {
                    crate::tracing_output::exit_once(status);
                }
            })),
            signal_trigger: None,
            signal_hooks,
            // Taped hooks mark a paired probe, whose stdout keeps the deterministic flush points.
            live_output,
        };
        let command = SharedDeferred::<ExecutionCommand>::new();
        let waiting = command.clone();
        let (constructed, outcome) =
            std::sync::mpsc::channel::<(Result<(), Failure>, marathon_core::logging::LateOutput)>();
        let thread = std::thread::Builder::new()
            .name("marathon-run".into())
            .spawn(move || {
                CoreExecutor::run(move |core| {
                    Box::pin(async move {
                        let late = core.log_queue().late_output();
                        let _ = publish_tail.set(late.clone());
                        let marathon = match construct(core.clone(), inputs).await {
                            Ok(marathon) => marathon,
                            Err(error) => {
                                let _ = constructed.send((Err(error), late));
                                return Ok(None);
                            }
                        };
                        let _ = constructed.send((Ok(()), late));
                        let command = match waiting.wait().await {
                            Ok(command) => (*command).clone(),
                            Err(_) => return Ok(None),
                        };
                        marathon.run(command).await.map(Some)
                    })
                })
            })
            .map_err(|e| failure(FailureKind::IllegalState, &e.to_string()))?;
        let mut run = Run {
            command,
            thread: Some(thread),
            late: None,
            console: late_console,
        };
        match outcome.recv() {
            Ok((Ok(()), late)) => {
                run.late = Some(late);
                Ok(Box::new(run))
            }
            Ok((Err(error), late)) => {
                run.late = Some(late);
                let _ = run.join();
                Err(error)
            }
            Err(_) => Err(match run.join() {
                Err(OperationError::Failed(f)) => f,
                _ => failure(FailureKind::IllegalState, "the run thread stopped during construction"),
            }),
        }
    }
}

impl Run {
    fn join(&mut self) -> OperationResult<Option<bool>> {
        let result = match self.thread.take() {
            Some(thread) => thread
                .join()
                .unwrap_or_else(|_| Err(failure(FailureKind::IllegalState, "the run thread panicked").into())),
            None => Ok(None),
        };
        if let Some(late) = self.late.take() {
            let console = self.console.clone();
            let _ = late.close(Arc::new(move |record| match record {
                marathon_core::logging::OutputRecord::Raw(text) => console.write_text(&text),
                marathon_core::logging::OutputRecord::Log(record) => console.write_record(&record),
            }));
        }
        result
    }
}

impl MarathonRun for Run {
    fn run(mut self: Box<Self>, command: ExecutionCommand) -> Result<bool, Failure> {
        self.command.complete(Ok(command));
        match self.join() {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Err(failure(FailureKind::IllegalState, "the run ended without a result")),
            Err(OperationError::Failed(f)) => Err(f),
            Err(OperationError::Cancelled(reason)) => Err(reason.failure().clone()),
        }
    }
}

/// A constructed run that is never started releases its executor thread.
impl Drop for Run {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.command
                .cancel(marathon_core::executor::shared::requested("run not started"));
            let _ = self.join();
        }
    }
}
