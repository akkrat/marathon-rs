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
use jcompat::{logging::Level, writers::JavaString};
use marathon_config::{binding::ConfigurationError, loader::LogicalConfigurationValidator};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

/// `private val log = MarathonLogging.logger {}` in Marathon.kt: kotlin-logging drops the `Kt` suffix.
pub const LOGGER: &str = "com.malinskiy.marathon.Marathon";
/// `TimingLog.LOGGER_NAME`.
pub const TIMINGS: &str = "Timings";
const NO_DEVICES: &str = "com.malinskiy.marathon.exceptions.NoDevicesException";
const NO_TESTS: &str = "com.malinskiy.marathon.exceptions.NoTestCasesFoundException";

/// `TimingLog.info(timer) { message }`'s text: the message, then the time since start.
pub fn timing_message(message: &str, timer: &dyn crate::time::Timer) -> crate::time::ClockResult<JavaString> {
    let elapsed = timer.elapsed_time_millis()?;
    Ok(format!("{message} (+{elapsed} ms since start)").into())
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
    pub system_information: Vec<(JavaString, JavaString)>,
    pub signals: Option<Rc<SignalRelay>>,
    /// Flush the log queue as records arrive (live runs only; see `start_output_pump`).
    pub live_output: bool,
}

/// One registered signal: completes on its next delivery.
pub type SignalReceipt = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
/// Installs the handler for a signal number; runs on the vendor runtime.
pub type SignalRegistration = Arc<dyn Fn(i32) -> std::io::Result<SignalReceipt> + Send + Sync>;

