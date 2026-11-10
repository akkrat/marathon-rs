//! Port of core/src/main/kotlin/com/malinskiy/marathon/Marathon.kt
//! and core/src/main/kotlin/com/malinskiy/marathon/ShutdownHook.kt.
//!
//! `run_async` is the source's `runAsync`; `run` is its `runBlocking` → supervisor →
//! `async` wrapper with the exceptional `NonCancellable` finish and the catch that maps
//! failures to a Boolean. Everything here runs on the core thread; vendor, analytics,
//! file and registry work is awaited on the vendor runtime.

use crate::{
    artifact_io::{blocking, clock_error, vendor_error},
    ci::Ci,
    error::*,
    execution::scheduler::{Scheduler, SchedulerParts},
    executor::{CoreExecutor, bridge::call_vendor, retry::with_retry, shared::*},
    filter::{FilterDependencies, TestFilter},
    model::{ExecutionCommand, Test, TestShard},
    output::{OutputDriver, PumpErrors},
    runtime::*,
    strategy::{flakiness::FlakinessStrategy, sharding::ShardingStrategy},
    vendor::{DeviceRef, TestParser, VendorContext},
    wiring::RunDependencies,
};
use marathon_config::{TestFilterConfiguration, binding::ConfigurationError, loader::LogicalConfigurationValidator};
use marathon_error::FailureKind;
use marathon_logging::Level;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

/// `private val log = LogSettings.logger {}` in Marathon.kt: the Kotlin logger drops the `Kt` suffix.
pub const LOGGER: &str = "marathon_core::marathon";
/// `TimingLog.LOGGER_NAME`.
pub const TIMINGS: &str = "marathon_core::timing";
const NO_DEVICES: FailureKind = FailureKind::NoDevices;
const NO_TESTS: FailureKind = FailureKind::NoTests;

/// D52: what a cancelled run gets before it exits, whatever is still running. A CI's grace between SIGTERM and SIGKILL is
/// often 10 to 30 s. Reports come first, so a shorter grace costs cleanup and not results, and this stays under the
/// longer one. It leaves room for D30's TERM, its 20 s and the KILL of a session the cancellation closed at the start.
pub const CANCEL_LIMIT_MILLIS: i64 = 24_000;
/// D52: how long one flush may hold a cancelled run, for a console that has stopped reading. The last flush comes after
/// the limit above, so 25 s in all.
pub const CANCEL_FLUSH_MILLIS: i64 = 1_000;
/// D52: how long a cancelled run's cleanup waits for the reports. They keep going after this, but a stuck backend's close,
/// which comes after the report files, must not use up the time for the provider.
pub const CANCEL_REPORTS_HEAD_START_MILLIS: i64 = 10_000;

/// `TimingLog.info(timer) { message }`'s text: the message, then the time since start.
pub fn timing_message(message: &str, timer: &dyn crate::time::Timer) -> crate::time::ClockResult<String> {
    let elapsed = timer.elapsed_time_millis()?;
    Ok(format!("{message} (+{elapsed} ms since start)"))
}

/// What the frozen `RunDependencies` cannot carry: the concrete executor Scheduler needs
/// (3.8), the console driver, CI detection, the filters' services, the P1 facts and the
/// optional signal relay (P3).
pub struct RunServices {
    pub core: Rc<CoreExecutor>,
    pub output: OutputDriver,
    pub ci: Ci,
    pub filters: FilterDependencies,
    /// P1: the fixed, ordered facts logged under `System Information:` (key, value).
    pub system_information: Vec<(String, String)>,
    pub signals: Option<Rc<SignalRelay>>,
    /// Flush the log queue as records arrive (live runs only; see `start_output_pump`).
    pub live_output: bool,
}

/// One registered signal: yields once per delivery for as long as it is kept.
pub type SignalStream = std::pin::Pin<Box<dyn futures_util::Stream<Item = ()> + Send>>;
/// Installs the handler for a signal number; runs on the vendor runtime.
pub type SignalRegistration = Arc<dyn Fn(i32) -> std::io::Result<SignalStream> + Send + Sync>;

/// Test seams for the OS relay. The default registers through tokio and announces nothing.
#[derive(Clone, Default)]
pub struct SignalHooks {
    /// Production emergency exit, which bypasses output draining on the second signal.
    pub immediate_exit: Option<Arc<dyn Fn(i32) + Send + Sync>>,
    /// Replaces the tokio registration (failure injection).
    pub registration: Option<SignalRegistration>,
    /// Called once both registrations have been attempted.
    pub ready: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// The tokio registration of `number`: the handler stays installed for the process's life.
#[cfg(unix)]
pub fn tokio_registration(number: i32) -> std::io::Result<SignalStream> {
    use tokio::signal::unix::{SignalKind, signal};
    let stream = signal(SignalKind::from_raw(number))?;
    Ok(Box::pin(futures_util::stream::unfold(
        stream,
        |mut stream| async move { stream.recv().await.map(|()| ((), stream)) },
    )))
}

/// Whether `number`'s current disposition is SIG_IGN. HotSpot installs no handler for a
/// SIGINT or SIGTERM the process started with ignored (shell background jobs, CI wrappers),
/// so the relay leaves such a signal ignored as well.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn signal_ignored(number: i32) -> bool {
    use nix::libc;
    // SAFETY: with a null `act`, sigaction(2) changes nothing and only writes the current
    // disposition into `old`, zeroed storage of the right type; `old` is read only when the
    // call succeeded, so it is initialised.
    unsafe {
        let mut old = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
        libc::sigaction(number, std::ptr::null(), old.as_mut_ptr()) == 0
            && old.assume_init().sa_sigaction == libc::SIG_IGN
    }
}

