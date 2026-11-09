//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/queue/QueueActorTest.kt
//! (createQueueActor and the pool channel), queue/QueueActorRetryPlacementTest.kt (queue, request, notifications)
//! and tools/oracle/queue-probes/QueueProbes.kt (the recording doubles and the step driver).
//!
//! Every recorded call takes a number from one counter, the log queue's wall clock included, so the
//! events of a message keep the order they happened in although logs and output reach the sink later.
#![allow(dead_code)]
use futures_util::future::{Either, select};
use jcompat::{jtime::Instant, logging::Level};
use marathon_config::{
    Configuration,
    binding::{BindingContext, Profile},
    loader::MarathonConfigurationLoader,
    schema::{
        BatchingStrategyConfiguration, ExecutionMode, ExecutionStrategyConfiguration, RetryStrategyConfiguration,
        SortingStrategyConfiguration,
    },
};
use marathon_core::{
    ci::Ci,
    error::*,
    execution::{
        DevicePoolMessage, FromQueue,
        progress::{PoolProgressAccumulator, ProgressServices, TestState},
        queue::{PreparedQueue, QueueActor, QueueHandle, QueueMessage, QueueSnapshot, RequestBatch, SharedProgress},
    },
    executor::{CoreExecutor, completion_logging::UNRECOVERABLE, shared::failure},
    logging::OutputRecord,
    model::*,
    output::{ConsoleSink, OutputDriver},
    runtime::*,
    strategy::{MetricsLookup, TestComparator},
    time::*,
    vendor::{BundleRef, TestBundleIdentifier},
    wiring::{MetricsProvider, Tracker},
};
use marathon_error::FailureKind;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicI64, Ordering},
    },
};

pub const POOL: &str = "pool";

pub fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `Configuration.Builder(name = "", outputDir = File(""))` with the Stub vendor and
/// `analyticsTracking = false`. Binding blocks: call it before entering the core executor.
pub fn configuration() -> Configuration {
    static CONFIGURATION: OnceLock<Configuration> = OnceLock::new();
    CONFIGURATION
        .get_or_init(|| {
            let context = BindingContext {
                profile: Profile::Marathonfile,
                marathonfile: PathBuf::from("/config/Marathonfile"),
                marathonfile_dir: PathBuf::from("/config"),
                process_dir: PathBuf::from("/"),
                environment: vec![],
                now: jcompat::jtime::instant_of(0, 0).unwrap(),
                analytics_tracking: false,
                bugsnag_reporting: false,
            };
            let marathonfile = b"name: \"\"\noutputDir: \"\"\nvendorConfiguration:\n  type: \"stub\"\n";
            MarathonConfigurationLoader
                .parse_sync(marathonfile, &context)
                .unwrap_or_else(|e| panic!("test Marathonfile: {e:?}"))
        })
        .clone()
}
pub fn fixed_size(size: i32) -> BatchingStrategyConfiguration {
    BatchingStrategyConfiguration::FixedSize {
        size,
        duration_millis: None,
        percentile: None,
        time_limit: None,
        last_mile_length: 0,
    }
}
pub fn epoch() -> Instant {
    jcompat::jtime::instant_of(0, 0).unwrap()
}

