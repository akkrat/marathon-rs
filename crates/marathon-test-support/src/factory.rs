//! Port of vendor/vendor-test/src/main/kotlin/com/malinskiy/marathon/test/factory/ConfigurationFactory.kt,
//! test/factory/MarathonFactory.kt, test/MarathonFactoryExtensions.kt and test/Mocks.kt.
//!
//! Koin and Mockito are replaced by explicit per-test objects (P2): the parser stub returns
//! the configured tests, the bundle identifier and log configurator are inert, and the
//! graph is the production `construction::construct` with the Stub vendor factory.
use crate::{console::RecordingConsole, stub_bundle::MockTestBundleIdentifier, stub_provider::StubDeviceProvider};
use jcompat::logging::MarathonLogging;
use marathon_config::{
    Configuration, VendorConfiguration,
    binding::{BindingContext, Profile},
    loader::MarathonConfigurationLoader,
    schema::*,
};
use marathon_core::{
    analytics::{
        BackendFactory, MetricsSetup,
        remote::{graphite::GraphiteQuery, influx::InfluxQuery, influx2::FluxQuery},
    },
    construction::{MetricsBackends, RunInputs, construct},
    error::*,
    executor::{CoreExecutor, hooks::RuntimeHooks, shared::failure},
    logging::host_millis,
    marathon::Marathon,
    model::Test,
    runtime::*,
    time::{ClockResult, Timer},
    vendor::{LocalTestParser, LogConfigurator, TestParser, VendorContext, VendorServices},
};
use std::path::PathBuf;
use std::{
    rc::Rc,
    sync::{Arc, Mutex, OnceLock},
};

fn base() -> Configuration {
    static BASE: OnceLock<Configuration> = OnceLock::new();
    BASE.get_or_init(|| {
        let context = BindingContext {
            profile: Profile::Marathonfile,
            marathonfile: PathBuf::from("/config/Marathonfile"),
            marathonfile_dir: PathBuf::from("/config"),
            process_dir: PathBuf::from("/"),
            environment: vec![],
            now: jcompat::jtime::instant_of(0, 0).unwrap_or_else(|| unreachable!("epoch is representable")),
            analytics_tracking: false,
            bugsnag_reporting: false,
        };
        let marathonfile = b"name: \"DEFAULT_TEST_CONFIG\"\noutputDir: \"\"\nvendorConfiguration:\n  type: \"stub\"\n";
        MarathonConfigurationLoader
            .parse_sync(marathonfile, &context)
            .unwrap_or_else(|e| panic!("test Marathonfile: {e:?}"))
    })
    .clone()
}

/// The builder's properties with their test defaults. `outputDir` is a fresh temporary
/// directory (`Files.createTempDirectory("test-run")`), removed when the factory's guard drops.
pub struct ConfigurationFactory {
    pub configuration: Configuration,
    pub output: Option<tempfile::TempDir>,
}
impl Default for ConfigurationFactory {
    fn default() -> Self {
        Self::new()
    }
}
impl ConfigurationFactory {
    /// Binds the base configuration: call before entering the core executor.
    pub fn new() -> Self {
        let output = tempfile::Builder::new()
            .prefix("test-run")
            .tempdir()
            .unwrap_or_else(|e| panic!("temporary outputDir: {e}"));
        let mut c = base();
        c.name = "DEFAULT_TEST_CONFIG".into();
        c.output_dir = PathBuf::from(output.path().to_string_lossy().into_owned());
        c.vendor_configuration = VendorConfiguration::Stub;
        c.debug = true;
        c.batching_strategy = BatchingStrategyConfiguration::Isolate;
        c.analytics_configuration = AnalyticsConfiguration::Disabled;
        c.exclude_serial_regexes = vec![];
        c.uncompleted_test_retry_quota = i32::MAX;
        c.filtering_configuration = FilteringConfiguration {
            allowlist: vec![],
            blocklist: vec![],
        };
        c.flakiness_strategy = FlakinessStrategyConfiguration::Ignore;
        c.ignore_failures = false;
        c.include_serial_regexes = vec![];
        c.is_code_coverage_enabled = false;
        c.pooling_strategy = PoolingStrategyConfiguration::Omni;
        c.retry_strategy = RetryStrategyConfiguration::NoRetry;
        c.sharding_strategy = ShardingStrategyConfiguration::Parallel;
        c.sorting_strategy = SortingStrategyConfiguration::NoSorting;
        c.test_batch_timeout_millis = 180_000;
        c.test_output_timeout_millis = 60_000;
        c.analytics_tracking = false;
        c.screen_recording_policy = ScreenRecordingPolicy::OnAny;
        c.device_initialization_timeout_millis = 60_000;
        Self {
            configuration: c,
            output: Some(output),
        }
    }
    pub fn output_dir(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(self.configuration.output_dir.to_string_lossy().into_owned())
    }
}