/// P3: the one owned relay for SIGINT/SIGTERM. A vendor-side listener completes `signal`
/// with the signal number; `run` awaits it on core. The JVM exits with 128 + the signal
/// number after its shutdown hooks, so `exit` receives that status in both debug modes.
///
/// The OS handlers are installed only when the run starts, without waiting on core for the
/// registration: until then the default disposition ends the process (the shell sees 130/143,
/// as with a JVM still constructing). Each signal is registered on its own: one that fails
/// keeps its default disposition, while one that succeeded is still relayed (tokio keeps a
/// registered handler, so abandoning its stream would swallow the signal). A signal the
/// process started with ignored is not registered at all and stays ignored, as on HotSpot.
///
/// D52: after the first signal the listener keeps listening, off core, and a second one exits at once. The hook is
/// not asked to run again, so nothing is done twice; whatever it has not done by then is lost. Each signal keeps the
/// one stream it was registered with for both deliveries: a stream dropped and registered again would miss a signal
/// that came in between.
pub struct SignalRelay {
    signal: Arc<SharedDeferred<i32>>,
    source: Option<Arc<dyn VendorRuntime>>,
    hooks: SignalHooks,
    listener: RefCell<Option<Arc<dyn VendorTask>>>,
    exit: Arc<dyn Fn(i32) + Send + Sync>,
}
impl SignalRelay {
    /// The process's SIGINT and SIGTERM, delivered through the vendor runtime's I/O driver.
    /// Tokio keeps its handlers installed after the run (a later signal is ignored rather
    /// than ending the process; the CLI exits right after the run).
    #[cfg(unix)]
    pub fn os(vendor: Arc<dyn VendorRuntime>, exit: Arc<dyn Fn(i32) + Send + Sync>, hooks: SignalHooks) -> Rc<Self> {
        Rc::new(Self {
            signal: SharedDeferred::<i32>::new(),
            source: Some(vendor),
            hooks,
            listener: RefCell::new(None),
            exit,
        })
    }
    /// A relay triggered by completing `signal` with a signal number (tests).
    pub fn manual(signal: Arc<SharedDeferred<i32>>, exit: Arc<dyn Fn(i32) + Send + Sync>) -> Rc<Self> {
        Rc::new(Self {
            signal,
            source: None,
            hooks: SignalHooks::default(),
            listener: RefCell::new(None),
            exit,
        })
    }
    #[cfg(unix)]
    fn listen(&self) {
        let Some(vendor) = &self.source else { return };
        let output = self.signal.clone();
        let registration = self
            .hooks
            .registration
            .clone()
            .unwrap_or_else(|| Arc::new(tokio_registration));
        let ready = self.hooks.ready.clone();
        let exit = self.hooks.immediate_exit.clone().unwrap_or_else(|| self.exit.clone());
        let listener = vendor.spawn_cancellable(Box::new(move |cancelled| {
            Box::pin(async move {
                let numbered = |number: i32| {
                    let stream = registration(number).ok()?;
                    Some(futures_util::StreamExt::map(stream, move |()| number))
                };
                let streams: Vec<_> = [2, 15]
                    .into_iter()
                    .filter(|&number| !signal_ignored(number))
                    .filter_map(numbered)
                    .collect();
                if let Some(ready) = ready {
                    ready();
                }
                if streams.is_empty() {
                    return Ok(());
                }
                let mut deliveries = futures_util::stream::select_all(streams);
                let next = futures_util::StreamExt::next(&mut deliveries);
                let number = tokio::select! {
                    number = next => match number {
                        Some(number) => number,
                        None => return Ok(()),
                    },
                    _ = cancelled.cancelled() => return Ok(()),
                };
                output.complete(Ok(number));
                let next = futures_util::StreamExt::next(&mut deliveries);
                tokio::select! {
                    Some(number) = next => exit(128 + number),
                    _ = cancelled.cancelled() => {}
                }
                Ok(())
            })
        }));
        *self.listener.borrow_mut() = Some(listener);
    }
    #[cfg(not(unix))]
    fn listen(&self) {}
    fn stop(&self) {
        if let Some(listener) = self.listener.borrow_mut().take() {
            listener.cancel(requested("run finished"));
        }
    }
    fn fired(&self) -> bool {
        self.signal.completed().is_some()
    }
}

/// `ShutdownHook`. D52: installed whatever `debug` says; Kotlin installs it only with `debug: true` (C10). Installation
/// and removal cannot fail natively, so the swallowed IllegalStateException/SecurityException paths are gone (P3);
/// `uninstall` returns true as after a nonthrowing `removeShutdownHook`.
struct ShutdownHook {
    installed: Rc<Cell<bool>>,
}
impl ShutdownHook {
    fn install(&self) {
        self.installed.set(true);
    }
    fn uninstall(&self) -> bool {
        self.installed.set(false);
        true
    }
}

/// The steps at the end of a run, each of which runs once.
#[derive(Clone, Copy)]
enum FinishStep {
    Analytics,
    Provider,
    Reports,
    Usage,
}
impl FinishStep {
    fn label(self) -> &'static str {
        match self {
            Self::Analytics => "Closing the analytics history",
            Self::Provider => "Terminating the device provider",
            Self::Reports => "Closing the reports and tracker",
            Self::Usage => "Closing the usage tracker",
        }
    }
}
#[derive(Default)]
struct StepState {
    started: Cell<bool>,
    done: SharedDeferred<()>,
}
/// D52: a signal can arrive while the run is in the middle of its own `on_finish`, so the two share these. Whichever gets
/// to a step first runs it, and the other waits for it, so nothing runs twice and the signal's order still holds for
/// what is left: with the run in a slow provider termination, the signal writes the reports now.
#[derive(Default)]
struct FinishSteps {
    analytics: StepState,
    provider: StepState,
    reports: StepState,
    usage: StepState,
}

/// The relay, the job its task runs under, and that task.
type RunningRelay = (Rc<SignalRelay>, JobRef, JobRef);

pub struct Marathon {
    deps: RunDependencies,
    services: RunServices,
    validator: LogicalConfigurationValidator,
    /// `cleanedUp`: one core thread, so a Cell is the AtomicBoolean.
    cleaned_up: Cell<bool>,
    hook_installed: Rc<Cell<bool>>,
    steps: FinishSteps,
    /// D52: the output directory is this run's, wiped and ready for reports.
    output_ready: Cell<bool>,
    /// D52: what a signal stops, as soon as there is something to stop.
    run_job: RefCell<Option<JobRef>>,
    scheduler_job: RefCell<Option<JobRef>>,
    diagnostic_scheduler: RefCell<Option<Scheduler>>,
    /// First failure of a live pump's write, left for the next serial flush to report.
    pump_error: PumpErrors,
}

/// A stage (b) ConfigurationError as its throwable: `causes` are outermost first.
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