pub fn mtest(pkg: &str, clazz: &str, method: &str) -> Test {
    Test {
        pkg: pkg.into(),
        clazz: clazz.into(),
        method: method.into(),
        meta_properties: vec![],
    }
}
/// `DeviceStub(serialNumber = serial).toDeviceInfo()`.
pub fn device(serial: &str) -> DeviceInfo {
    DeviceInfo {
        operating_system: OperatingSystem {
            version: "Fake OS".into(),
        },
        serial_number: serial.into(),
        model: "fake model".into(),
        manufacturer: "fake manufacturer".into(),
        network_state: NetworkState::Connected,
        device_features: vec![],
        healthy: true,
    }
}
pub fn result(test: &Test, device: &DeviceInfo, batch: &str, status: TestStatus, start: i64, end: i64) -> TestResult {
    TestResult {
        test: test.clone(),
        device: device.clone(),
        test_batch_id: batch.into(),
        status,
        start_time: start,
        end_time: end,
        stacktrace: None,
        attachments: vec![],
    }
}
/// TestBatchResults.device is a Device the queue never reads.
pub fn batch_results(
    finished: Vec<TestResult>,
    failed: Vec<TestResult>,
    uncompleted: Vec<TestResult>,
) -> TestBatchResults {
    TestBatchResults {
        device: marathon_core::vendor::DeviceRef(Arc::new(NoDevice(
            jcompat::logging::MarathonLogging::default().logger("NoDevice", None),
        ))),
        finished,
        failed,
        uncompleted,
    }
}
struct NoDevice(jcompat::logging::Logger);
impl marathon_core::vendor::Device for NoDevice {
    fn info(&self) -> DeviceInfo {
        device("device")
    }
    fn abi(&self) -> String {
        "".into()
    }
    fn worker_id(&self) -> Option<String> {
        None
    }
    fn logger(&self) -> &jcompat::logging::Logger {
        &self.0
    }
    fn prepare<'a>(
        &'a self,
        _configuration: Arc<Configuration>,
        _context: marathon_core::vendor::VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn execute<'a>(
        &'a self,
        _configuration: Arc<Configuration>,
        _pool: DevicePoolId,
        _batch: BatchRef,
        _result: Arc<dyn Deferred<TestBatchResults>>,
        _context: marathon_core::vendor::VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

// ---- the trace ---------------------------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct Trace {
    counter: Arc<AtomicI64>,
    events: Arc<Mutex<Vec<(i64, Value)>>>,
}
impl Trace {
    pub fn next(&self) -> i64 {
        self.counter.fetch_add(1, Ordering::SeqCst)
    }
    pub fn push(&self, event: Value) {
        let at = self.next();
        self.push_at(at, event);
    }
    pub fn push_at(&self, at: i64, event: Value) {
        lock(&self.events).push((at, event));
    }
    /// The events so far, in the order they happened.
    pub fn drain(&self) -> Vec<Value> {
        let mut events = std::mem::take(&mut *lock(&self.events));
        events.sort_by_key(|(at, _)| *at);
        events.into_iter().map(|(_, event)| event).collect()
    }
}
pub fn t(test: &Test) -> Value {
    json!([test.pkg.to_owned(), test.clazz.to_owned(), test.method.to_owned()])
}
pub fn status_name(status: TestStatus) -> &'static str {
    match status {
        TestStatus::Failure => "FAILURE",
        TestStatus::Passed => "PASSED",
        TestStatus::Ignored => "IGNORED",
        TestStatus::Incomplete => "INCOMPLETE",
        TestStatus::AssumptionFailure => "ASSUMPTION_FAILURE",
    }
}
fn level_name(level: Level) -> &'static str {
    match level {
        Level::Trace => "TRACE",
        Level::Debug => "DEBUG",
        Level::Info => "INFO",
        Level::Warn => "WARN",
        Level::Error => "ERROR",
        Level::All | Level::Off => "OTHER",
    }
}
pub fn logger_name(pool: &str) -> String {
    format!("QueueActor[DevicePoolId(name={pool})]")
}

// ---- recording doubles --------------------------------------------------------------------------------------

