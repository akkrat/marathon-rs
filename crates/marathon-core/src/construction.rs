//! Port of core/src/main/kotlin/com/malinskiy/marathon/di/Modules.kt
//!
//! The per-run graph `marathonStartKoin` + `koin.get<Marathon>()` builds, in Koin's
//! resolution order: Track and Timer (vendor providers inject them), the vendor's
//! provider, bundle identifier, parser and log configurator, then TrackerFactory (file
//! manager, reporters, optional remote backend, MappingTracker) and AnalyticsFactory, then
//! the parse command. Vendor, analytics and file work runs on the vendor runtime; the
//! concrete executor, console driver and contexts stay on core (`RunServices`).

use crate::{
    analytics::{
        BackendFactory, BackendRegistration, BackendRequest, MetricsContext, MetricsProviderFactory, MetricsSetup,
        ReporterRegistration, Track, TrackerFactory,
    },
    artifact_io::{ServiceLog, blocking, on_io, vendor_error},
    artifacts::{FileManagerOutput, LocalAttachmentFiles},
    ci::Ci,
    error::*,
    execution::parse_command::MarathonTestParseCommand,
    executor::{CoreExecutor, shared::*},
    filter::{
        AnnotationRenderer, FilterDependencies,
        allure::{FileTestPlanSupplier, ProcessEnvironment},
        values::{FileValuesReader, FilterLogger},
    },
    logging::{LogRecord, LogSender, WallSource},
    marathon::{Marathon, RunServices, SignalHooks, SignalRelay},
    output::{ConsoleSink, OutputDriver},
    report::{
        BillingReporter, DeviceInfoJsonReporter, JUnitReporter, RawJsonReporter, ReporterFiles, StdoutReporter,
        TestJsonReporter, reporter_registrations,
    },
    reporters::{allure::AllureReporter, html::HtmlSummaryReporter, timeline::TimelineReporter},
    runtime::*,
    time::{MonotonicClock, ReplayBarriers, ReplayHooks, SystemTimer, Timer, WallClock},
    vendor::VendorServices,
    wiring::{RunDependencies, UsageLifecycle, VendorDependencies},
};
use jcompat::{
    file_manager::{FileManager, child_file},
    logging::{Level, Logger, MarathonLogging},
};
use marathon_config::{AnalyticsConfiguration, Configuration};
use std::path::PathBuf;
use std::{
    rc::Rc,
    sync::{Arc, Mutex},
};

pub type VendorFactoryFn = Box<dyn FnOnce(VendorDependencies) -> VendorFuture<'static, VendorServices> + Send>;
/// The analytics clients (3.4b) behind core's injected seams: one `MetricsSetup` and,
/// for remote configurations, the `BackendFactory` sharing its clients.
pub struct MetricsBackends {
    pub setup: Arc<dyn MetricsSetup>,
    pub backend: Option<Arc<dyn BackendFactory>>,
}
pub type MetricsFactoryFn = Box<dyn FnOnce(MetricsContext, Arc<dyn VendorRuntime>) -> MetricsBackends + Send>;
/// A pre-built provider in place of AnalyticsFactory, as tests that construct Marathon
/// directly pass `Analytics(MetricsProviderImpl(...))` (StartupTimingsTest).
pub type AnalyticsOverride = Box<dyn FnOnce(MetricsContext) -> OperationResult<crate::analytics::Analytics> + Send>;
/// Reporters built from the `ReporterContext`: `standard_reporters` (Timeline, Allure, Html),
/// plus whatever a test injects.
pub type ExtraReporters = Box<dyn FnOnce(&ReporterContext) -> VendorResult<Vec<ReporterRegistration>> + Send>;

/// TrackerFactory's Timeline, Allure and HtmlSummary reporters (their slots fix the
/// generation order among the other six).
pub fn standard_reporters(ctx: &ReporterContext) -> VendorResult<Vec<ReporterRegistration>> {
    let output_dir = ctx.configuration.output_dir.clone();
    let timeline = {
        let mut registry = ctx.logging.lock().unwrap_or_else(|e| e.into_inner());
        TimelineReporter::new(ctx.io.clone(), output_dir.clone(), &mut registry)
    };
    let allure = AllureReporter::new(
        ctx.io.clone(),
        ctx.configuration.clone(),
        child_file(&output_dir, "allure-results"),
        ctx.cwd.clone(),
        ctx.hooks.clone(),
        Arc::new(LocalAttachmentFiles { io: ctx.io.clone() }),
        ctx.stderr.clone(),
        ctx.console.clone(),
    );
    let html = HtmlSummaryReporter::new(
        ctx.io.clone(),
        ctx.files.clone(),
        ctx.hooks.clone(),
        ctx.configuration.name.clone(),
        ctx.cwd.clone(),
    );
    Ok(vec![
        timeline.registration(),
        allure.registration(),
        html.registration(),
    ])
}