fn text(parts: &[&str]) -> String {
    let mut out = String::default();
    for part in parts {
        out.push_str(part);
    }
    out
}

impl Marathon {
    pub fn new(deps: RunDependencies, services: RunServices) -> Rc<Self> {
        Rc::new(Self {
            deps,
            services,
            validator: LogicalConfigurationValidator,
            cleaned_up: Cell::new(false),
            hook_installed: Rc::new(Cell::new(false)),
            steps: FinishSteps::default(),
            output_ready: Cell::new(false),
            run_job: RefCell::new(None),
            scheduler_job: RefCell::new(None),
            diagnostic_scheduler: RefCell::new(None),
            pump_error: PumpErrors::default(),
        })
    }
    pub fn dependencies(&self) -> &RunDependencies {
        &self.deps
    }
    pub fn services(&self) -> &RunServices {
        &self.services
    }
    /// The retained Scheduler owner, for gated run-level admission witnesses.
    #[cfg(feature = "test-support")]
    pub fn scheduler_job(&self) -> Option<JobRef> {
        self.scheduler_job.borrow().clone()
    }
    /// Whether the shutdown hook is currently installed (D52, C10).
    pub fn hook_installed(&self) -> bool {
        self.hook_installed.get()
    }

    fn log(&self, logger: &str, level: Level, message: String, cause: Option<Failure>) {
        let core = &self.services.core;
        core.log_queue().log(
            logger,
            crate::time::SchedulerClock::now_millis(&*core.clock()),
            level,
            message,
            cause,
        );
    }
    async fn flush(&self, cancellation: Arc<dyn CancellationSignal>) -> VendorResult<()> {
        self.services
            .output
            .flush(&self.services.core.log_queue(), cancellation)
            .await?;
        match self.pump_error.lock().ok().and_then(|mut slot| slot.take()) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    fn elapsed(&self) -> VendorResult<i64> {
        self.deps.vendor.timer.elapsed_time_millis().map_err(clock_error)
    }
    /// `TimingLog.info(timer) { message }`: the message (and its duration read) first, then
    /// the separate elapsed read of the suffix. Timings is fixed at INFO, so both always run.
    fn timing(&self, message: impl FnOnce() -> VendorResult<String>) -> VendorResult<()> {
        let message = message()?;
        let line = timing_message(&message, self.deps.vendor.timer.as_ref()).map_err(clock_error)?;
        self.log(TIMINGS, Level::INFO, line, None);
        Ok(())
    }
    fn context(&self, cancellation: Arc<dyn CancellationSignal>) -> VendorContext {
        VendorContext {
            cancellation,
            clock: self.deps.vendor.clock.clone(),
            hooks: self.deps.vendor.hooks.clone(),
        }
    }
    async fn vendor<T: Send + 'static>(
        &self,
        task: &TaskContext,
        call: impl FnOnce(Arc<dyn CancellationSignal>) -> VendorFuture<'static, T> + Send + 'static,
    ) -> VendorResult<T> {
        call_vendor(
            self.services.core.vendor().as_ref(),
            task.job.cancellation_signal(),
            call,
        )
        .await
    }

    /// `configureLogging()`: `LogSettings.debug` first, then the vendor's appenders.
    async fn configure_logging(&self, task: &TaskContext) -> VendorResult<()> {
        let debug = self.deps.vendor.configuration.debug;
        let logging = self.deps.vendor.logging.clone();
        let registry = logging.clone();
        blocking(self.services.core.vendor().as_ref(), move || {
            registry
                .lock()
                .map_err(|_| VendorError::Other(failure(FailureKind::IllegalState, "Poisoned logging registry")))?
                .debug = debug;
            Ok(())
        })
        .await?;
        let configurator = self.deps.log_configurator.clone();
        self.vendor(task, move |_| {
            Box::pin(async move { configurator.configure(debug, logging).await })
        })
        .await?;
        // TimingLog's logger is fixed at INFO whenever it is first used.
        let registry = self.deps.vendor.logging.clone();
        blocking(self.services.core.vendor().as_ref(), move || {
            registry
                .lock()
                .map_err(|_| VendorError::Other(failure(FailureKind::IllegalState, "Poisoned logging registry")))?
                .timings();
            Ok(())
        })
        .await
    }

    /// `logSystemInformation()` under P1: the heading, then the documented OS facts.
    fn log_system_information(&self) {
        self.log(LOGGER, Level::INFO, "System Information:".into(), None);
        for (key, value) in &self.services.system_information {
            let mut line = key.clone();
            line.push_str(": ");
            line.push_str(&value.clone());
            self.log(LOGGER, Level::INFO, line, None);
        }
    }