/// `Mocks.TestParser.DEFAULT` with `whenever(extract()).thenReturn(tests)`.
pub struct StubTestParser(pub Vec<Test>);
impl LocalTestParser for StubTestParser {
    fn extract(&self, _context: VendorContext) -> VendorFuture<'_, Vec<Test>> {
        let tests = self.0.clone();
        Box::pin(async move { Ok(tests) })
    }
}
/// `mock<MarathonLogConfigurator>()`.
pub struct MockLogConfigurator;
impl LogConfigurator for MockLogConfigurator {
    fn configure(&self, _debug: bool, _logging: Arc<Mutex<MarathonLogging>>) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
/// Disabled analytics never reaches its clients.
pub struct NoMetricsSetup;
fn unused<T>() -> ServiceFuture<'static, T> {
    Box::pin(async { Err(failure("java.lang.IllegalStateException", "analytics is disabled in tests").into()) })
}
impl MetricsSetup for NoMetricsSetup {
    fn influx<'a>(&'a self, _: &'a InfluxDbConfiguration, _: bool) -> ServiceFuture<'a, Arc<dyn InfluxQuery>> {
        unused()
    }
    fn influx2<'a>(&'a self, _: &'a InfluxDb2Configuration, _: bool) -> ServiceFuture<'a, Arc<dyn FluxQuery>> {
        unused()
    }
    fn graphite<'a>(&'a self, _: &'a str) -> ServiceFuture<'a, Arc<dyn GraphiteQuery>> {
        unused()
    }
}

/// `mock<Timer>()` with `currentTimeMillis()` counting up from 0; every other reading is
/// Mockito's 0 and `measure` does not run its block.
#[derive(Default)]
pub struct CounterTimer(Mutex<i64>);
impl Timer for CounterTimer {
    fn start_time_millis(&self) -> i64 {
        0
    }
    fn elapsed_time_millis(&self) -> ClockResult<i64> {
        Ok(0)
    }
    fn current_time_millis(&self) -> ClockResult<i64> {
        let mut i = crate::lock(&self.0);
        let value = *i;
        *i += 1;
        Ok(value)
    }
    fn measure(&self, _block: &mut dyn FnMut()) -> ClockResult<i64> {
        Ok(0)
    }
}

/// StartupTimingsTest's `FakeTimer { currentTime }`: every reading is the given clock and
/// `measure` is unsupported.
pub struct FakeTimer(pub Arc<dyn Fn() -> i64 + Send + Sync>);
impl Timer for FakeTimer {
    fn start_time_millis(&self) -> i64 {
        0
    }
    fn elapsed_time_millis(&self) -> ClockResult<i64> {
        Ok((self.0)())
    }
    fn current_time_millis(&self) -> ClockResult<i64> {
        Ok((self.0)())
    }
    fn measure(&self, _block: &mut dyn FnMut()) -> ClockResult<i64> {
        Err(marathon_core::time::ReplayError {
            label: "FakeTimer.measure".into(),
            message: "java.lang.UnsupportedOperationException".into(),
        })
    }
}