/// Test seams for the OS relay. The default registers through tokio and announces nothing.
#[derive(Clone, Default)]
pub struct SignalHooks {
    /// Replaces the tokio registration (failure injection).
    pub registration: Option<SignalRegistration>,
    /// Called once both registrations have been attempted.
    pub ready: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// The tokio registration of `number`: the handler stays installed for the process's life.
#[cfg(unix)]
pub fn tokio_registration(number: i32) -> std::io::Result<SignalReceipt> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut stream = signal(SignalKind::from_raw(number))?;
    Ok(Box::pin(async move {
        if stream.recv().await.is_none() {
            std::future::pending::<()>().await;
        }
    }))
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
        let listener = vendor.spawn_cancellable(Box::new(move |cancelled| {
            Box::pin(async move {
                let receipts: Vec<_> = [2, 15]
                    .into_iter()
                    .filter(|&number| !signal_ignored(number))
                    .filter_map(|number| {
                        let receipt = registration(number).ok()?;
                        Some(Box::pin(async move {
                            receipt.await;
                            number
                        }))
                    })
                    .collect();
                if let Some(ready) = ready {
                    ready();
                }
                if receipts.is_empty() {
                    return Ok(());
                }
                tokio::select! {
                    (number, _, _) = futures_util::future::select_all(receipts) => { output.complete(Ok(number)); }
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
}

/// `ShutdownHook`: only `debug: true` installs it (C10). Installation and removal cannot
/// fail natively, so the swallowed IllegalStateException/SecurityException paths are gone
/// (P3); `uninstall` returns true as after a nonthrowing `removeShutdownHook`.
struct ShutdownHook {
    debug: bool,
    installed: Rc<Cell<bool>>,
}
impl ShutdownHook {
    fn install(&self) {
        if self.debug {
            self.installed.set(true);
        }
    }
    fn uninstall(&self) -> bool {
        if self.debug {
            self.installed.set(false);
        }
        true
    }
}

pub struct Marathon {
    deps: RunDependencies,
    services: RunServices,
    validator: LogicalConfigurationValidator,
    /// `cleanedUp`: one core thread, so a Cell is the AtomicBoolean.
    cleaned_up: Cell<bool>,
    hook_installed: Rc<Cell<bool>>,
    /// First failure of a live pump's write, left for the next serial flush to report.
    pump_error: PumpErrors,
}

/// A stage (b) ConfigurationError as its throwable: `causes` are outermost first.
pub fn configuration_failure(error: &ConfigurationError) -> Failure {
    let cause = error.causes.iter().rev().fold(None, |inner, c| {
        Some(Box::new(Failure {
            class: c.class.clone(),
            message: c.message.clone(),
            cause: inner,
            stacktrace: None,
        }))
    });
    Failure {
        class: error.class.clone(),
        message: error.message.clone(),
        cause,
        stacktrace: None,
    }
}

fn text(parts: &[&str]) -> JavaString {
    let mut out = JavaString::default();
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
            pump_error: PumpErrors::default(),
        })
    }
    pub fn dependencies(&self) -> &RunDependencies {
        &self.deps
    }
    pub fn services(&self) -> &RunServices {
        &self.services
    }
    /// Whether the debug-only shutdown hook is currently installed (C10).
    pub fn hook_installed(&self) -> bool {
        self.hook_installed.get()
    }

    fn log(&self, logger: &str, level: Level, message: JavaString, cause: Option<Failure>) {
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
        self.log(TIMINGS, Level::Info, line, None);
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

    /// `configureLogging()`: `MarathonLogging.debug` first, then the vendor's appenders.
    async fn configure_logging(&self, task: &TaskContext) -> VendorResult<()> {
        let debug = self.deps.vendor.configuration.debug;
        let logging = self.deps.vendor.logging.clone();
        let registry = logging.clone();
        blocking(self.services.core.vendor().as_ref(), move || {
            registry
                .lock()
                .map_err(|_| {
                    VendorError::Other(failure("java.lang.IllegalStateException", "Poisoned logging registry"))
                })?
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
                .map_err(|_| {
                    VendorError::Other(failure("java.lang.IllegalStateException", "Poisoned logging registry"))
                })?
                .timings();
            Ok(())
        })
        .await
    }

    /// `logSystemInformation()` under P1: the heading, then the documented OS facts.
    fn log_system_information(&self) {
        self.log(LOGGER, Level::Info, "System Information:".into(), None);
        for (key, value) in &self.services.system_information {
            let mut line = key.clone();
            line.push_str(": ");
            line.append(value.clone());
            self.log(LOGGER, Level::Info, line, None);
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
                Level::Info,
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
            Level::Debug,
            format!("Parsing took {} ms", parsing_finished.wrapping_sub(parsing_started)).into(),
            None,
        );
        self.flush(task.job.cancellation_signal()).await?;

        // D1: usageTracker.meta is gone.
        if parsed_all_tests.is_empty() {
            return Err(VendorError::Other(failure(NO_TESTS, "No tests cases were found")));
        }
        let parsed_filtered_tests = self.apply_test_filters(parsed_all_tests).await?;

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
                Box::pin(async move { output.execute(&tests, &flaky_tests, name.as_ref()).await })
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
        let mut names = JavaString::default();
        for (index, test) in parsed_filtered_tests.iter().enumerate() {
            if index > 0 {
                names.push_str(", ");
            }
            names.append(test.to_test_name('.', '#'));
        }
        self.log(LOGGER, Level::Debug, names, None);

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

        self.wipe_output_dir().await?;

        self.timing(|| Ok(format!("Scheduling {} tests", parsed_filtered_tests.len())))?;
        self.flush(task.job.cancellation_signal()).await?;
        let result = if parsed_filtered_tests.is_empty() {
            true
        } else {
            scheduler.execute().await?
        };

        self.on_finish(task.job.cancellation_signal()).await?;
        Ok(result)
    }

    /// `withTimeoutOrNull(deviceInitializationTimeoutMillis) { withRetry(3, 0) { borrow; extract } }`.
    /// Only this scope's own expiry becomes NoDevices; the last attempt's typed error survives.
    async fn extract_remotely(
        &self,
        parser: Arc<dyn crate::vendor::RemoteTestParser>,
        task: &TaskContext,
    ) -> VendorResult<Vec<Test>> {
        let tests: RefCell<Option<Vec<Test>>> = RefCell::new(None);
        let last: RefCell<Option<VendorError>> = RefCell::new(None);
        let millis = self.deps.vendor.configuration.device_initialization_timeout_millis;
        let (tests_cell, last_cell) = (&tests, &last);
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
                                let result = self.borrow_and_extract(parser, scope_ref).await;
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
        scope: &TaskContext,
    ) -> VendorResult<Vec<Test>> {
        let wait_started = self.elapsed()?;
        let provider = self.deps.provider.clone();
        let context = self.context(scope.job.cancellation_signal());
        let device: DeviceRef = self
            .vendor(scope, move |signal| {
                Box::pin(async move {
                    provider
                        .borrow(VendorContext {
                            cancellation: signal,
                            ..context
                        })
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
            .await?;
        self.timing(|| {
            Ok(format!(
                "Parsed {} tests in {} ms on {}, which took {} ms to become available",
                tests.len(),
                self.elapsed()?.wrapping_sub(started),
                device.0.info().serial_number.to_string_lossy(),
                started.wrapping_sub(wait_started)
            ))
        })?;
        Ok(tests)
    }

    /// Each allowlist filter, then each blocklist filter, freshly constructed per call.
    async fn apply_test_filters(&self, parsed: Vec<Test>) -> VendorResult<Vec<Test>> {
        let vendor = self.services.core.vendor();
        let filtering = &self.deps.vendor.configuration.filtering_configuration;
        let mut tests = parsed;
        for config in &filtering.allowlist {
            tests = TestFilter::from_config(config, &self.services.filters)
                .filter(tests, vendor.as_ref())
                .await
                .map_err(vendor_error)?;
        }
        for config in &filtering.blocklist {
            tests = TestFilter::from_config(config, &self.services.filters)
                .filter_not(tests, vendor.as_ref())
                .await
                .map_err(vendor_error)?;
        }
        Ok(tests)
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
        let path = output.to_file_path_buf().ok();
        let probe = path.clone();
        let exists = blocking(self.services.core.vendor().as_ref(), move || {
            Ok(probe.is_some_and(|p| p.exists()))
        })
        .await?;
        if exists {
            let mut message = text(&["Output "]);
            message.append(JavaString(output.0.clone()));
            message.push_str(" already exists");
            self.log(LOGGER, Level::Info, message, None);
        }
        blocking(self.services.core.vendor().as_ref(), move || {
            if let Some(path) = path {
                if exists {
                    delete_recursively(&path);
                }
                let _ = std::fs::create_dir_all(&path);
            }
            Ok(())
        })
        .await
    }

    /// `onFinish`: the guard is set first; a failing close skips the later slots and every
    /// later finish attempt (C25). D1 keeps the usage slot as a no-op.
    async fn on_finish(&self, cancellation: Arc<dyn CancellationSignal>) -> VendorResult<()> {
        if self.cleaned_up.replace(true) {
            return Ok(());
        }
        // Console order: records logged so far precede the reporters' stdout summary.
        self.flush(cancellation.clone()).await?;
        let vendor = self.services.core.vendor();
        let analytics = self.deps.analytics.clone();
        call_vendor(vendor.as_ref(), cancellation.clone(), move |_| {
            Box::pin(async move {
                crate::wiring::MetricsProvider::close(analytics.as_ref())
                    .await
                    .map_err(vendor_error)
            })
        })
        .await?;
        let provider = self.deps.provider.clone();
        call_vendor(vendor.as_ref(), cancellation.clone(), move |_| {
            Box::pin(async move { provider.terminate().await })
        })
        .await?;
        // Termination logs are enqueued by vendor tasks. Kotlin prints them
        // synchronously before Allure and the stdout summary close.
        self.flush(cancellation.clone()).await?;
        let reports = self.deps.reports.clone();
        call_vendor(vendor.as_ref(), cancellation.clone(), move |_| {
            Box::pin(async move { reports.close().await.map_err(vendor_error) })
        })
        .await?;
        let usage = self.deps.usage.clone();
        call_vendor(vendor.as_ref(), cancellation.clone(), move |_| {
            Box::pin(async move { usage.close().await })
        })
        .await?;
        self.flush(cancellation).await
    }

    /// `run`: install the debug-only hook, run, uninstall only after a successful return.
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
        let relay = self.services.signals.clone().map(|relay| self.start_relay(relay));
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
                        debug: marathon.deps.vendor.configuration.debug,
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
                                child.cancel(Cancellation::Parent(error.failure().clone()));
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
        let joined = child.join().await;
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
                    Level::Error,
                    "kotlin.Unit".into(),
                    Some(error.failure().clone()),
                );
                match error {
                    VendorError::NoDevices(_) => {
                        self.log(LOGGER, Level::Warn, "No devices found".into(), None);
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
        if let Some((relay, job)) = relay {
            relay.stop();
            job.cancel(requested("run finished"));
            let _ = job.join().await;
        }
        root.complete();
        flushed.map_err(|e| match e {
            VendorError::Cancelled(reason) => OperationError::Cancelled(reason),
            other => OperationError::Failed(other.failure().clone()),
        })?;
        Ok(mapped)
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
    /// The hook's `runBlocking { onFinish() }` on core, then the JVM's exit status.
    fn start_relay(self: &Rc<Self>, relay: Rc<SignalRelay>) -> (Rc<SignalRelay>, JobRef) {
        let core = self.services.core.clone();
        let job = core.job(JobKind::Ordinary, None);
        let marathon = self.clone();
        let waiting = relay.clone();
        relay.listen();
        core.spawn(
            job.clone(),
            Box::new(move |task: TaskContext| {
                Box::pin(async move {
                    let number = match waiting.signal.wait_in(task.job.cancellation_signal()).await {
                        Ok(number) => *number,
                        Err(VendorError::Cancelled(reason)) => return Err(reason.into()),
                        Err(other) => return Err(other.failure().clone().into()),
                    };
                    if marathon.hook_installed.get() {
                        // A throwing hook thread is reported by the JVM; the exit still follows.
                        let _ = marathon.on_finish(Signal::new()).await;
                    }
                    let _ = marathon.flush(Signal::new()).await;
                    (waiting.exit)(128 + number);
                    Ok(())
                })
            }),
        );
        (relay, job)
    }
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