/// The progress output sink and the Track: the queue's log lines, progress lines and `track` calls.
pub struct Sink {
    pub trace: Trace,
    pub logger: String,
    pub track_fails: std::sync::atomic::AtomicBool,
}
impl Sink {
    fn record(&self, record: OutputRecord) {
        match record {
            OutputRecord::Raw(text) => {
                let text = text.to_owned();
                self.trace
                    .push(json!(["stdout", text.strip_suffix('\n').unwrap_or(&text)]));
            }
            // The actor's exception handler logs under the queue's logger too (Actor.kt:22–23).
            OutputRecord::Log(record) if record.logger == self.logger && record.message == UNRECOVERABLE => {
                self.trace.push_at(
                    record.wall_millis,
                    json!(["handler", level_name(record.level), record.logger, UNRECOVERABLE]),
                )
            }
            OutputRecord::Log(record) if record.logger == self.logger => self.trace.push_at(
                record.wall_millis,
                json!(["log", level_name(record.level), record.message.to_owned(), null]),
            ),
            OutputRecord::Log(_) => {}
        }
    }
}
impl ConsoleSink for Sink {
    fn write(&self, records: Vec<OutputRecord>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            for record in records {
                self.record(record);
            }
            Ok(())
        })
    }
}
impl Tracker for Sink {
    fn device_connected<'a>(&'a self, _: &'a DevicePoolId, _: &'a DeviceInfo) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn device_disconnected<'a>(&'a self, _: &'a DevicePoolId, _: &'a DeviceInfo) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn device_provider_init<'a>(&'a self, _: &'a str, _: Instant, _: Instant) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn device_preparing<'a>(&'a self, _: &'a str, _: Instant, _: Instant) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn test<'a>(
        &'a self,
        pool: &'a DevicePoolId,
        device: &'a DeviceInfo,
        result: &'a TestResult,
        final_result: bool,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.trace.push(json!([
                "track",
                pool.name.to_owned(),
                device.serial_number.to_owned(),
                t(&result.test),
                status_name(result.status),
                result.start_time,
                result.end_time,
                result.test_batch_id.to_owned(),
                final_result
            ]));
            if self.track_fails.load(Ordering::SeqCst) {
                return Err(VendorError::Other(failure(FailureKind::IllegalState, "tracker failed")));
            }
            Ok(())
        })
    }
}

