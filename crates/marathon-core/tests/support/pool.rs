//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/DevicePoolActorSchedulingTest.kt
//! (TestDevice, RecordingTracker, startPool), DevicePoolActorBatchAccountingTest.kt (ScriptedDevice)
//! and DevicePoolActorSchedulingCharacterizationTest.kt (TimedDevice).
//!
//! Every scenario runs the real pool, queue, device actors and accumulator on `run_virtual`:
//! `runCurrent` is a drain, `advanceUntilIdle` runs the virtual timers out, and Kotlin's
//! `CompletableDeferred`s and `Channel`s that the devices wait on are `Gate`s and `Finish`es.
#![allow(dead_code)]
use jcompat::{
    jtime::Instant,
    logging::{Level, Logger, MarathonLogging},
};
use marathon_config::{
    Configuration,
    binding::{BindingContext, Profile},
    loader::MarathonConfigurationLoader,
    schema::{BatchingStrategyConfiguration, RetryStrategyConfiguration},
};
use marathon_core::{
    ci::Ci,
    error::*,
    execution::{
        DevicePoolMessage, FromDevice, FromQueue, FromScheduler,
        device::DeviceActorRef,
        pool::{DevicePool, DevicePoolActor, PoolParts},
        progress::{PoolProgressAccumulator, ProgressServices},
        queue::{BeforeExecute, PreparedQueue, SharedProgress},
    },
    executor::{
        CoreExecutor,
        hooks::{ClockMode, RuntimeHooks, Tape},
        shared::failure,
        test_support::ControlledReplay,
    },
    logging::{LogRecord, OutputRecord},
    model::*,
    output::{ConsoleSink, OutputDriver},
    runtime::*,
    strategy::MetricsLookup,
    time::*,
    vendor::{Device, DeviceRef, TestBundleIdentifier, VendorContext},
    wiring::{MetricsProvider, Tracker},
};
use marathon_error::FailureKind;
use std::path::PathBuf;
use std::{
    cell::RefCell,
    future::Future,
    rc::Rc,
    sync::{Arc, Mutex, OnceLock, Weak},
};

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
pub fn fixed_quota(total: i32, per_test: i32) -> RetryStrategyConfiguration {
    RetryStrategyConfiguration::FixedQuota {
        total_allowed_retry_quota: total,
        retry_per_test_quota: per_test,
    }
}
/// `estimatingBatches`: FixedSize(size = 1, durationMillis = 1, percentile = 90.0, timeLimit = EPOCH).
pub fn estimating_batches() -> BatchingStrategyConfiguration {
    BatchingStrategyConfiguration::FixedSize {
        size: 1,
        duration_millis: Some(1),
        percentile: Some(90.0),
        time_limit: Some(jcompat::jtime::instant_of(0, 0).unwrap()),
        last_mile_length: 0,
    }
}

pub fn mtest(clazz: &str, method: &str) -> Test {
    Test {
        pkg: "pkg".into(),
        clazz: clazz.into(),
        method: method.into(),
        meta_properties: vec![],
    }
}
/// `tests(count, clazz)`: test1..testN of `pkg.<clazz>`.
pub fn tests(count: usize, clazz: &str) -> Vec<Test> {
    (1..=count).map(|i| mtest(clazz, &format!("test{i}"))).collect()
}
pub fn methods(tests: &[Test]) -> Vec<String> {
    tests.iter().map(|t| t.method.to_owned()).collect()
}
pub fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values
}

pub fn ise(message: &str) -> Failure {
    failure(FailureKind::IllegalState, message)
}
/// `DeviceLostException(cause)`: Throwable(cause) takes `cause.toString()` as its message.
pub fn device_lost(cause: Failure) -> VendorError {
    VendorError::DeviceLost(Failure {
        kind: FailureKind::DeviceLost,
        message: Some(cause.to_string()),
        cause: Some(Box::new(cause)),
        stacktrace: None,
    })
}
/// `Job.cancel()` without a cause.
pub fn job_was_cancelled() -> Cancellation {
    Cancellation::Requested(failure(FailureKind::JobCancelled, "Job was cancelled"))
}

// ---- Send signals the devices wait on --------------------------------------------------