/// Everything the reporters need.
pub struct ReporterContext {
    pub io: Arc<dyn VendorRuntime>,
    pub configuration: Arc<Configuration>,
    pub files: Arc<FileManagerOutput>,
    pub cwd: PathBuf,
    pub hooks: Arc<dyn ReplayHooks>,
    pub console: Arc<dyn ConsoleSink>,
    pub stderr: Arc<dyn Fn(&Failure) + Send + Sync>,
    pub logging: Arc<Mutex<MarathonLogging>>,
}

pub struct RunInputs {
    pub configuration: Arc<Configuration>,
    /// The per-process registry the CLI's startup lines used.
    pub logging: Arc<Mutex<MarathonLogging>>,
    /// stdout: log records and raw println text, through the shared console.
    pub console: Arc<dyn ConsoleSink>,
    /// stderr for throwables the source prints rather than logs (Allure's environment writer).
    pub stderr: Arc<dyn Fn(&Failure) + Send + Sync>,
    /// `user.dir`.
    pub cwd: PathBuf,
    pub environment: Vec<(String, String)>,
    pub system_information: Vec<(String, String)>,
    pub vendor: VendorFactoryFn,
    pub metrics: MetricsFactoryFn,
    pub analytics: Option<AnalyticsOverride>,
    pub extra_reporters: ExtraReporters,
    pub hooks: Arc<dyn ReplayHooks>,
    pub barriers: Arc<dyn ReplayBarriers>,
    /// `MarathonFactory.timer`; `SystemTimer(Clock)` otherwise.
    pub timer: Option<Arc<dyn Timer>>,
    /// Wall time stamped on log records.
    pub wall: WallSource,
    /// Exit function for the P3 signal relay; `None` installs no relay.
    pub exit: Option<Arc<dyn Fn(i32) + Send + Sync>>,
    /// With `exit`: a manual trigger in place of the process's signals (tests).
    pub signal_trigger: Option<Arc<SharedDeferred<i32>>>,
    /// Test seams for the OS relay; the default changes nothing.
    pub signal_hooks: SignalHooks,
    /// Live runs print each queued record promptly, as logback does, rather than only at the
    /// core's flush points. Replay and oracle runs keep those points and leave this false.
    pub live_output: bool,
}