    /// `runAsync`. Errors propagate unmapped; only `run` applies the source's catch.
    pub async fn run_async(&self, command: &ExecutionCommand, task: &TaskContext) -> VendorResult<bool> {
        let configuration = self.deps.vendor.configuration.clone();
        let hooks = self.deps.vendor.hooks.clone();
        // measureTimeMillis reads System.currentTimeMillis; a throwing block takes no second read.
        let parsing_started = hooks.millis("core.Marathon.runAsync.parsing").map_err(clock_error)?;
        self.configure_logging(task).await?;
        self.log_system_information();
        self.validator
            .validate_sync(&configuration)
            .map_err(|e| VendorError::Other(configuration_failure(&e)))?;

        let parse = matches!(command, ExecutionCommand::Parse { .. });
        if parse && matches!(self.deps.parser, TestParser::Local(_)) {
            self.log(
                LOGGER,
                Level::INFO,
                "DeviceProvider is not required for local parsing".into(),
                None,
            );
        } else {
            let started = self.elapsed()?;
            let provider = self.deps.provider.clone();
            let context = self.context(task.job.cancellation_signal());
            self.vendor(task, move |signal| {
                Box::pin(async move {
                    provider
                        .initialize(VendorContext {
                            cancellation: signal,
                            ..context
                        })
                        .await
                })
            })
            .await?;
            self.timing(|| {
                Ok(format!(
                    "Device provider initialized in {} ms",
                    self.elapsed()?.wrapping_sub(started)
                ))
            })?;
        }
        self.flush(task.job.cancellation_signal()).await?;

        let parsed_all_tests = match &self.deps.parser {
            TestParser::Local(parser) => {
                let started = self.elapsed()?;
                let parser = parser.clone();
                let context = self.context(task.job.cancellation_signal());
                let tests = self
                    .vendor(task, move |signal| {
                        Box::pin(async move {
                            parser
                                .extract(VendorContext {
                                    cancellation: signal,
                                    ..context
                                })
                                .await
                        })
                    })
                    .await?;
                self.timing(|| {
                    Ok(format!(
                        "Parsed {} tests in {} ms",
                        tests.len(),
                        self.elapsed()?.wrapping_sub(started)
                    ))
                })?;
                tests
            }
            TestParser::Remote(parser) => self.extract_remotely(parser.clone(), task).await?,
        };
        let parsing_finished = hooks.millis("core.Marathon.runAsync.parsing").map_err(clock_error)?;
        self.log(
            LOGGER,
            Level::DEBUG,
            format!("Parsing took {} ms", parsing_finished.wrapping_sub(parsing_started)),
            None,
        );
        self.flush(task.job.cancellation_signal()).await?;

        // D1: usageTracker.meta is gone.
        if parsed_all_tests.is_empty() {
            return Err(VendorError::Other(failure(NO_TESTS, "No tests cases were found")));
        }
        let (parsed_filtered_tests, filter_stages) = self.apply_test_filters(parsed_all_tests).await?;

        if let ExecutionCommand::Parse {
            output_file_name,
            include_flaky_tests,
        } = command
        {
            // Delay potentially querying remote TSDB unless user requested to get flaky tests
            let flaky_tests = if *include_flaky_tests {
                self.prepare_test_shard(&parsed_filtered_tests, task).await?.flaky_tests
            } else {
                Vec::new()
            };
            let output = self.deps.parse_output.clone();
            let name = output_file_name.clone();
            let tests = parsed_filtered_tests.clone();
            self.vendor(task, move |_| {
                Box::pin(async move { output.execute(&tests, &flaky_tests, name.as_deref()).await })
            })
            .await?;
            self.flush(task.job.cancellation_signal()).await?;
            // C26: no onFinish on a successful parse.
            return Ok(true);
        }

        let shard_started = self.elapsed()?;
        let shard = self.prepare_test_shard(&parsed_filtered_tests, task).await?;
        let analytics = self.deps.analytics.clone();
        let covered = shard.clone();
        self.vendor(task, move |_| {
            Box::pin(async move {
                crate::analytics::report_execution_time_coverage(&configuration, &covered, analytics.as_ref())
                    .await
                    .map_err(vendor_error)
            })
        })
        .await?;
        self.timing(|| {
            Ok(format!(
                "Prepared the shard of {} tests and the analytics history it uses in {} ms",
                parsed_filtered_tests.len(),
                self.elapsed()?.wrapping_sub(shard_started)
            ))
        })?;
        // D1: TestsTotal/TestsRun usage events are gone.
        let mut names = String::default();
        for (index, test) in parsed_filtered_tests.iter().enumerate() {
            if index > 0 {
                names.push_str(", ");
            }
            names.push_str(&test.to_test_name('.', '#'));
        }
        self.log(LOGGER, Level::DEBUG, names, None);

        let scheduler = Scheduler::new(SchedulerParts {
            core: self.services.core.clone(),
            provider: self.deps.provider.clone(),
            analytics: self.deps.analytics.clone(),
            configuration: self.deps.vendor.configuration.clone(),
            shard,
            tracker: self.deps.vendor.tracker.clone(),
            timer: self.deps.vendor.timer.clone(),
            bundle_identifier: self.deps.bundle_identifier.clone(),
            hooks: self.deps.vendor.hooks.clone(),
            barriers: self.deps.vendor.barriers.clone(),
            ci: self.services.ci,
            output: self.services.output.clone(),
            context: task.clone(),
        });
        *self.scheduler_job.borrow_mut() = Some(scheduler.job());
        *self.diagnostic_scheduler.borrow_mut() = Some(scheduler.clone());

        // D52: a signal may have stopped this run while it was in a call that ignores cancellation, and the wipe would
        // take the last run's reports with it for nothing.
        if let Some(reason) = task.job.cancellation_signal().reason() {
            return Err(VendorError::Cancelled(reason));
        }
        self.wipe_output_dir().await?;
        self.output_ready.set(true);

        self.timing(|| Ok(format!("Scheduling {} tests", parsed_filtered_tests.len())))?;
        self.flush(task.job.cancellation_signal()).await?;
        let result = if parsed_filtered_tests.is_empty() {
            // D51: a green run that ran nothing should at least say why, and this logger shows nothing below ERROR
            // with `debug: false`. It may be right (an empty shard fragment), so it stays exit 0; `parse` writes its
            // empty list as before.
            self.log(
                LOGGER,
                Level::ERROR,
                format!("No tests left to run after filtering: {}", filter_stages.join(", ")),
                None,
            );
            true
        } else {
            scheduler.execute().await?
        };

        self.on_finish(task.job.cancellation_signal()).await?;
        Ok(result)
    }

    /// `withTimeoutOrNull(deviceInitializationTimeoutMillis) { withRetry(3, 0) { borrow; extract } }`.
    /// Only this scope's own expiry becomes NoDevices; the last attempt's typed error survives.
    /// D53 (A103): an attempt borrows a device that no earlier attempt failed on, if one is available.
    async fn extract_remotely(
        &self,
        parser: Arc<dyn crate::vendor::RemoteTestParser>,
        task: &TaskContext,
    ) -> VendorResult<Vec<Test>> {
        let tests: RefCell<Option<Vec<Test>>> = RefCell::new(None);
        let last: RefCell<Option<VendorError>> = RefCell::new(None);
        let tried: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let millis = self.deps.vendor.configuration.device_initialization_timeout_millis;
        let (tests_cell, last_cell, tried) = (&tests, &last, &tried);
        let outcome = self
            .deps
            .timeouts
            .with_timeout_or_null(
                task.clone(),
                millis,
                Box::new(|scope: TaskContext| {
                    Box::pin(async move {
                        let (tests, last, scope_ref) = (tests_cell, last_cell, &scope);
                        let attempt = || {
                            let parser = parser.clone();
                            async move {
                                let result = self.borrow_and_extract(parser, tried, scope_ref).await;
                                result.map_err(|error| {
                                    let operation = match &error {
                                        VendorError::Cancelled(reason) => OperationError::Cancelled(reason.clone()),
                                        other => OperationError::Failed(other.failure().clone()),
                                    };
                                    *last.borrow_mut() = Some(error);
                                    operation
                                })
                            }
                        };
                        let value = with_retry(3, 0, &*scope.clock, scope.job.clone(), attempt).await?;
                        *tests.borrow_mut() = Some(value);
                        Ok(())
                    })
                }),
            )
            .await;
        match outcome {
            Ok(Some(())) => Ok(tests.take().unwrap_or_default()),
            Ok(None) => Err(VendorError::NoDevices(failure(
                NO_DEVICES,
                "Timed out waiting for a temporary device for remote test parsing",
            ))),
            Err(error) => {
                let last = last.take();
                Err(match (error, last) {
                    (OperationError::Failed(failure), Some(original)) if *original.failure() == failure => original,
                    (error, _) => vendor_error(error),
                })
            }
        }
    }