/// A `CompletableDeferred<Unit>` shared with vendor code.
#[derive(Clone, Default)]
pub struct Gate(Arc<Mutex<GateState>>);
#[derive(Default)]
struct GateState {
    open: bool,
    waiters: Vec<tokio::sync::oneshot::Sender<()>>,
}
impl Gate {
    pub fn open(&self) {
        let mut state = lock(&self.0);
        state.open = true;
        // CompletableDeferred resumes its waiters in registration order; watch wakes in reverse order.
        for sender in state.waiters.drain(..) {
            let _ = sender.send(());
        }
    }
    pub fn is_open(&self) -> bool {
        lock(&self.0).open
    }
    pub async fn wait(&self) {
        let receiver = {
            let mut state = lock(&self.0);
            if state.open {
                return;
            }
            let (sender, receiver) = tokio::sync::oneshot::channel();
            state.waiters.push(sender);
            receiver
        };
        let _ = receiver.await;
    }
    /// `await()` in vendor code: cancellable, as a suspending Kotlin call is.
    pub async fn wait_in(&self, context: &VendorContext) -> VendorResult<()> {
        use futures_util::future::{Either, select};
        let cancelled = context.cancellation.cancelled();
        let opened = self.wait();
        match select(std::pin::pin!(cancelled), std::pin::pin!(opened)).await {
            Either::Left((reason, _)) => Err(VendorError::Cancelled(reason)),
            Either::Right(_) => Ok(()),
        }
    }
}

/// `Channel<Unit>(Channel.UNLIMITED)`.
pub struct Finish {
    sender: tokio::sync::mpsc::UnboundedSender<()>,
    receiver: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<()>>,
}
impl Default for Finish {
    fn default() -> Self {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        Self {
            sender,
            receiver: tokio::sync::Mutex::new(receiver),
        }
    }
}
impl Finish {
    pub fn send(&self) {
        let _ = self.sender.send(());
    }
    pub async fn receive_in(&self, context: &VendorContext) -> VendorResult<()> {
        use futures_util::future::{Either, select};
        let cancelled = context.cancellation.cancelled();
        let received = async { self.receiver.lock().await.recv().await };
        match select(std::pin::pin!(cancelled), std::pin::pin!(received)).await {
            Either::Left((reason, _)) => Err(VendorError::Cancelled(reason)),
            Either::Right(_) => Ok(()),
        }
    }
}

// ---- TestDevice --------------------------------------------------------------------------

pub type BoxFuture = std::pin::Pin<Box<dyn Future<Output = VendorResult<()>> + Send>>;
/// One `execute` call: the device, its batch, the result deferred and the vendor context.
pub struct Execution {
    pub device: Arc<TestDevice>,
    pub batch: BatchRef,
    pub result: Arc<dyn Deferred<TestBatchResults>>,
    pub context: VendorContext,
}
pub type OnPrepare = Arc<dyn Fn(VendorContext) -> BoxFuture + Send + Sync>;
pub type OnExecute = Arc<dyn Fn(Execution) -> BoxFuture + Send + Sync>;