/// `Analytics(metrics)`: execution times from a map, success rate 1.0, recorded observations.
pub struct Metrics {
    pub trace: Trace,
    pub execution_times: HashMap<Test, f64>,
    pub observed: Mutex<Vec<Vec<TestResult>>>,
}
impl MetricsLookup for Metrics {
    fn success_rate(&self, _test: &Test, _limit: Instant) -> f64 {
        1.0
    }
    fn execution_time(&self, test: &Test, _percentile: f64, _limit: Instant) -> f64 {
        self.execution_times.get(test).copied().unwrap_or(1000.0)
    }
}
impl MetricsProvider for Metrics {
    fn success_rate<'a>(&'a self, _test: &'a Test, _limit: Instant) -> ServiceFuture<'a, f64> {
        Box::pin(async { Ok(1.0) })
    }
    fn execution_time<'a>(&'a self, test: &'a Test, percentile: f64, limit: Instant) -> ServiceFuture<'a, f64> {
        Box::pin(async move { Ok(MetricsLookup::execution_time(self, test, percentile, limit)) })
    }
    fn observe_execution_times<'a>(&'a self, results: &'a [TestResult]) -> ServiceFuture<'a, ()> {
        let observed: Vec<Value> = results
            .iter()
            .map(|r| json!([t(&r.test), status_name(r.status)]))
            .collect();
        self.trace.push(json!(["observe", observed]));
        lock(&self.observed).push(results.to_vec());
        Box::pin(async { Ok(()) })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Scripted random draws and sequential batch ids.
pub struct Hooks {
    pub trace: Trace,
    pub draws: Mutex<VecDeque<i32>>,
    pub ids: Mutex<u32>,
}
impl WallClock for Hooks {
    fn millis(&self, _label: &'static str) -> ClockResult<i64> {
        Ok(0)
    }
    fn instant(&self, _label: &'static str) -> ClockResult<Instant> {
        Ok(epoch())
    }
}
impl MonotonicClock for Hooks {
    fn nanos(&self, _label: &'static str) -> ClockResult<i64> {
        Ok(0)
    }
}
impl ReplayHooks for Hooks {
    fn draw_int(&self, label: &'static str) -> ClockResult<i32> {
        let value = lock(&self.draws).pop_front().ok_or_else(|| ReplayError {
            label: label.into(),
            message: "draw script exhausted".into(),
        })?;
        self.trace.push(json!(["draw", value]));
        Ok(value)
    }
    fn choose_key(&self, label: &'static str, _candidates: &[String]) -> ClockResult<String> {
        Err(ReplayError {
            label: label.into(),
            message: "unused".into(),
        })
    }
    fn allocate_id(&self, _label: &'static str) -> ClockResult<String> {
        let mut ids = lock(&self.ids);
        *ids += 1;
        Ok(format!("batch-{ids}"))
    }
    fn verify_consumed(&self) -> ClockResult<()> {
        Ok(())
    }
}

/// A scripted `Timer.currentTimeMillis`; an exhausted script fails like the probe's `removeFirst`.
pub struct ScriptedTimer {
    pub trace: Trace,
    pub times: Mutex<VecDeque<i64>>,
}
impl Timer for ScriptedTimer {
    fn start_time_millis(&self) -> i64 {
        0
    }
    fn elapsed_time_millis(&self) -> ClockResult<i64> {
        Ok(0)
    }
    fn current_time_millis(&self) -> ClockResult<i64> {
        let value = lock(&self.times).pop_front().ok_or_else(|| ReplayError {
            label: "Timer.currentTimeMillis".into(),
            message: "timer script exhausted".into(),
        })?;
        self.trace.push(json!(["time", value]));
        Ok(value)
    }
    fn measure(&self, block: &mut dyn FnMut()) -> ClockResult<i64> {
        block();
        Ok(0)
    }
}

/// Throws for one test, as the probe's identifier does; null bundles otherwise.
pub struct FailingIdentifier {
    pub trace: Trace,
    pub failing: Test,
}
impl TestBundleIdentifier for FailingIdentifier {
    fn identify(&self, test: &Test) -> VendorResult<Option<BundleRef>> {
        self.trace.push(json!(["identify", t(test)]));
        if *test == self.failing {
            let message = format!("no bundle for {}", test.method.to_owned());
            return Err(VendorError::Other(failure(FailureKind::IllegalState, &message)));
        }
        Ok(None)
    }
}

/// The pool's `SendChannel<FromQueue>` (`Channel.UNLIMITED`).
pub struct RecordingPool {
    job: JobRef,
    received: RefCell<VecDeque<FromQueue>>,
}
impl RecordingPool {
    pub fn new(core: &Rc<CoreExecutor>) -> Rc<Self> {
        Rc::new(Self {
            job: core.job(JobKind::Ordinary, None),
            received: RefCell::new(VecDeque::new()),
        })
    }
    pub fn take(&self) -> Vec<FromQueue> {
        self.received.borrow_mut().drain(..).collect()
    }
}
impl Actor<DevicePoolMessage> for RecordingPool {
    fn job(&self) -> JobRef {
        self.job.clone()
    }
    fn is_closed_for_send(&self) -> bool {
        false
    }
    fn try_send(&self, message: DevicePoolMessage) -> Result<(), SendError<DevicePoolMessage>> {
        match message {
            DevicePoolMessage::FromQueue(message) => {
                self.received.borrow_mut().push_back(message);
                Ok(())
            }
            other => panic!("the queue sent {other:?}"),
        }
    }
    fn send(&self, message: DevicePoolMessage) -> LocalFuture<'_, Result<(), SendError<DevicePoolMessage>>> {
        Box::pin(async move { self.try_send(message) })
    }
    fn safe_send(&self, message: DevicePoolMessage) -> LocalFuture<'_, Result<(), SendError<DevicePoolMessage>>> {
        self.send(message)
    }
    fn close(&self) {}
}

// ---- a queue --------------------------------------------------------------------------------------------------

pub struct Setup {
    pub configuration: Configuration,
    pub tests: Vec<Test>,
    pub flaky: Vec<Test>,
    pub execution_times: HashMap<Test, f64>,
    pub draws: Vec<i32>,
    pub timer: Vec<i64>,
    pub bundle_fails_on: Option<Test>,
    pub pool: String,
    /// Record each comparison as a `compare` event.
    pub record_comparisons: bool,
}
impl Setup {
    pub fn new(configuration: Configuration, tests: Vec<Test>) -> Self {
        Self {
            configuration,
            tests,
            flaky: vec![],
            execution_times: HashMap::new(),
            draws: vec![],
            timer: vec![],
            bundle_fails_on: None,
            pool: "test".into(),
            record_comparisons: false,
        }
    }
}