    async fn borrow_and_extract(
        &self,
        parser: Arc<dyn crate::vendor::RemoteTestParser>,
        tried: &RefCell<Vec<String>>,
        scope: &TaskContext,
    ) -> VendorResult<Vec<Test>> {
        let wait_started = self.elapsed()?;
        let provider = self.deps.provider.clone();
        let context = self.context(scope.job.cancellation_signal());
        let avoid = tried.borrow().clone();
        let (device, key): (DeviceRef, String) = self
            .vendor(scope, move |signal| {
                Box::pin(async move {
                    provider
                        .borrow_untried(
                            avoid,
                            VendorContext {
                                cancellation: signal,
                                ..context
                            },
                        )
                        .await
                })
            })
            .await?;
        let started = self.elapsed()?;
        let context = self.context(scope.job.cancellation_signal());
        let extracted = device.clone();
        let tests = self
            .vendor(scope, move |signal| {
                Box::pin(async move {
                    parser
                        .extract(
                            extracted,
                            VendorContext {
                                cancellation: signal,
                                ..context
                            },
                        )
                        .await
                })
            })
            .await;
        // D53: a cancellation says nothing about the device. A failed borrow never gets here, so it marks none.
        let tests = match tests {
            Err(error) if !matches!(error, VendorError::Cancelled(_)) => {
                let mut tried = tried.borrow_mut();
                if !tried.contains(&key) {
                    tried.push(key);
                }
                return Err(error);
            }
            other => other?,
        };
        self.timing(|| {
            Ok(format!(
                "Parsed {} tests in {} ms on {}, which took {} ms to become available",
                tests.len(),
                self.elapsed()?.wrapping_sub(started),
                device.0.info().serial_number.to_owned(),
                started.wrapping_sub(wait_started)
            ))
        })?;
        Ok(tests)
    }

    /// Each allowlist filter, then each blocklist filter, freshly constructed per call.
    /// Also returns what each one removed, as the empty-run warning's text.
    async fn apply_test_filters(&self, parsed: Vec<Test>) -> VendorResult<(Vec<Test>, Vec<String>)> {
        let vendor = self.services.core.vendor();
        let filtering = &self.deps.vendor.configuration.filtering_configuration;
        let mut stages = Vec::new();
        let mut tests = parsed;
        for config in &filtering.allowlist {
            let before = tests.len();
            tests = TestFilter::from_config(config, &self.services.filters)
                .filter(tests, vendor.as_ref())
                .await
                .map_err(vendor_error)?;
            stages.push(removed_by("allowlist", config, before - tests.len()));
        }
        for config in &filtering.blocklist {
            let before = tests.len();
            tests = TestFilter::from_config(config, &self.services.filters)
                .filter_not(tests, vendor.as_ref())
                .await
                .map_err(vendor_error)?;
            stages.push(removed_by("blocklist", config, before - tests.len()));
        }
        Ok((tests, stages))
    }

    /// Shard first, flakiness second. D13: the flakiness key is fetched where `process`
    /// first queries it, and only when it will (ProbabilityBased, nonempty shard).
    async fn prepare_test_shard(&self, tests: &[Test], task: &TaskContext) -> VendorResult<TestShard> {
        let configuration = &self.deps.vendor.configuration;
        let sharding = ShardingStrategy::from_config(&configuration.sharding_strategy);
        let flakiness = FlakinessStrategy::from_config(&configuration.flakiness_strategy);
        let shard = sharding.create_shard(tests);
        let keys = flakiness.metrics_keys();
        if !keys.is_empty() && !shard.tests.is_empty() {
            let analytics = self.deps.analytics.clone();
            self.vendor(task, move |_| {
                Box::pin(async move { analytics.prefetch(&keys).await.map_err(vendor_error) })
            })
            .await?;
        }
        Ok(flakiness.process(shard, self.deps.analytics.lookup().as_ref()))
    }

    /// `if (outputDir.exists()) { log; deleteRecursively() }; mkdirs()`, both Boolean
    /// results ignored (C9: whatever the provider or parser wrote there is lost).
    async fn wipe_output_dir(&self) -> VendorResult<()> {
        let output = self.deps.vendor.configuration.output_dir.clone();
        let path = output.clone();
        let probe = path.clone();
        let exists = blocking(self.services.core.vendor().as_ref(), move || Ok(probe.exists())).await?;
        if exists {
            let mut message = text(&["Output "]);
            message.push_str(&output.to_string_lossy());
            message.push_str(" already exists");
            self.log(LOGGER, Level::INFO, message, None);
        }
        blocking(self.services.core.vendor().as_ref(), move || {
            if exists {
                delete_recursively(&path);
            }
            let _ = std::fs::create_dir_all(&path);
            Ok(())
        })
        .await
    }

    /// D51: one step of `on_finish` that failed is logged and doesn't stop the later ones, and the
    /// first failure is what the run returns, so the exit code stays as it was. Cancellation still
    /// stops them, as the steps after it would only be cancelled too.
    fn finish_step(&self, first: &mut Option<VendorError>, step: &str, result: VendorResult<()>) -> VendorResult<()> {
        match result {
            Ok(()) => Ok(()),
            Err(VendorError::Cancelled(reason)) => Err(first.take().unwrap_or(VendorError::Cancelled(reason))),
            Err(error) => {
                self.log(
                    LOGGER,
                    Level::ERROR,
                    format!("{step} failed during cleanup"),
                    Some(error.failure().clone()),
                );
                first.get_or_insert(error);
                Ok(())
            }
        }
    }