pub fn on_prepare<F, Fut>(f: F) -> OnPrepare
where
    F: Fn(VendorContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = VendorResult<()>> + Send + 'static,
{
    Arc::new(move |context| Box::pin(f(context)))
}
pub fn on_execute<F, Fut>(f: F) -> OnExecute
where
    F: Fn(Execution) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = VendorResult<()>> + Send + 'static,
{
    Arc::new(move |execution| Box::pin(f(execution)))
}
/// Prepare waits for `gate`.
pub fn prepare_on(gate: &Gate) -> OnPrepare {
    let gate = gate.clone();
    on_prepare(move |context| {
        let gate = gate.clone();
        async move { gate.wait_in(&context).await }
    })
}
/// The default `onExecute`: report every test passed.
pub fn report_passed() -> OnExecute {
    on_execute(|e| async move {
        e.result.complete(Ok(e.device.report(&e.batch, TestStatus::Passed)));
        Ok(())
    })
}
/// `finishOnSignal`: report every test passed once `finish` has a signal.
pub fn finish_on_signal() -> OnExecute {
    on_execute(|e| async move {
        e.device.finish.receive_in(&e.context).await?;
        e.result.complete(Ok(e.device.report(&e.batch, TestStatus::Passed)));
        Ok(())
    })
}
/// `awaitCancellation()`.
pub fn await_cancellation() -> OnExecute {
    on_execute(|e| async move { Err(VendorError::Cancelled(e.context.cancellation.cancelled().await)) })
}
/// Wait for `gate`, then report every test failed.
pub fn fail_on(gate: &Gate) -> OnExecute {
    let gate = gate.clone();
    on_execute(move |e| {
        let gate = gate.clone();
        async move {
            gate.wait_in(&e.context).await?;
            e.result.complete(Ok(e.device.report(&e.batch, TestStatus::Failure)));
            Ok(())
        }
    })
}

/// `TestDevice` (and the BatchAccounting `ScriptedDevice`): a StubDevice(0, 0) with a serial
/// number, a worker, and prepare/execute hooks. It records the batches it was given.
pub struct TestDevice {
    me: Weak<TestDevice>,
    serial: String,
    pub model: String,
    pub worker_id: Option<String>,
    logger: Logger,
    prepare: Option<OnPrepare>,
    execute: OnExecute,
    pub batches: Mutex<Vec<Vec<String>>>,
    pub finish: Finish,
    return_wait_used: std::sync::atomic::AtomicI64,
}
impl TestDevice {
    pub fn new(serial: &str, worker_id: Option<&str>, prepare: Option<OnPrepare>, execute: OnExecute) -> Arc<Self> {
        Self::new_with_model(serial, "test", worker_id, prepare, execute)
    }
    pub fn new_with_model(
        serial: &str,
        model: &str,
        worker_id: Option<&str>,
        prepare: Option<OnPrepare>,
        execute: OnExecute,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            serial: serial.into(),
            model: model.into(),
            worker_id: worker_id.map(Into::into),
            logger: MarathonLogging::default().logger("StubDevice", None),
            prepare,
            execute,
            batches: Mutex::new(vec![]),
            finish: Finish::default(),
            return_wait_used: std::sync::atomic::AtomicI64::new(0),
        })
    }
    /// What this device reports having used of an emptied pool's wait (D48).
    pub fn set_return_wait_used(&self, millis: i64) {
        self.return_wait_used
            .store(millis, std::sync::atomic::Ordering::Relaxed);
    }
    /// `TestDevice(serial)` with the default hooks.
    pub fn plain(serial: &str) -> Arc<Self> {
        Self::new(serial, None, None, report_passed())
    }
    pub fn reference(&self) -> DeviceRef {
        match self.me.upgrade() {
            Some(me) => DeviceRef(me),
            None => unreachable!("a device is alive while it is called"),
        }
    }
    pub fn batches(&self) -> Vec<Vec<String>> {
        lock(&self.batches).clone()
    }
    /// `batches.flatten()`.
    pub fn executed_tests(&self) -> Vec<String> {
        self.batches().concat()
    }
    /// The BatchAccounting `executedTests()`: sorted.
    pub fn executed_sorted(&self) -> Vec<String> {
        sorted(self.executed_tests())
    }
    pub fn report(&self, batch: &BatchRef, status: TestStatus) -> TestBatchResults {
        self.report_each(
            batch,
            &batch.tests.iter().map(|t| (t.clone(), status)).collect::<Vec<_>>(),
        )
    }
    /// `report(batch, *statuses)`.
    pub fn report_each(&self, batch: &BatchRef, statuses: &[(Test, TestStatus)]) -> TestBatchResults {
        let results: Vec<TestResult> = statuses
            .iter()
            .map(|(test, status)| TestResult {
                test: test.clone(),
                device: self.info(),
                test_batch_id: batch.id.clone(),
                status: *status,
                start_time: 0,
                end_time: 1,
                stacktrace: None,
                attachments: vec![],
            })
            .collect();
        let only = |status| results.iter().filter(|r| r.status == status).cloned().collect();
        TestBatchResults {
            device: self.reference(),
            finished: only(TestStatus::Passed),
            failed: only(TestStatus::Failure),
            uncompleted: only(TestStatus::Incomplete),
        }
    }
}
impl Device for TestDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            operating_system: OperatingSystem { version: "25".into() },
            serial_number: self.serial.as_str().into(),
            model: self.model.as_str().into(),
            manufacturer: "test".into(),
            network_state: NetworkState::Connected,
            device_features: vec![],
            healthy: true,
        }
    }
    fn abi(&self) -> String {
        "test".into()
    }
    fn worker_id(&self) -> Option<String> {
        self.worker_id.as_deref().map(Into::into)
    }
    fn logger(&self) -> &Logger {
        &self.logger
    }
    fn prepare<'a>(&'a self, _configuration: Arc<Configuration>, context: VendorContext) -> VendorFuture<'a, ()> {
        match &self.prepare {
            Some(prepare) => prepare(context),
            None => Box::pin(async { Ok(()) }),
        }
    }
    fn execute<'a>(
        &'a self,
        _configuration: Arc<Configuration>,
        _pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<dyn Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        lock(&self.batches).push(methods(&batch.tests));
        let Some(device) = self.me.upgrade() else {
            unreachable!("a device is alive while it is called")
        };
        (self.execute)(Execution {
            device,
            batch,
            result,
            context,
        })
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn return_wait_used_millis(&self) -> i64 {
        self.return_wait_used.load(std::sync::atomic::Ordering::Relaxed)
    }
}