pub struct Queue {
    pub core: Rc<CoreExecutor>,
    pub handle: QueueHandle,
    pub pool: Rc<RecordingPool>,
    pub progress: SharedProgress,
    pub trace: Trace,
    pub sink: Arc<Sink>,
    pub metrics: Arc<Metrics>,
    pub snapshot: Rc<RefCell<Option<QueueSnapshot>>>,
    pub shard: TestShard,
}
impl Queue {
    pub fn spawn(core: &Rc<CoreExecutor>, setup: Setup) -> Self {
        let trace = Trace::default();
        let counter = trace.clone();
        core.log_queue().set_wall_clock(Rc::new(move || counter.next()));
        let pool_id = DevicePoolId {
            name: setup.pool.as_str().into(),
        };
        let shard = TestShard {
            tests: setup.tests,
            flaky_tests: setup.flaky,
        };
        let sink = Arc::new(Sink {
            trace: trace.clone(),
            logger: logger_name(&setup.pool),
            track_fails: Default::default(),
        });
        let accumulator = PoolProgressAccumulator::new(
            pool_id.clone(),
            &shard,
            setup.configuration.execution_strategy.clone(),
            Ci::None,
            ProgressServices {
                tracker: sink.clone(),
                logs: core.log_queue(),
                clock: core.clock(),
                output: OutputDriver {
                    vendor: core.vendor(),
                    sink: sink.clone(),
                },
            },
        );
        let progress: SharedProgress = Rc::new(tokio::sync::Mutex::new(accumulator));
        let metrics = Arc::new(Metrics {
            trace: trace.clone(),
            execution_times: setup.execution_times,
            observed: Mutex::new(vec![]),
        });
        let hooks = Arc::new(Hooks {
            trace: trace.clone(),
            draws: Mutex::new(setup.draws.into_iter().collect()),
            ids: Mutex::new(0),
        });
        let timer = Arc::new(ScriptedTimer {
            trace: trace.clone(),
            times: Mutex::new(setup.timer.into_iter().collect()),
        });
        let identifier = setup.bundle_fails_on.map(|failing| {
            Arc::new(FailingIdentifier {
                trace: trace.clone(),
                failing,
            }) as Arc<dyn TestBundleIdentifier>
        });
        let snapshot = Rc::new(RefCell::new(None));
        let observed = snapshot.clone();
        let mut prepared = PreparedQueue::from_parts(&setup.configuration, metrics.clone(), metrics.clone())
            .with_observer(Rc::new(move |state| *observed.borrow_mut() = Some(state)));
        if setup.record_comparisons {
            let comparisons = trace.clone();
            prepared = prepared.with_comparator(Box::new(move |mut inner: TestComparator| {
                Box::new(move |a: &Test, b: &Test| {
                    comparisons.push(json!(["compare", t(a), t(b)]));
                    inner(a, b)
                }) as TestComparator
            }));
        }
        let pool = RecordingPool::new(core);
        let parent = core.job(JobKind::Ordinary, None);
        let handle = QueueActor::spawn(
            core.clone(),
            prepared,
            &setup.configuration,
            shard.clone(),
            pool.clone(),
            pool_id,
            timer,
            identifier,
            progress.clone(),
            parent,
            hooks,
        );
        Self {
            core: core.clone(),
            handle,
            pool,
            progress,
            trace,
            sink,
            metrics,
            snapshot,
            shard,
        }
    }