    /// `onFinish`: the guard is set first, so a later finish attempt is a no-op even if this one
    /// failed (C25). Unlike Kotlin, a failing step doesn't skip the later ones (D51). D1 keeps the
    /// usage slot as a no-op. A signal finishes in its own order instead: see `finish_cancelled`.
    async fn on_finish(&self, cancellation: Arc<dyn CancellationSignal>) -> VendorResult<()> {
        if self.cleaned_up.replace(true) {
            return Ok(());
        }
        let mut first = None;
        // Console order: records logged so far precede the reporters' stdout summary.
        let flushed = self.flush(cancellation.clone()).await;
        self.finish_step(&mut first, "Console output", flushed)?;
        for step in [FinishStep::Analytics, FinishStep::Provider] {
            let result = self.run_step(step, cancellation.clone()).await;
            self.finish_step(&mut first, step.label(), result)?;
        }
        // Termination logs are enqueued by vendor tasks. Kotlin prints them
        // synchronously before Allure and the stdout summary close.
        let flushed = self.flush(cancellation.clone()).await;
        self.finish_step(&mut first, "Console output", flushed)?;
        for step in [FinishStep::Reports, FinishStep::Usage] {
            let result = self.run_step(step, cancellation.clone()).await;
            self.finish_step(&mut first, step.label(), result)?;
        }
        let flushed = self.flush(cancellation).await;
        self.finish_step(&mut first, "Console output", flushed)?;
        first.map_or(Ok(()), Err)
    }

    /// One step, or if it has started already, the wait for it. The caller that ran it gets its result and reports it.
    async fn run_step(&self, step: FinishStep, cancellation: Arc<dyn CancellationSignal>) -> VendorResult<()> {
        let steps = &self.steps;
        let state = match step {
            FinishStep::Analytics => &steps.analytics,
            FinishStep::Provider => &steps.provider,
            FinishStep::Reports => &steps.reports,
            FinishStep::Usage => &steps.usage,
        };
        if state.started.replace(true) {
            let _ = state.done.wait_in(Signal::new()).await;
            return Ok(());
        }
        let vendor = self.services.core.vendor();
        let result = match step {
            FinishStep::Analytics => {
                let analytics = self.deps.analytics.clone();
                call_vendor(vendor.as_ref(), cancellation, move |_| {
                    Box::pin(async move {
                        crate::wiring::MetricsProvider::close(analytics.as_ref())
                            .await
                            .map_err(vendor_error)
                    })
                })
                .await
            }
            FinishStep::Provider => {
                let provider = self.deps.provider.clone();
                call_vendor(vendor.as_ref(), cancellation, move |_| {
                    Box::pin(async move { provider.terminate().await })
                })
                .await
            }
            FinishStep::Reports => {
                let reports = self.deps.reports.clone();
                call_vendor(vendor.as_ref(), cancellation, move |_| {
                    Box::pin(async move { reports.close().await.map_err(vendor_error) })
                })
                .await
            }
            FinishStep::Usage => {
                let usage = self.deps.usage.clone();
                call_vendor(vendor.as_ref(), cancellation, move |_| {
                    Box::pin(async move { usage.close().await })
                })
                .await
            }
        };
        state.done.complete(Ok(()));
        result
    }

    /// A step of a cancelled run: its own task, so that a slow one holds up nothing else.
    fn spawn_step(self: &Rc<Self>, step: FinishStep) -> JobRef {
        let marathon = self.clone();
        self.services.core.spawn_cleanup(Box::new(move |_| {
            Box::pin(async move {
                let result = marathon.run_step(step, Signal::new()).await;
                let _ = marathon.finish_step(&mut None, step.label(), result);
                Ok(())
            })
        }))
    }

    /// D52, and what makes a finish safe here (U161): the actors stop before anything closes. Left running, they'd hand
    /// out tests to a closed adb client, whose batches end at once and quietly (N19), in a loop that takes no time and
    /// adds INCOMPLETE results to the reports. With the queue gone, a test in flight can't be recorded behind the
    /// reports' back either (A99). Cancelling also starts closing what the batches in flight have open.
    fn stop_execution(&self, reason: &str) {
        // Out of the cells first: a cancel can complete a job, and run what it left behind, right here.
        let jobs = [self.scheduler_job.borrow().clone(), self.run_job.borrow().clone()];
        for job in jobs.into_iter().flatten() {
            job.cancel(requested(reason));
        }
    }

    /// D52: a cancel writes what it can. The reports go first and the rest side by side after them, the opposite of
    /// `on_finish`'s order, since CI may kill the process at any moment and results are what can't be had again. An
    /// outer timer bounds all this: a limit built on cancellation would stop the steps (`finish_step`).
    ///
    /// Reports wait for a run that has wiped its output directory. Before that there are no results, and the directory
    /// still holds the last run's reports, which empty ones would overwrite. Kotlin's hook overwrites them.
    async fn finish_cancelled(self: &Rc<Self>, number: i32, task: &TaskContext) {
        // At ERROR because this logger shows nothing below that without `debug: true`, and the line must reach a CI log.
        self.log(
            LOGGER,
            Level::ERROR,
            format!(
                "Signal {number}: stopping the tests, writing the reports and cleaning up for up to {} s. A second signal exits at once",
                (CANCEL_LIMIT_MILLIS + CANCEL_FLUSH_MILLIS) / 1000
            ),
            None,
        );
        self.cleaned_up.set(true);
        self.stop_execution("Cancelled by a signal");
        let _ = race(
            self.flush(Signal::new()),
            task.clock.delay(CANCEL_FLUSH_MILLIS, task.job.clone()),
        )
        .await;
        let reports = self.output_ready.get().then(|| self.spawn_step(FinishStep::Reports));
        if let Some(reports) = &reports {
            let _ = race(
                reports.join(),
                task.clock.delay(CANCEL_REPORTS_HEAD_START_MILLIS, task.job.clone()),
            )
            .await;
        }
        let rest = [
            self.spawn_step(FinishStep::Provider),
            self.spawn_step(FinishStep::Analytics),
            self.spawn_step(FinishStep::Usage),
        ];
        for job in rest.iter().chain(&reports) {
            let _ = job.join().await;
        }
    }