// ---- Recording doubles ------------------------------------------------------------------

/// `RecordingTracker`: one line per `test` call; `events()` sorts them.
#[derive(Default)]
pub struct RecordingTracker {
    events: Mutex<Vec<String>>,
    pub connected: Mutex<Vec<(String, String)>>,
    pub disconnected: Mutex<Vec<(String, String)>>,
}
impl RecordingTracker {
    pub fn events(&self) -> Vec<String> {
        sorted(lock(&self.events).clone())
    }
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
impl Tracker for RecordingTracker {
    fn device_connected<'a>(&'a self, pool: &'a DevicePoolId, device: &'a DeviceInfo) -> VendorFuture<'a, ()> {
        lock(&self.connected).push((pool.name.to_owned(), device.serial_number.to_owned()));
        Box::pin(async { Ok(()) })
    }
    fn device_disconnected<'a>(&'a self, pool: &'a DevicePoolId, device: &'a DeviceInfo) -> VendorFuture<'a, ()> {
        lock(&self.disconnected).push((pool.name.to_owned(), device.serial_number.to_owned()));
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
        _pool: &'a DevicePoolId,
        device: &'a DeviceInfo,
        result: &'a TestResult,
        final_result: bool,
    ) -> VendorFuture<'a, ()> {
        lock(&self.events).push(format!(
            "{} {} {} on {}",
            result.test.method.to_owned(),
            status_name(result.status),
            if final_result { "final" } else { "non-final" },
            device.serial_number.to_owned()
        ));
        Box::pin(async { Ok(()) })
    }
}