    /// Sends `message`, then waits until the queue has handled it (an IsEmpty barrier) or has ended.
    pub async fn send(&self, message: QueueMessage) -> bool {
        if !self.handle.try_send(message) {
            return false;
        }
        self.barrier().await;
        true
    }
    /// Waits for the messages sent so far: IsEmpty is answered after them, unless the queue ends first.
    pub async fn barrier(&self) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.handle.try_send(QueueMessage::IsEmpty(tx));
        self.settle_on(rx).await;
    }
    pub async fn is_empty(&self) -> Option<bool> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if !self.handle.try_send(QueueMessage::IsEmpty(tx)) {
            return None;
        }
        self.settle_on(rx).await
    }
    pub async fn settle_on(&self, rx: tokio::sync::oneshot::Receiver<bool>) -> Option<bool> {
        let job = self.handle.job();
        let answer = match select(rx, job.join()).await {
            Either::Left((answer, _)) => answer.ok(),
            Either::Right(_) => None,
        };
        self.drain_logs();
        answer
    }
    /// Log records the accumulator has not flushed yet.
    pub fn drain_logs(&self) {
        for record in self.core.log_queue().take_output() {
            self.sink.record(record);
        }
    }
    pub fn replies(&self) -> Vec<FromQueue> {
        self.pool.take()
    }
    pub async fn request(&self, device: &DeviceInfo, worker: Option<&str>) -> Vec<FromQueue> {
        self.send(QueueMessage::RequestBatch(RequestBatch {
            device: device.clone(),
            worker_id: worker.map(String::from),
        }))
        .await;
        self.replies()
    }
    pub async fn complete(&self, device: &DeviceInfo, results: TestBatchResults) -> Vec<FromQueue> {
        self.send(QueueMessage::Completed(device.clone(), results)).await;
        self.replies()
    }
    pub async fn states(&self) -> Vec<(String, TestState)> {
        let progress = self.progress.lock().await;
        let mut tests: Vec<&Test> = self.shard.tests.iter().chain(&self.shard.flaky_tests).collect();
        tests.dedup();
        let mut states: Vec<(String, TestState)> = tests
            .into_iter()
            .filter_map(|test| {
                progress
                    .state(test)
                    .map(|state| (test.to_test_name('.', '#').to_owned(), state))
            })
            .collect();
        states.sort_by(|a, b| a.0.cmp(&b.0));
        states.dedup_by(|a, b| a.0 == b.0);
        states
    }
    pub fn tracked(&self) -> Vec<Value> {
        self.trace
            .drain()
            .into_iter()
            .filter(|event| event[0] == "track")
            .collect()
    }
}

pub fn batch_of(reply: &FromQueue) -> BatchRef {
    match reply {
        FromQueue::ExecuteBatch(_, batch) => batch.clone(),
        other => panic!("expected ExecuteBatch, got {other:?}"),
    }
}
pub fn executed(replies: &[FromQueue]) -> Vec<Test> {
    replies
        .iter()
        .find_map(|reply| match reply {
            FromQueue::ExecuteBatch(_, batch) => Some(batch.tests.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no ExecuteBatch in {replies:?}"))
}

pub fn run(body: impl FnOnce(Rc<CoreExecutor>) -> LocalFuture<'static, OperationResult<()>> + Send + 'static) {
    CoreExecutor::run(body).unwrap();
}

/// The row's configuration (QueueProbes.kt `configuration`).
pub fn scenario_configuration(row: &Value) -> Configuration {
    let mut configuration = configuration();
    configuration.sorting_strategy = match row["sorting"].as_str() {
        Some("execution-time") => SortingStrategyConfiguration::ExecutionTime {
            percentile: 90.0,
            time_limit: epoch(),
        },
        Some("random-order") => SortingStrategyConfiguration::RandomOrder,
        _ => SortingStrategyConfiguration::NoSorting,
    };
    configuration.batching_strategy = match row["batching"].as_str() {
        Some("class-name") => BatchingStrategyConfiguration::ClassName,
        Some("isolate") => BatchingStrategyConfiguration::Isolate,
        Some(other) => fixed_size(other.trim_start_matches("fixed-size:").parse().unwrap()),
        None => panic!("no batching"),
    };
    configuration.retry_strategy = match row["retry"].as_array() {
        Some(quotas) => RetryStrategyConfiguration::FixedQuota {
            total_allowed_retry_quota: quotas[0].as_i64().unwrap() as i32,
            retry_per_test_quota: quotas[1].as_i64().unwrap() as i32,
        },
        None => RetryStrategyConfiguration::NoRetry,
    };
    configuration.uncompleted_test_retry_quota = row["uncompletedQuota"].as_i64().unwrap() as i32;
    configuration.execution_strategy = ExecutionStrategyConfiguration {
        mode: if row["mode"] == "ALL_SUCCESS" {
            ExecutionMode::AllSuccess
        } else {
            ExecutionMode::AnySuccess
        },
        fast: row["fast"].as_bool().unwrap(),
    };
    configuration
}
pub fn test_of(value: &Value) -> Test {
    mtest(
        value[0].as_str().unwrap(),
        value[1].as_str().unwrap(),
        value[2].as_str().unwrap(),
    )
}
pub fn tests_of(value: &Value) -> Vec<Test> {
    value.as_array().unwrap().iter().map(test_of).collect()
}