/// D1: usage analytics are gone; onFinish keeps its final slot.
pub struct NoUsage;
impl UsageLifecycle for NoUsage {
    fn close(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Koin's resolution failure for a type the vendor module does not define, as the JVM
/// reports it from `koin.get<Marathon>()` (oracle rows `koin-missing-*`).
pub fn missing_definition(class: &str) -> Failure {
    let mut message = String::from("No definition found for type '");
    message.push_str(class);
    message.push_str("'. Check your Modules configuration and add missing type and/or qualifier!");
    Failure {
        class: "org.koin.core.error.InstanceCreationException".into(),
        message: Some("Could not create instance for '[Singleton:'com.malinskiy.marathon.Marathon']'".into()),
        cause: Some(Box::new(Failure {
            class: "org.koin.core.error.NoBeanDefFoundException".into(),
            message: Some(message),
            cause: None,
            stacktrace: None,
        })),
        stacktrace: None,
    }
}

struct RecordLog {
    logs: LogSender,
    stamp: Arc<dyn Fn() -> (i64, i64) + Send + Sync>,
}
impl RecordLog {
    fn push(&self, logger: &str, level: Level, message: String, cause: Option<Failure>) {
        let (at_millis, wall_millis) = (self.stamp)();
        self.logs.push(LogRecord {
            logger: logger.to_owned(),
            at_millis,
            wall_millis,
            level,
            message,
            cause,
        });
    }
}
impl FilterLogger for RecordLog {
    fn log(&self, level: Level, logger: &str, message: String, cause: Option<Failure>) {
        self.push(logger, level, message, cause);
    }
}

fn registry_logger(logging: &Mutex<MarathonLogging>, name: &str) -> VendorResult<Logger> {
    Ok(logging
        .lock()
        .map_err(|_| VendorError::Other(failure("java.lang.IllegalStateException", "Poisoned logging registry")))?
        .logger(name, None))
}

struct Built {
    vendor: VendorDependencies,
    services: VendorServices,
    parser: crate::vendor::TestParser,
    log_configurator: Arc<dyn crate::vendor::LogConfigurator>,
    analytics: Arc<crate::analytics::Analytics>,
    reports: Arc<dyn crate::wiring::ReportLifecycle>,
    parse_output: Arc<dyn crate::wiring::ParseOutput>,
}

/// `marathonStartKoin(configuration, modules)` + `koin.get<Marathon>()`. An error here
/// precedes `marathon.run` and its catch (the CLI's uncaught path).
pub async fn construct(core: Rc<CoreExecutor>, inputs: RunInputs) -> Result<Rc<Marathon>, Failure> {
    let io = core.vendor();
    let wall = inputs.wall.clone();
    core.log_queue().set_wall_clock(Rc::new(move || wall()));
    let now = core.clock().now_source();
    let wall = inputs.wall.clone();
    let stamp: Arc<dyn Fn() -> (i64, i64) + Send + Sync> = Arc::new(move || (now(), wall()));
    let record = Arc::new(RecordLog {
        logs: core.log_queue().sender(),
        stamp: stamp.clone(),
    });
    let monotonic: Arc<dyn MonotonicClock> = inputs.hooks.clone();
    let context = MetricsContext {
        clock: core.vendor_clock(),
        vendor: core.vendor(),
        monotonic,
        cancellation: Signal::new(),
        registry: inputs.logging.clone(),
        logs: core.log_queue().sender(),
        stamp,
    };
    let filters = FilterDependencies {
        values: Arc::new(FileValuesReader { logger: record.clone() }),
        allure: Arc::new(FileTestPlanSupplier {
            environment: Arc::new(ProcessEnvironment),
            logger: record.clone(),
        }),
        rendering: Arc::new(AnnotationRenderer),
    };
    let ci = Ci::from_config(inputs.configuration.ci_configuration, &inputs.environment);
    let signals = match (inputs.exit.clone(), inputs.signal_trigger.clone()) {
        (Some(exit), Some(trigger)) => Some(SignalRelay::manual(trigger, exit)),
        (Some(exit), None) => Some(os_relay(io.clone(), exit, inputs.signal_hooks.clone())?),
        (None, _) => None,
    };
    let console = inputs.console.clone();
    let live_output = inputs.live_output;
    let system_information = inputs.system_information.clone();
    let vendor_clock = core.vendor_clock();
    let built = on_io(
        io.as_ref(),
        Box::pin(build(io.clone(), vendor_clock, record, context, inputs)),
    )
    .await
    .map_err(|e| e.failure().clone())?;
    let deps = RunDependencies {
        vendor: built.vendor,
        provider: built.services.provider,
        parser: built.parser,
        bundle_identifier: built.services.bundle_identifier,
        log_configurator: built.log_configurator,
        analytics: built.analytics,
        reports: built.reports,
        usage: Arc::new(NoUsage),
        parse_output: built.parse_output,
        core: core.clone(),
        scheduler_clock: core.clock(),
        timeouts: core.timeouts(),
    };
    let services = RunServices {
        output: OutputDriver {
            vendor: io,
            sink: console,
        },
        core,
        ci,
        filters,
        system_information,
        signals,
        live_output,
    };
    Ok(Marathon::new(deps, services))
}

#[cfg(unix)]
fn os_relay(
    io: Arc<dyn VendorRuntime>,
    exit: Arc<dyn Fn(i32) + Send + Sync>,
    hooks: SignalHooks,
) -> Result<Rc<SignalRelay>, Failure> {
    Ok(SignalRelay::os(io, exit, hooks))
}
#[cfg(not(unix))]
fn os_relay(
    _: Arc<dyn VendorRuntime>,
    _: Arc<dyn Fn(i32) + Send + Sync>,
    _: SignalHooks,
) -> Result<Rc<SignalRelay>, Failure> {
    Err(failure(
        "java.lang.UnsupportedOperationException",
        "signals need a Unix host",
    ))
}

async fn build(
    io: Arc<dyn VendorRuntime>,
    clock: Arc<dyn VendorClock>,
    record: Arc<RecordLog>,
    context: MetricsContext,
    inputs: RunInputs,
) -> VendorResult<Built> {
    let configuration = inputs.configuration.clone();
    let hooks = inputs.hooks.clone();
    let wall: Arc<dyn WallClock> = hooks.clone();
    let track = Arc::new(Track::new(io.clone(), wall.clone()));
    let timer: Arc<dyn Timer> = match inputs.timer.clone() {
        Some(timer) => timer,
        None => Arc::new(SystemTimer::new(wall.clone()).map_err(crate::artifact_io::clock_error)?),
    };
    let log: ServiceLog = {
        let record = record.clone();
        Arc::new(move |logger: &Logger, level, message: &str, cause: Option<&Failure>| {
            record.push(logger.name(), level, message.to_string(), cause.cloned());
        })
    };
    let logging = inputs.logging.clone();
    let registry = logging.clone();
    let file_logger = blocking(io.as_ref(), move || registry_logger(&registry, "FileManager")).await?;
    let output_dir = configuration.output_dir.clone();
    let manager = FileManager::new(
        configuration.output_configuration.max_path,
        configuration.output_configuration.max_filename,
        &output_dir.to_string_lossy(),
        &inputs.cwd.to_string_lossy(),
    );
    let files = Arc::new(FileManagerOutput::new(
        io.clone(),
        manager,
        output_dir.clone(),
        file_logger,
        log.clone(),
    ));
    let vendor = VendorDependencies {
        logs: record.logs.clone(),
        wall: inputs.wall.clone(),
        configuration: configuration.clone(),
        io: io.clone(),
        clock,
        logging: logging.clone(),
        log: log.clone(),
        hooks: hooks.clone(),
        timer: timer.clone(),
        tracker: track.clone(),
        files: files.clone(),
        barriers: inputs.barriers.clone(),
    };
    let services = (inputs.vendor)(vendor.clone()).await?;
    // Marathon's constructor resolves the bundle identifier, parser and log configurator
    // before the tracker; the first missing definition fails construction.
    let parser = services
        .parser
        .clone()
        .ok_or_else(|| VendorError::Other(missing_definition("com.malinskiy.marathon.execution.TestParser")))?;
    let log_configurator = services
        .log_configurator
        .clone()
        .ok_or_else(|| VendorError::Other(missing_definition("com.malinskiy.marathon.log.MarathonLogConfigurator")))?;

    let reporter_files = ReporterFiles {
        io: io.clone(),
        files: files.clone(),
        cwd: inputs.cwd.clone(),
    };
    let registry = logging.clone();
    let (bill_logger, factory_logger) = blocking(io.as_ref(), move || {
        Ok((
            registry_logger(&registry, "com.malinskiy.marathon.report.bill.BillingReporter")?,
            registry_logger(&registry, "TrackerFactory")?,
        ))
    })
    .await?;
    let bill =
        BillingReporter::new(reporter_files.clone(), wall.as_ref(), bill_logger, log.clone()).map_err(vendor_error)?;
    let mut reporters = reporter_registrations(
        DeviceInfoJsonReporter::new(reporter_files.clone()),
        bill,
        JUnitReporter::new(io.clone(), output_dir.clone(), inputs.cwd.clone()),
        RawJsonReporter::new(reporter_files.clone()),
        TestJsonReporter::new(reporter_files.clone()),
        StdoutReporter::new(io.clone(), timer.clone(), inputs.console.clone()),
    );
    let reporter_context = ReporterContext {
        io: io.clone(),
        configuration: configuration.clone(),
        files,
        cwd: inputs.cwd.clone(),
        hooks: hooks.clone(),
        console: inputs.console.clone(),
        stderr: inputs.stderr.clone(),
        logging: logging.clone(),
    };
    let extra = inputs.extra_reporters;
    // The reporter constructors register loggers: keep the registry work on the blocking pool.
    reporters.extend(blocking(io.as_ref(), move || extra(&reporter_context)).await?);

    let metrics = (inputs.metrics)(context.clone(), io.clone());
    let request = match &configuration.analytics_configuration {
        AnalyticsConfiguration::Disabled => None,
        AnalyticsConfiguration::InfluxDb(c) => Some(BackendRequest::Influx { url: c.url.clone() }),
        AnalyticsConfiguration::InfluxDb2(c) => Some(BackendRequest::Influx { url: c.url.clone() }),
        AnalyticsConfiguration::Graphite(c) => Some(BackendRequest::Graphite { port: c.port }),
    };
    let backend = match (request, metrics.backend) {
        (Some(request), Some(factory)) => Some(BackendRegistration { request, factory }),
        _ => None,
    };
    let pipeline = TrackerFactory {
        io: io.clone(),
        track,
        clock: wall,
        reporters,
        backend,
        logger: factory_logger,
        log,
    }
    .create()
    .await
    .map_err(vendor_error)?;
    let analytics = match inputs.analytics {
        Some(analytics) => analytics(context.clone()).map_err(vendor_error)?,
        None => MetricsProviderFactory::new(context.clone(), metrics.setup)
            .map_err(vendor_error)?
            .create(&configuration)
            .await
            .map_err(vendor_error)?,
    };
    let parse_output = Arc::new(MarathonTestParseCommand::new(
        io.clone(),
        output_dir,
        inputs.cwd.clone(),
        &context,
    ));
    Ok(Built {
        vendor,
        parser,
        log_configurator,
        services,
        analytics: Arc::new(analytics),
        reports: pipeline.reports(),
        parse_output,
    })
}