    /// `run`: install the hook, run, uninstall only after a successful return.
    /// An exceptional child completion runs a protected finish before the catch maps the
    /// throwable: NoDevices to false, anything else to `ignoreFailures`. `Err` is left for
    /// console/clock failures of this wrapper itself.
    pub async fn run(self: &Rc<Self>, command: ExecutionCommand) -> OperationResult<bool> {
        self.run_under(command, None).await
    }
    /// `run` with its `runBlocking` root under `parent`, so a harness can tear it down.
    pub async fn run_under(
        self: &Rc<Self>,
        command: ExecutionCommand,
        parent: Option<JobRef>,
    ) -> OperationResult<bool> {
        let core = self.services.core.clone();
        let root = core.job(JobKind::Ordinary, parent);
        let relay = self
            .services
            .signals
            .clone()
            .map(|relay| self.start_relay(relay, &root));
        #[cfg(unix)]
        let diagnostics = self.start_diagnostics(&root);
        let pump = self.services.live_output.then(|| self.start_output_pump(&root));
        let supervisor = core.job(JobKind::Supervisor, Some(root.clone()));
        let result: Rc<RefCell<Option<VendorResult<bool>>>> = Rc::new(RefCell::new(None));
        let output = result.clone();
        let marathon = self.clone();
        let child = core.spawn(
            supervisor.clone(),
            Box::new(move |task: TaskContext| {
                Box::pin(async move {
                    let hook = ShutdownHook {
                        installed: marathon.hook_installed.clone(),
                    };
                    hook.install();
                    let value = marathon.run_async(&command, &task).await;
                    match &value {
                        Ok(_) => {
                            hook.uninstall();
                        }
                        // A failing `async` cancels its children (the Scheduler's subscription
                        // outlives a NoDevices timeout) before it completes.
                        Err(error) => {
                            for child in task.job.children_snapshot() {
                                child.cancel(Cancellation::Requested(error.failure().clone()));
                            }
                        }
                    }
                    // The outcome travels in the cell, as async's Deferred holds it: no
                    // unhandled-exception report for the child.
                    *output.borrow_mut() = Some(value);
                    Ok(())
                })
            }),
        );
        *self.run_job.borrow_mut() = Some(child.clone());
        let joined = child.join().await;
        Self::await_signal_exit(&relay).await;
        let outcome = match result.take() {
            Some(value) => value,
            None => Err(VendorError::Cancelled(match (joined, child.state()) {
                (Err(reason), _) | (_, JobState::Completed(Completion::Cancelled(reason))) => reason,
                _ => requested("Marathon run was cancelled"),
            })),
        };
        if outcome.is_err() {
            // invokeOnCompletion { runBlocking { withContext(NonCancellable) { onFinish() } } }.
            // A failing finish there is reported by the completion handler, not rethrown:
            // the original throwable still reaches the catch.
            let marathon = self.clone();
            let cleanup = core.spawn_cleanup(Box::new(move |_| {
                Box::pin(async move {
                    let _ = marathon.on_finish(Signal::new()).await;
                    Ok(())
                })
            }));
            let _ = cleanup.join().await;
        }
        supervisor.complete();
        let mapped = match outcome {
            Ok(value) => value,
            Err(error) => {
                // `log.error(th) {}`: the empty lambda's Unit is the message.
                self.log(
                    LOGGER,
                    Level::ERROR,
                    "kotlin.Unit".into(),
                    Some(error.failure().clone()),
                );
                match error {
                    VendorError::NoDevices(_) => {
                        self.log(LOGGER, Level::WARN, "No devices found".into(), None);
                        false
                    }
                    _ => self.deps.vendor.configuration.ignore_failures,
                }
            }
        };
        let mut flushed = self.flush(Signal::new()).await;
        if let Some(pump) = pump {
            // The flush above waited for any batch the pump had in flight.
            pump.cancel(requested("run finished"));
            let _ = pump.join().await;
            // A batch the pump took after the flush above is awaited here, and its failure
            // reported as that flush would have.
            if flushed.is_ok() {
                flushed = self.flush(Signal::new()).await;
            }
        }
        Self::await_signal_exit(&relay).await;
        if let Some((relay, job, _)) = relay {
            relay.stop();
            job.cancel(requested("run finished"));
            let _ = job.join().await;
        }
        #[cfg(unix)]
        if let Some(diagnostics) = diagnostics {
            diagnostics.cancel(requested("run finished"));
            let _ = diagnostics.join().await;
        }
        self.diagnostic_scheduler.borrow_mut().take();
        root.complete();
        flushed.map_err(|e| match e {
            VendorError::Cancelled(reason) => OperationError::Cancelled(reason),
            other => OperationError::Failed(other.failure().clone()),
        })?;
        Ok(mapped)
    }

    #[cfg(unix)]
    fn start_diagnostics(self: &Rc<Self>, root: &JobRef) -> Option<JobRef> {
        let requests = crate::diagnostics::requests()?;
        // Configuration-time and previous-run signals need no deferred snapshot.
        requests.discard();
        let marathon = self.clone();
        Some(self.services.core.spawn(
            root.clone(),
            Box::new(move |task| {
                Box::pin(async move {
                    let signal = task.job.cancellation_signal();
                    loop {
                        if let futures_util::future::Either::Left(_) = race(signal.cancelled(), requests.next()).await {
                            break;
                        }
                        let dump = crate::diagnostics::capture(|| {
                            let mut dump = crate::diagnostics::header();
                            dump.push_str(if marathon.cleaned_up.get() {
                                "run=finishing\n"
                            } else {
                                "run=active\n"
                            });
                            if let Some(scheduler) = marathon.diagnostic_scheduler.borrow().as_ref() {
                                scheduler.diagnostic(&mut dump);
                            } else {
                                dump.push_str("scheduler=not-started (configuration, preparation or parsing)\n");
                            }
                            crate::diagnostics::bound(&mut dump);
                            dump
                        });
                        marathon.log(crate::diagnostics::TARGET, Level::WARN, dump, None);
                        // A burst of diagnostic requests must let execution and D52's timer run.
                        tokio::task::yield_now().await;
                    }
                    Ok(())
                })
            }),
        ))
    }