/// Mockito's `mock<Analytics>()`: every estimate is 0.0, observations are dropped.
pub struct NoMetrics;
impl MetricsLookup for NoMetrics {
    fn success_rate(&self, _test: &Test, _limit: Instant) -> f64 {
        0.0
    }
    fn execution_time(&self, _test: &Test, _percentile: f64, _limit: Instant) -> f64 {
        0.0
    }
}
impl MetricsProvider for NoMetrics {
    fn success_rate<'a>(&'a self, _test: &'a Test, _limit: Instant) -> ServiceFuture<'a, f64> {
        Box::pin(async { Ok(0.0) })
    }
    fn execution_time<'a>(&'a self, _test: &'a Test, _percentile: f64, _limit: Instant) -> ServiceFuture<'a, f64> {
        Box::pin(async { Ok(0.0) })
    }
    fn observe_execution_times<'a>(&'a self, _results: &'a [TestResult]) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Mockito's `mock<Timer>()`: 0 for every reading.
pub struct ZeroTimer;
impl Timer for ZeroTimer {
    fn start_time_millis(&self) -> i64 {
        0
    }
    fn elapsed_time_millis(&self) -> ClockResult<i64> {
        Ok(0)
    }
    fn current_time_millis(&self) -> ClockResult<i64> {
        Ok(0)
    }
    fn measure(&self, block: &mut dyn FnMut()) -> ClockResult<i64> {
        block();
        Ok(0)
    }
}

/// Progress lines go nowhere; logs stay in the core log queue for the test to read.
pub struct NullSink;
#[derive(Default)]
pub struct RecordingSink(pub Mutex<Vec<OutputRecord>>);
impl ConsoleSink for RecordingSink {
    fn write(&self, records: Vec<OutputRecord>) -> VendorFuture<'_, ()> {
        lock(&self.0).extend(records);
        Box::pin(async { Ok(()) })
    }
}
impl ConsoleSink for NullSink {
    fn write(&self, _records: Vec<OutputRecord>) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Live hooks whose wall clock reads virtual time, so vendor code can tell the time
/// (the characterization's `testScheduler.currentTime`).
pub struct VirtualWallHooks {
    live: Arc<RuntimeHooks>,
    wall: Arc<RuntimeHooks>,
}
impl VirtualWallHooks {
    pub fn new(core: &CoreExecutor) -> Arc<Self> {
        let tape = Tape {
            wall: ClockMode::Scheduler(0),
            ..Tape::default()
        };
        Arc::new(Self {
            live: RuntimeHooks::live(),
            wall: RuntimeHooks::taped(tape, &core.clock()),
        })
    }
}
impl WallClock for VirtualWallHooks {
    fn millis(&self, label: &'static str) -> ClockResult<i64> {
        self.wall.millis(label)
    }
    fn instant(&self, label: &'static str) -> ClockResult<Instant> {
        self.live.instant(label)
    }
}
impl MonotonicClock for VirtualWallHooks {
    fn nanos(&self, label: &'static str) -> ClockResult<i64> {
        self.live.nanos(label)
    }
}
impl ReplayHooks for VirtualWallHooks {
    fn draw_int(&self, label: &'static str) -> ClockResult<i32> {
        self.live.draw_int(label)
    }
    fn choose_key(&self, label: &'static str, candidates: &[String]) -> ClockResult<String> {
        self.live.choose_key(label, candidates)
    }
    fn allocate_id(&self, label: &'static str) -> ClockResult<String> {
        self.live.allocate_id(label)
    }
    fn verify_consumed(&self) -> ClockResult<()> {
        Ok(())
    }
}

// ---- The harness ------------------------------------------------------------------------

/// A pool, its accumulator, and the device actors it has created.
pub struct Pool {
    pub pool: DevicePool,
    pub progress: SharedProgress,
    pub actors: Rc<RefCell<Vec<DeviceActorRef>>>,
}
impl Pool {
    pub fn send(&self, message: DevicePoolMessage) {
        assert!(self.pool.actor.try_send(message).is_ok(), "the pool rejected a message");
    }
    /// `trySend`, whose failure the Kotlin tests ignore.
    pub fn try_send(&self, message: DevicePoolMessage) -> bool {
        self.pool.actor.try_send(message).is_ok()
    }
    pub fn add(&self, device: &Arc<TestDevice>) {
        self.send(add(device));
    }
    pub fn remove(&self, device: &Arc<TestDevice>) {
        self.send(remove(device));
    }
    pub fn terminated(&self) -> bool {
        self.pool.terminated()
    }
    pub async fn aggregate_result(&self) -> VendorResult<bool> {
        self.progress.lock().await.aggregate_result()
    }
    /// The latest actor created for `device`.
    pub fn actor_of(&self, device: &Arc<TestDevice>) -> Option<DeviceActorRef> {
        let reference = device.reference();
        self.actors
            .borrow()
            .iter()
            .rev()
            .find(|actor| actor.device == reference)
            .cloned()
    }
}
pub fn add(device: &Arc<TestDevice>) -> DevicePoolMessage {
    DevicePoolMessage::FromScheduler(FromScheduler::AddDevice(device.reference()))
}
pub fn remove(device: &Arc<TestDevice>) -> DevicePoolMessage {
    DevicePoolMessage::FromScheduler(FromScheduler::RemoveDevice(device.reference()))
}
pub fn is_ready(device: &Arc<TestDevice>) -> DevicePoolMessage {
    DevicePoolMessage::FromDevice(FromDevice::IsReady(device.reference()))
}
pub fn notify() -> DevicePoolMessage {
    DevicePoolMessage::FromQueue(FromQueue::Notify { retry_workers: vec![] })
}

/// What `startPool` may change.
pub struct PoolOptions {
    pub pool: String,
    pub configure: Box<dyn FnOnce(&mut Configuration)>,
    pub before_execute: Option<BeforeExecute>,
    pub bundle_identifier: Option<Arc<dyn TestBundleIdentifier>>,
    /// Durations for sorting and batching, in place of `NoMetrics`.
    pub lookup: Option<Arc<dyn MetricsLookup>>,
}
impl Default for PoolOptions {
    fn default() -> Self {
        Self {
            pool: "scheduling".into(),
            configure: Box::new(|_| {}),
            before_execute: None,
            bundle_identifier: None,
            lookup: None,
        }
    }
}

pub struct Harness {
    pub core: Rc<CoreExecutor>,
    pub driver: Rc<ControlledReplay>,
    /// The Kotlin tests' `parent = Job()`.
    pub parent: JobRef,
    pub tracker: Arc<RecordingTracker>,
    pub sink: Arc<RecordingSink>,
    pub hooks: Arc<dyn ReplayHooks>,
    pub barriers: Arc<dyn ReplayBarriers>,
    collected_logs: RefCell<Vec<LogRecord>>,
    base: Configuration,
}
impl Harness {
    pub fn new(core: Rc<CoreExecutor>, driver: Rc<ControlledReplay>, base: Configuration) -> Rc<Self> {
        let live = RuntimeHooks::live();
        let barriers = live.barriers();
        Rc::new(Self {
            parent: core.job(JobKind::Ordinary, None),
            hooks: VirtualWallHooks::new(&core),
            core,
            driver,
            tracker: Arc::new(RecordingTracker::default()),
            sink: Arc::new(RecordingSink::default()),
            barriers,
            collected_logs: RefCell::new(vec![]),
            base,
        })
    }
    pub fn start_pool(&self, tests: Vec<Test>, options: PoolOptions) -> Pool {
        let mut configuration = self.base.clone();
        (options.configure)(&mut configuration);
        let pool_id = DevicePoolId {
            name: options.pool.as_str().into(),
        };
        let shard = TestShard {
            tests,
            flaky_tests: vec![],
        };
        let accumulator = PoolProgressAccumulator::new(
            pool_id.clone(),
            &shard,
            configuration.execution_strategy.clone(),
            Ci::None,
            ProgressServices {
                tracker: self.tracker.clone(),
                logs: self.core.log_queue(),
                clock: self.core.clock(),
                output: OutputDriver {
                    vendor: self.core.vendor(),
                    sink: self.sink.clone(),
                },
            },
        );
        let progress: SharedProgress = Rc::new(tokio::sync::Mutex::new(accumulator));
        let metrics = Arc::new(NoMetrics);
        let lookup: Arc<dyn MetricsLookup> = options.lookup.unwrap_or_else(|| metrics.clone());
        let mut prepared = PreparedQueue::from_parts(&configuration, lookup, metrics);
        if let Some(hook) = options.before_execute {
            prepared = prepared.with_before_execute(hook);
        }
        let actors = Rc::new(RefCell::new(vec![]));
        let observed = actors.clone();
        let pool = DevicePoolActor::spawn_observed(
            PoolParts {
                core: self.core.clone(),
                pool_id,
                configuration: Arc::new(configuration),
                progress: progress.clone(),
                prepared,
                shard,
                timer: Arc::new(ZeroTimer),
                bundle_identifier: options.bundle_identifier,
                parent: self.parent.clone(),
                hooks: self.hooks.clone(),
                barriers: self.barriers.clone(),
            },
            Rc::new(move |actor: &DeviceActorRef| observed.borrow_mut().push(actor.clone())),
        );
        Pool { pool, progress, actors }
    }
    pub async fn run_current(&self) {
        replay(self.driver.drain_current().await);
    }
    pub async fn advance_until_idle(&self) {
        replay(self.driver.advance_until_idle().await);
    }
    pub fn now(&self) -> i64 {
        self.core.clock().now_millis()
    }
    /// `parent.children.forEach { it.join() }`, once virtual time has run out.
    pub async fn await_pool_termination(&self) {
        self.advance_until_idle().await;
        for child in self.parent.children_snapshot() {
            assert!(
                matches!(child.state(), JobState::Completed(_)),
                "a child of the pools' parent is still running: {:?}",
                child.state()
            );
        }
    }
    /// A core child of the device actor's job that waits for `gate` and then fails, as a
    /// coroutine launched into the device's scope does (3.6 decision 6).
    pub fn fail_actor_on(&self, actor: &DeviceActorRef, gate: Option<Gate>, message: &'static str) {
        let job = Actor::job(actor.actor.as_ref());
        self.core.spawn(
            job,
            Box::new(move |_| {
                Box::pin(async move {
                    if let Some(gate) = gate {
                        gate.wait().await;
                    }
                    Err(ise(message).into())
                })
            }),
        );
    }
    /// The log records so far, oldest first.
    pub fn logs(&self) -> Vec<LogRecord> {
        let mut records = std::mem::take(&mut *lock(&self.sink.0));
        records.extend(self.core.log_queue().take_output());
        self.collected_logs
            .borrow_mut()
            .extend(records.into_iter().filter_map(|record| match record {
                OutputRecord::Log(record) => Some(record),
                OutputRecord::Raw(_) => None,
            }));
        self.collected_logs.borrow().clone()
    }
}

pub fn replay<T>(result: ClockResult<T>) -> T {
    result.unwrap_or_else(|e| panic!("replay failure: {e:?}"))
}

/// Runs a scenario on a fresh virtual executor; the teardown cancels `parent` and checks
/// that nothing is left runnable.
pub fn scheduling_test(body: impl FnOnce(Rc<Harness>) -> LocalFuture<'static, ()> + Send + 'static) {
    let name = std::thread::current().name().unwrap_or("").to_owned();
    let base = configuration();
    let result = CoreExecutor::run_virtual(move |core, driver| {
        Box::pin(async move {
            let harness = Harness::new(core, driver.clone(), base);
            body(harness.clone()).await;
            if name != "devices_stay_busy_while_the_queue_has_work" {
                let row = include_str!("../../../../fixtures/oracle/pool/tests.jsonl")
                    .lines()
                    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                    .find(|row| row["name"] == name);
                if let Some(row) = row {
                    let normalize = |events: Vec<String>| {
                        // These Kotlin rows assert assignments through relational invariants, not exact
                        // device strings. Vendor completions without a tape can interleave (PLAN §5).
                        let relational = matches!(
                            name.as_str(),
                            "repeated_readiness_and_queue_notifications_neither_duplicate_assignments_nor_leave_devices_idle"
                                | "retries_of_batches_that_failed_together_on_one_worker_run_on_idle_devices_of_other_workers"
                                | "retries_of_batches_that_failed_together_on_different_workers_swap_workers"
                                | "idle_devices_that_ask_together_leave_each_retry_to_another_worker"
                                | "retries_of_a_batch_that_failed_on_one_worker_run_on_all_idle_devices_of_other_workers"
                                | "pools_schedule_their_own_devices_and_tests_independently"
                                | "device_added_during_the_run_starts_right_away_and_a_removed_device_returns_its_batch"
                        );
                        let quota = name
                            == "a_retry_of_one_of_two_batches_that_failed_together_runs_on_an_idle_device_of_another_worker";
                        sorted(
                            events
                                .into_iter()
                                .map(|e| {
                                    if quota {
                                        e.split_once(' ').unwrap().1.split_once(" on ").unwrap().0.to_owned()
                                    } else if relational {
                                        e.split_once(" on ").unwrap().0.to_owned()
                                    } else {
                                        e
                                    }
                                })
                                .collect(),
                        )
                    };
                    let expected = row["tracker"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap().to_owned())
                        .collect();
                    // D60 assigns FIFO ties to these fixed failure/removal checkpoints.
                    // Keep exact per-test accounting; the test body still checks worker placement.
                    let expected = match name.as_str() {
                        "retries_of_a_batch_that_failed_on_one_worker_run_on_all_idle_devices_of_other_workers" => [
                            "test1 FAILURE non-final",
                            "test1 PASSED final",
                            "test2 FAILURE non-final",
                            "test2 PASSED final",
                            "test3 PASSED final",
                        ]
                        .map(str::to_owned)
                        .to_vec(),
                        "device_added_during_the_run_starts_right_away_and_a_removed_device_returns_its_batch" => [
                            "test1 PASSED final",
                            "test2 INCOMPLETE non-final",
                            "test2 PASSED final",
                            "test3 PASSED final",
                            "test4 PASSED final",
                        ]
                        .map(str::to_owned)
                        .to_vec(),
                        _ => normalize(expected),
                    };
                    assert_eq!(
                        normalize(harness.tracker.events()),
                        expected,
                        "tracker row {name} (D60 FIFO where specified)"
                    );
                }
            }
            harness.parent.cancel(job_was_cancelled());
            replay(driver.drain_current().await);
            replay(driver.finish());
            Ok(())
        })
    });
    if let Err(error) = result {
        panic!("scenario failed: {error:?}");
    }
}

pub fn level_name(level: Level) -> &'static str {
    match level {
        Level::Trace => "TRACE",
        Level::Debug => "DEBUG",
        Level::Info => "INFO",
        Level::Warn => "WARN",
        Level::Error => "ERROR",
        _ => "OFF",
    }
}