pub struct MarathonFactory {
    pub configuration: ConfigurationFactory,
    pub timer: Option<Arc<dyn Timer>>,
    /// `tests { ... }`.
    pub tests: Vec<Test>,
    /// Replaces the parser (StartupTimings' remote parser, failure witnesses).
    pub parser: Option<TestParser>,
    pub bundle_identifier: Option<Arc<dyn marathon_core::vendor::TestBundleIdentifier>>,
    pub log_configurator: Option<Arc<dyn LogConfigurator>>,
    pub metrics: Option<MetricsBackends>,
    pub analytics: Option<marathon_core::construction::AnalyticsOverride>,
    pub backend: Option<Arc<dyn BackendFactory>>,
    pub exit: Option<Arc<dyn Fn(i32) + Send + Sync>>,
    pub signal_trigger: Option<Arc<marathon_core::executor::SharedDeferred<i32>>>,
    /// Appended through the 3.9c reporter seam.
    pub reporters: Vec<marathon_core::analytics::ReporterRegistration>,
    pub logging: Arc<Mutex<MarathonLogging>>,
    /// Prompt console output, as the CLI's live run has it; the oracle runs leave it off.
    pub live_output: bool,
}
impl Default for MarathonFactory {
    fn default() -> Self {
        Self::new()
    }
}

/// The built graph and the handles a test drives or inspects.
pub struct Built {
    pub marathon: Rc<Marathon>,
    pub provider: Arc<StubDeviceProvider>,
    pub console: Arc<RecordingConsole>,
}

impl MarathonFactory {
    pub fn new() -> Self {
        Self {
            configuration: ConfigurationFactory::new(),
            timer: None,
            tests: vec![],
            parser: None,
            bundle_identifier: Some(Arc::new(MockTestBundleIdentifier)),
            log_configurator: Some(Arc::new(MockLogConfigurator)),
            metrics: None,
            analytics: None,
            backend: None,
            exit: None,
            signal_trigger: None,
            reporters: vec![],
            logging: Arc::new(Mutex::new(MarathonLogging::default())),
            live_output: false,
        }
    }
    /// `MarathonFactory.build()`: Koin's graph, here the production construction with the
    /// Stub vendor (never reachable through the CLI, which rejects the stub vendor).
    pub async fn build(&mut self, core: &Rc<CoreExecutor>) -> Result<Built, Failure> {
        self.build_with(core, true).await
    }
    /// A vendor module that defines no TestParser.
    pub async fn build_without_parser(&mut self, core: &Rc<CoreExecutor>) -> Result<Built, Failure> {
        self.build_with(core, false).await
    }
    async fn build_with(&mut self, core: &Rc<CoreExecutor>, with_parser: bool) -> Result<Built, Failure> {
        let provider = StubDeviceProvider::new(core.event_channel("StubDeviceProvider"));
        let console = RecordingConsole::new(self.logging.clone());
        let parser = self
            .parser
            .clone()
            .unwrap_or_else(|| TestParser::Local(Arc::new(StubTestParser(self.tests.clone()))));
        let services = VendorServices {
            provider: provider.clone(),
            parser: with_parser.then_some(parser),
            bundle_identifier: self.bundle_identifier.clone(),
            log_configurator: self.log_configurator.clone(),
        };
        let metrics = self.metrics.take().unwrap_or(MetricsBackends {
            setup: Arc::new(NoMetricsSetup),
            backend: self.backend.clone(),
        });
        let hooks = RuntimeHooks::live();
        let barriers = hooks.barriers();
        let inputs = RunInputs {
            configuration: Arc::new(self.configuration.configuration.clone()),
            logging: self.logging.clone(),
            console: console.clone(),
            stderr: Arc::new(|_| {}),
            cwd: PathBuf::from(
                &std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            environment: vec![],
            system_information: vec![("os.name".into(), "test".into())],
            vendor: Box::new(move |_| Box::pin(async move { Ok(services) })),
            metrics: Box::new(move |_, _| metrics),
            analytics: self.analytics.take(),
            extra_reporters: {
                let reporters = std::mem::take(&mut self.reporters);
                Box::new(move |ctx| {
                    let mut all = marathon_core::construction::standard_reporters(ctx)?;
                    all.extend(reporters);
                    Ok(all)
                })
            },
            hooks,
            barriers,
            timer: self.timer.clone(),
            wall: Arc::new(host_millis),
            exit: self.exit.clone(),
            signal_trigger: self.signal_trigger.clone(),
            signal_hooks: Default::default(),
            live_output: self.live_output,
        };
        let marathon = construct(core.clone(), inputs).await?;
        Ok(Built {
            marathon,
            provider,
            console,
        })
    }
}