    /// Live runs only. Kotlin's console appender prints each record as it is logged; here
    /// vendor tasks and core push to the shared queue and this task flushes it as soon as
    /// something arrives, through the same `OutputDriver` as the serial flush points. Taking
    /// each batch on core and chaining its write keeps the output order, and the write itself
    /// runs on the vendor runtime, so core never blocks. Records that arrive while a write is
    /// in flight go out together in the next batch. The serial owners keep their flush points,
    /// which now also wait for the pump's batch (`LogQueue::serial_output`).
    fn start_output_pump(self: &Rc<Self>, root: &JobRef) -> JobRef {
        let core = self.services.core.clone();
        let queue = core.log_queue();
        queue.set_concurrent_output();
        let job = core.job(JobKind::Ordinary, Some(root.clone()));
        let marathon = self.clone();
        core.spawn(
            job.clone(),
            Box::new(move |task: TaskContext| {
                Box::pin(async move {
                    let wake = queue.wake();
                    let signal = task.job.cancellation_signal();
                    loop {
                        let cancelled = signal.cancelled();
                        let arrived = wake.notified();
                        futures_util::pin_mut!(cancelled, arrived);
                        if let futures_util::future::Either::Left(_) =
                            futures_util::future::select(cancelled, arrived).await
                        {
                            break;
                        }
                        // A failed write is left in `pump_error` by the write itself (see
                        // `flush_recording`); the next serial flush reports it.
                        let flushed = marathon
                            .services
                            .output
                            .flush_recording(&queue, signal.clone(), marathon.pump_error.clone())
                            .await;
                        match flushed {
                            Ok(()) => {}
                            Err(VendorError::Cancelled(_)) => break,
                            // The vendor task itself failed, not just the sink write.
                            Err(error) => {
                                if let Ok(mut slot) = marathon.pump_error.lock() {
                                    slot.get_or_insert(error);
                                }
                            }
                        }
                    }
                    Ok(())
                })
            }),
        );
        job
    }
    /// The hook's `runBlocking { onFinish() }` on core, in D52's order and bounded, then the JVM's exit status.
    fn start_relay(self: &Rc<Self>, relay: Rc<SignalRelay>, root: &JobRef) -> RunningRelay {
        let core = self.services.core.clone();
        // Under the run root, so tearing the run down (a harness, never the
        // production path after a signal) also ends the hook's final wait.
        let job = core.job(JobKind::Ordinary, Some(root.clone()));
        let marathon = self.clone();
        let waiting = relay.clone();
        relay.listen();
        let hook = core.spawn(
            job.clone(),
            Box::new(move |task: TaskContext| {
                Box::pin(async move {
                    let number = match waiting.signal.wait_in(task.job.cancellation_signal()).await {
                        Ok(number) => *number,
                        Err(VendorError::Cancelled(reason)) => return Err(reason.into()),
                        Err(other) => return Err(other.failure().clone().into()),
                    };
                    if marathon.hook_installed.get() {
                        let finished = race(
                            marathon.finish_cancelled(number, &task),
                            task.clock.delay(CANCEL_LIMIT_MILLIS, task.job.clone()),
                        )
                        .await;
                        if let futures_util::future::Either::Right(_) = finished {
                            marathon.log(
                                LOGGER,
                                Level::ERROR,
                                format!("Cleanup did not finish in {CANCEL_LIMIT_MILLIS} ms; exiting anyway"),
                                None,
                            );
                        }
                    }
                    let _ = race(
                        marathon.flush(Signal::new()),
                        task.clock.delay(CANCEL_FLUSH_MILLIS, task.job.clone()),
                    )
                    .await;
                    (waiting.exit)(128 + number);
                    // `exit` owns the process's end from here: it queues the final
                    // flush and the process exits from the console's writer/watchdog
                    // thread. This hook must never complete — if it did, `run` would
                    // proceed past `await_signal_exit` to its own cleanup and result,
                    // racing that exit with an ordinary failure return. Waiting on a
                    // deferred nobody completes parks the task without blocking core
                    // and without a timer (so it holds on every clock, driven or
                    // not); only cancelling this job — a harness tearing the run
                    // down — ends it.
                    let never = SharedDeferred::<()>::new();
                    let _ = never.wait_in(task.job.cancellation_signal()).await;
                    Ok(())
                })
            }),
        );
        (relay, job, hook)
    }
    /// After a signal the process's end is the hook's, so the run neither stops the relay under it nor returns a result
    /// of its own. Once the hook has queued the exit it never completes, so in production this does not return; the
    /// process ends from the exit path the hook queued (or a second signal's immediate exit).
    async fn await_signal_exit(relay: &Option<RunningRelay>) {
        if let Some((relay, _, hook)) = relay
            && relay.fired()
        {
            let _ = hook.join().await;
        }
    }
}

/// One stage of the empty-run warning, naming the filter as the Marathonfile's `type` does.
fn removed_by(list: &str, config: &TestFilterConfiguration, removed: usize) -> String {
    use TestFilterConfiguration as F;
    let kind = match config {
        F::SimpleClassname(_) => "simple-class-name",
        F::SimpleTestname(_) => "simple-test-name",
        F::FullyQualifiedClassname(_) => "fully-qualified-class-name",
        F::FullyQualifiedTestname(_) => "fully-qualified-test-name",
        F::TestMethod(_) => "method",
        F::Annotation(_) => "annotation",
        F::TestPackage(_) => "package",
        F::AnnotationData { .. } => "annotationData",
        F::Fragmentation { .. } => "fragmentation",
        F::Composition { .. } => "composition",
        F::Allure { .. } => "allure",
    };
    format!("{list} {kind} removed {removed}")
}

/// Kotlin's `File.deleteRecursively` keeps going after a failure and ignores the result.
/// Unlike its walk, this does not descend into symlinked directories (C68).
fn delete_recursively(path: &std::path::Path) {
    let is_dir = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
    if is_dir {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                delete_recursively(&entry.path());
            }
        }
        let _ = std::fs::remove_dir(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}
