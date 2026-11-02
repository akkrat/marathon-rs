//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/device/DeviceActorBatchAccountingTest.kt
//! (scriptedDevice and the pool channel), vendor/vendor-test/src/main/kotlin/com/malinskiy/marathon/test/StubDevice.kt
//! (only the parameters DeviceActor's tests use; StubDevice itself is WP 3.10's) and
//! tools/oracle/module/src/main/kotlin/oracle/DeviceActorProbe.kt (the scenarios and their row shape).
//!
//! Every scenario runs on `run_virtual`: virtual time, explicit drains, no sleeps.
#![allow(dead_code)]
use jcompat::{
    file_manager::JavaPath,
    logging::{Level, Logger, MarathonLogging},
    writers::JavaString,
};
use marathon_config::{
    Configuration,
    binding::{BindingContext, Profile},
    loader::MarathonConfigurationLoader,
};
use marathon_core::{
    error::*,
    execution::{
        DevicePoolMessage, FromDevice,
        device::{DeviceActor, DeviceActorRef, DeviceEvent},
    },
    executor::{
        CoreExecutor,
        completion_logging::CompletionLog,
        hooks::{RuntimeHooks, Tape},
        shared::failure,
        test_support::ControlledReplay,
    },
    logging::LogRecord,
    model::*,
    runtime::*,
    time::*,
    vendor::{Device, DeviceRef, VendorContext},
};
use serde_json::{Map, Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    future::Future,
    rc::Rc,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    },
};

pub const LOGGER: &str = "DevicePool[test]_DeviceActor[serial-1]";

/// `Configuration.Builder(name = "", outputDir = File(""))` with the Stub vendor,
/// bound from a Marathonfile by the 2.4 loader. Binding blocks: call it before
/// entering the core executor.
pub fn configuration() -> Arc<Configuration> {
    static CONFIGURATION: OnceLock<Arc<Configuration>> = OnceLock::new();
    CONFIGURATION
        .get_or_init(|| {
            let context = BindingContext {
                profile: Profile::Marathonfile,
                marathonfile: JavaPath::from_text("/config/Marathonfile"),
                marathonfile_dir: JavaPath::from_text("/config"),
                process_dir: JavaPath::from_text("/"),
                environment: vec![],
                now: jcompat::jtime::instant_of(0, 0).unwrap(),
                analytics_tracking: false,
                bugsnag_reporting: false,
            };
            let marathonfile = b"name: \"\"\noutputDir: \"\"\nvendorConfiguration:\n  type: \"stub\"\n";
            let configuration = MarathonConfigurationLoader
                .parse_sync(marathonfile, &context)
                .unwrap_or_else(|e| panic!("test Marathonfile: {e:?}"));
            Arc::new(configuration)
        })
        .clone()
}

pub fn ise(message: &str) -> Failure {
    failure("java.lang.IllegalStateException", message)
}
/// `DeviceLostException(cause)`: Throwable(cause) takes `cause.toString()` as its message.
pub fn device_lost(cause: Failure) -> VendorError {
    VendorError::DeviceLost(Failure {
        class: "com.malinskiy.marathon.exceptions.DeviceLostException".into(),
        message: Some(cause.to_java_string()),
        cause: Some(Box::new(cause)),
        stacktrace: None,
    })
}
pub fn batch_failure(message: &str) -> VendorError {
    VendorError::BatchExecution(failure(
        "com.malinskiy.marathon.exceptions.TestBatchExecutionException",
        message,
    ))
}
/// `CancellationException(message)` thrown by a vendor.
pub fn vendor_cancellation(message: &str) -> VendorError {
    VendorError::Cancelled(marathon_core::executor::shared::requested(message))
}
/// `Job.cancel()` / `CompletableDeferred.cancel()` without a cause.
pub fn job_was_cancelled() -> Cancellation {
    Cancellation::Requested(failure(
        "kotlinx.coroutines.JobCancellationException",
        "Job was cancelled",
    ))
}
/// `Deferred.cancel()` on an `async` job.
pub fn deferred_coroutine_cancelled() -> Cancellation {
    Cancellation::Requested(failure(
        "kotlinx.coroutines.JobCancellationException",
        "DeferredCoroutine was cancelled",
    ))
}

pub fn passed(device: &DeviceRef, batch: &BatchRef) -> TestBatchResults {
    TestBatchResults {
        device: device.clone(),
        finished: vec![TestResult {
            test: batch.tests[0].clone(),
            device: device.0.info(),
            test_batch_id: batch.id.clone(),
            status: TestStatus::Passed,
            start_time: 0,
            end_time: 1,
            stacktrace: None,
            attachments: vec![],
        }],
        failed: vec![],
        uncompleted: vec![],
    }
}

// ---- The device ---------------------------------------------------------------------------

/// One `execute` call, as the scripted device's script sees it.
pub struct Execution {
    pub device: DeviceRef,
    pub batch: BatchRef,
    pub result: Arc<dyn Deferred<TestBatchResults>>,
    pub context: VendorContext,
}
pub type PrepareScript = Arc<dyn Fn(VendorContext) -> VendorFuture<'static, ()> + Send + Sync>;
pub type ExecuteScript = Arc<dyn Fn(Execution) -> VendorFuture<'static, ()> + Send + Sync>;

pub fn prepare_script<F, Fut>(f: F) -> PrepareScript
where
    F: Fn(VendorContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = VendorResult<()>> + Send + 'static,
{
    Arc::new(move |context| Box::pin(f(context)))
}
pub fn execute_script<F, Fut>(f: F) -> ExecuteScript
where
    F: Fn(Execution) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = VendorResult<()>> + Send + 'static,
{
    Arc::new(move |execution| Box::pin(f(execution)))
}

/// StubDevice's constructor parameters.
#[derive(Clone)]
pub struct Stub {
    pub prepare_time_millis: i64,
    pub test_time_millis: i64,
    pub crash_with_test_batch_exception: bool,
    pub crash_with_device_lost_exception: bool,
    pub report_results_before_crash: bool,
    pub execution_results: Vec<(Test, Vec<TestStatus>)>,
}
impl Default for Stub {
    fn default() -> Self {
        Self {
            prepare_time_millis: 5000,
            test_time_millis: 5000,
            crash_with_test_batch_exception: false,
            crash_with_device_lost_exception: false,
            report_results_before_crash: false,
            execution_results: vec![],
        }
    }
}
impl Stub {
    pub fn timed(prepare_time_millis: i64, test_time_millis: i64) -> Self {
        Self {
            prepare_time_millis,
            test_time_millis,
            ..Self::default()
        }
    }
}

/// StubDevice, or StubDevice(0, 0) delegating `prepare`/`execute` to a script, with
/// the call counts the probe records.
pub struct ScriptedDevice {
    me: Weak<ScriptedDevice>,
    logger: Logger,
    stub: Stub,
    prepare: Option<PrepareScript>,
    execute: Option<ExecuteScript>,
    execution_index: Mutex<HashMap<Test, usize>>,
    time_counter: AtomicI64,
    pub prepares: AtomicUsize,
    pub executes: AtomicUsize,
}
impl ScriptedDevice {
    pub fn new(stub: Stub, prepare: Option<PrepareScript>, execute: Option<ExecuteScript>) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            logger: MarathonLogging::default().logger("StubDevice", None),
            stub,
            prepare,
            execute,
            execution_index: Mutex::new(HashMap::new()),
            time_counter: AtomicI64::new(0),
            prepares: AtomicUsize::new(0),
            executes: AtomicUsize::new(0),
        })
    }
    pub fn stub(stub: Stub) -> Arc<Self> {
        Self::new(stub, None, None)
    }
    /// `scriptedDevice { … }`: StubDevice(0, 0) whose execute runs the script.
    pub fn scripted(script: ExecuteScript) -> Arc<Self> {
        Self::new(Stub::timed(0, 0), None, Some(script))
    }
    fn reference(&self) -> DeviceRef {
        match self.me.upgrade() {
            Some(me) => DeviceRef(me),
            None => unreachable!("a device is alive while it is called"),
        }
    }
    async fn stub_execute(
        &self,
        batch: BatchRef,
        result: Arc<dyn Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorResult<()> {
        context
            .clock
            .delay(self.stub.test_time_millis, context.cancellation.clone())
            .await?;
        let device = self.reference();
        if self.stub.report_results_before_crash {
            result.complete(Ok(TestBatchResults {
                device: device.clone(),
                finished: vec![],
                failed: vec![],
                uncompleted: vec![],
            }));
        }
        if self.stub.crash_with_test_batch_exception {
            return Err(batch_failure(
                "user requested the device to crash via crashWithTestBatchException parameter",
            ));
        }
        if self.stub.crash_with_device_lost_exception {
            return Err(device_lost(failure(
                "java.lang.RuntimeException",
                "user requested the device to crash via crashWithDeviceLostException parameter",
            )));
        }
        let mut results = vec![];
        for test in &batch.tests {
            let statuses = self
                .stub
                .execution_results
                .iter()
                .find(|(t, _)| t == test)
                .map(|(_, s)| s);
            let index = {
                let mut indices = self.execution_index.lock().unwrap();
                let index = indices.entry(test.clone()).or_insert(0);
                *index += 1;
                *index - 1
            };
            // `executionResults[it]!![i]`
            let Some(status) = statuses.and_then(|s| s.get(index)).copied() else {
                return Err(VendorError::Other(failure(
                    "java.lang.NullPointerException",
                    "executionResults",
                )));
            };
            let time = self.time_counter.fetch_add(1, Ordering::SeqCst);
            results.push(TestResult {
                test: test.clone(),
                device: self.info(),
                test_batch_id: batch.id.clone(),
                status,
                start_time: time,
                end_time: time + 1,
                stacktrace: None,
                attachments: vec![],
            });
        }
        let only = |status| results.iter().filter(|r| r.status == status).cloned().collect();
        result.complete(Ok(TestBatchResults {
            device,
            finished: only(TestStatus::Passed),
            failed: only(TestStatus::Failure),
            uncompleted: only(TestStatus::Incomplete),
        }));
        Ok(())
    }
}
impl Device for ScriptedDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            operating_system: OperatingSystem { version: "25".into() },
            serial_number: "serial-1".into(),
            model: "test".into(),
            manufacturer: "test".into(),
            network_state: NetworkState::Connected,
            device_features: vec![],
            healthy: true,
        }
    }
    fn abi(&self) -> JavaString {
        "test".into()
    }
    fn worker_id(&self) -> Option<JavaString> {
        None
    }
    fn logger(&self) -> &Logger {
        &self.logger
    }
    fn prepare<'a>(&'a self, _configuration: Arc<Configuration>, context: VendorContext) -> VendorFuture<'a, ()> {
        self.prepares.fetch_add(1, Ordering::SeqCst);
        match &self.prepare {
            Some(script) => script(context),
            None => Box::pin(async move {
                context
                    .clock
                    .delay(self.stub.prepare_time_millis, context.cancellation.clone())
                    .await
            }),
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
        self.executes.fetch_add(1, Ordering::SeqCst);
        match &self.execute {
            Some(script) => script(Execution {
                device: self.reference(),
                batch,
                result,
                context,
            }),
            None => Box::pin(self.stub_execute(batch, result, context)),
        }
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

// ---- The pool -----------------------------------------------------------------------------

pub type MessageHook = Box<dyn Fn(&FromDevice)>;
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PoolMode {
    Open,
    /// `channel.cancel()`: a send throws CancellationException.
    Cancelled,
    /// `channel.close()`: a send throws ClosedSendChannelException.
    Closed,
}
#[derive(Clone)]
pub enum Entry {
    Log(LogRecord),
    Pool { at: i64, message: FromDevice, via: String },
}

/// The probe's recording `SendChannel`. Each delivery first moves the core log
/// records queued so far into the trace, so logs and messages keep their order.
pub struct RecordingPool {
    core: Rc<CoreExecutor>,
    job: JobRef,
    mode: Cell<PoolMode>,
    entries: RefCell<Vec<Entry>>,
    /// Runs synchronously inside the delivering `send`/`trySend`.
    pub on_message: RefCell<Option<MessageHook>>,
}
impl RecordingPool {
    pub fn new(core: Rc<CoreExecutor>, mode: PoolMode) -> Rc<Self> {
        let job = core.job(JobKind::Ordinary, None);
        Rc::new(Self {
            core,
            job,
            mode: Cell::new(mode),
            entries: RefCell::new(vec![]),
            on_message: RefCell::new(None),
        })
    }
    pub fn flush_logs(&self) {
        let records = self.core.log_queue().take();
        self.entries.borrow_mut().extend(records.into_iter().map(Entry::Log));
    }
    fn record(&self, message: &DevicePoolMessage, via: &str) {
        self.flush_logs();
        let DevicePoolMessage::FromDevice(message) = message else {
            panic!("DeviceActor sent a non-device message");
        };
        self.entries.borrow_mut().push(Entry::Pool {
            at: self.core.clock().now_millis(),
            message: message.clone(),
            via: via.to_owned(),
        });
        if via == "send" || via == "trySend" {
            let hook = self.on_message.borrow();
            if let Some(hook) = hook.as_ref() {
                hook(message);
            }
        }
    }
    pub fn entries(&self) -> Vec<Entry> {
        self.flush_logs();
        self.entries.borrow().clone()
    }
    /// What `messages.receive()` returns, in order.
    pub fn received(&self) -> Vec<FromDevice> {
        self.entries()
            .into_iter()
            .filter_map(|entry| match entry {
                Entry::Pool { message, via, .. } if via == "send" || via == "trySend" => Some(message),
                _ => None,
            })
            .collect()
    }
}
impl Actor<DevicePoolMessage> for RecordingPool {
    fn job(&self) -> JobRef {
        self.job.clone()
    }
    fn is_closed_for_send(&self) -> bool {
        self.mode.get() != PoolMode::Open
    }
    fn try_send(&self, message: DevicePoolMessage) -> Result<(), SendError<DevicePoolMessage>> {
        if self.is_closed_for_send() {
            self.record(&message, "trySend-failed");
            Err(SendError { message, cause: None })
        } else {
            self.record(&message, "trySend");
            Ok(())
        }
    }
    fn send(&self, message: DevicePoolMessage) -> LocalFuture<'_, Result<(), SendError<DevicePoolMessage>>> {
        Box::pin(async move {
            match self.mode.get() {
                PoolMode::Open => {
                    self.record(&message, "send");
                    Ok(())
                }
                PoolMode::Cancelled => {
                    self.record(&message, "send-failed:java.util.concurrent.CancellationException");
                    Err(SendError { message, cause: None })
                }
                PoolMode::Closed => {
                    self.record(
                        &message,
                        "send-failed:kotlinx.coroutines.channels.ClosedSendChannelException",
                    );
                    Err(SendError {
                        message,
                        cause: Some(failure(
                            "kotlinx.coroutines.channels.ClosedSendChannelException",
                            "Channel was closed",
                        )),
                    })
                }
            }
        })
    }
    fn safe_send(&self, message: DevicePoolMessage) -> LocalFuture<'_, Result<(), SendError<DevicePoolMessage>>> {
        Box::pin(async move {
            if self.is_closed_for_send() {
                Ok(())
            } else {
                self.send(message).await
            }
        })
    }
    fn close(&self) {
        self.mode.set(PoolMode::Cancelled);
    }
}

// ---- A scenario ---------------------------------------------------------------------------

pub struct Run {
    pub core: Rc<CoreExecutor>,
    pub driver: Rc<ControlledReplay>,
    /// The pool's supervisor in production; a plain `Job()` in the Kotlin tests.
    pub parent: JobRef,
    pub pool: Rc<RecordingPool>,
    pub batch: BatchRef,
    pub hooks: Arc<RuntimeHooks>,
    pub barriers: RefCell<Option<Arc<dyn ReplayBarriers>>>,
    configuration: Arc<Configuration>,
    device: RefCell<Option<Arc<ScriptedDevice>>>,
    actor: RefCell<Option<DeviceActorRef>>,
    pub extra: RefCell<Map<String, Value>>,
}
impl Run {
    pub fn new(
        core: Rc<CoreExecutor>,
        driver: Rc<ControlledReplay>,
        configuration: Arc<Configuration>,
        mode: PoolMode,
        hooks: Arc<RuntimeHooks>,
    ) -> Rc<Self> {
        let test = Test {
            pkg: "pkg".into(),
            clazz: "clazz".into(),
            method: "method".into(),
            meta_properties: vec![],
        };
        Rc::new(Self {
            parent: core.job(JobKind::Ordinary, None),
            pool: RecordingPool::new(core.clone(), mode),
            core,
            driver,
            batch: Arc::new(TestBatch {
                tests: vec![test],
                id: "8a1b4c3e-0000-4000-8000-000000000001".into(),
            }),
            hooks,
            barriers: RefCell::new(None),
            configuration,
            device: RefCell::new(None),
            actor: RefCell::new(None),
            extra: RefCell::new(Map::new()),
        })
    }
    pub fn start(&self, device: Arc<ScriptedDevice>) -> DeviceActorRef {
        let barriers = self.barriers.borrow().clone().unwrap_or_else(|| self.hooks.barriers());
        let pool: Rc<dyn Actor<DevicePoolMessage>> = self.pool.clone();
        let actor = DeviceActor::spawn(
            self.core.clone(),
            DevicePoolId { name: "test".into() },
            pool,
            self.configuration.clone(),
            DeviceRef(device.clone()),
            self.parent.clone(),
            self.hooks.clone(),
            barriers,
        );
        *self.device.borrow_mut() = Some(device);
        *self.actor.borrow_mut() = Some(actor.clone());
        actor
    }
    pub fn actor(&self) -> DeviceActorRef {
        self.actor.borrow().clone().expect("started")
    }
    pub fn device(&self) -> DeviceRef {
        self.actor().device
    }
    pub fn scripted_device(&self) -> Arc<ScriptedDevice> {
        self.device.borrow().clone().expect("started")
    }
    pub fn send(&self, event: DeviceEvent) {
        assert!(self.actor().actor.try_send(event).is_ok(), "actor rejected an event");
    }
    pub async fn run_current(&self) {
        replay(self.driver.drain_current().await);
    }
    pub async fn advance_by(&self, millis: i64) {
        let now = self.core.clock().now_millis();
        replay(self.driver.advance_to(now + millis).await);
    }
    pub async fn advance_until_idle(&self) {
        replay(self.driver.advance_until_idle().await);
    }
    /// Initialize, drain to IsReady, then Execute and drain.
    pub async fn start_batch(&self, device: Arc<ScriptedDevice>) {
        self.start(device);
        self.send(DeviceEvent::Initialize);
        self.run_current().await;
        self.send(DeviceEvent::Execute(self.batch.clone()));
        self.run_current().await;
    }
    /// A core child of the actor job that fails, as a coroutine launched into the
    /// actor's scope does.
    pub fn fail_actor_job(&self, message: &'static str) {
        let job = self.actor().actor.job();
        self.core.spawn(
            job,
            Box::new(move |_| Box::pin(async move { Err(ise(message).into()) })),
        );
    }
    pub fn passed(&self) -> TestBatchResults {
        passed(&self.device(), &self.batch)
    }
    fn outcome(&self) -> Outcome {
        let device = self.scripted_device();
        let job = self.actor().actor.job().state();
        let parent = self.parent.state();
        let calls = (
            device.prepares.load(Ordering::SeqCst),
            device.executes.load(Ordering::SeqCst),
        );
        Outcome {
            entries: self.pool.entries(),
            calls,
            job,
            parent,
            now: self.core.clock().now_millis(),
            extra: self.extra.borrow().clone(),
            completion_logs: self.core.take_completion_logs(),
            device: self.device(),
            batch: self.batch.clone(),
        }
    }
}

pub fn replay<T>(result: ClockResult<T>) -> T {
    result.unwrap_or_else(|e| panic!("replay failure: {e:?}"))
}

/// What a scenario leaves behind; `Send`, so it leaves the core thread.
pub struct Outcome {
    pub entries: Vec<Entry>,
    pub calls: (usize, usize),
    pub job: JobState,
    pub parent: JobState,
    pub now: i64,
    pub extra: Map<String, Value>,
    /// 3.1's loggerless coroutine-handler records (`newCoroutineExceptionHandler`).
    pub completion_logs: Vec<CompletionLog>,
    pub device: DeviceRef,
    pub batch: BatchRef,
}
impl Outcome {
    pub fn received(&self) -> Vec<FromDevice> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Pool { message, via, .. } if via == "send" || via == "trySend" => Some(message.clone()),
                _ => None,
            })
            .collect()
    }
    pub fn logs(&self) -> Vec<LogRecord> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Log(record) => Some(record.clone()),
                _ => None,
            })
            .collect()
    }
    pub fn describe(&self) -> Vec<String> {
        self.received().iter().map(|m| describe(m, &self.batch)).collect()
    }
    pub fn job_completed(&self) -> bool {
        matches!(self.job, JobState::Completed(_))
    }
}

pub fn describe(message: &FromDevice, batch: &BatchRef) -> String {
    match message {
        FromDevice::IsReady(_) => "IsReady".into(),
        FromDevice::Terminated(_) => "Terminated".into(),
        FromDevice::CompletedTestBatch(_, results) => format!(
            "CompletedTestBatch(finished={}, failed={}, uncompleted={})",
            results.finished.len(),
            results.failed.len(),
            results.uncompleted.len()
        ),
        FromDevice::ReturnTestBatch(_, returned, reason) => format!(
            "ReturnTestBatch(same batch={}, reason={})",
            Arc::ptr_eq(returned, batch),
            reason.to_string_lossy()
        ),
    }
}

/// Runs one scenario body on a fresh virtual executor and checks quiescence.
pub fn scenario(mode: PoolMode, body: impl FnOnce(Rc<Run>) -> LocalFuture<'static, ()> + Send + 'static) -> Outcome {
    scenario_with(mode, None, body)
}
/// With a replay tape (barriers); the body checks `hooks.verify_consumed()` itself.
pub fn scenario_with(
    mode: PoolMode,
    tape: Option<Tape>,
    body: impl FnOnce(Rc<Run>) -> LocalFuture<'static, ()> + Send + 'static,
) -> Outcome {
    let configuration = configuration();
    let result = CoreExecutor::run_virtual(move |core, driver| {
        Box::pin(async move {
            let hooks = match tape {
                Some(tape) => RuntimeHooks::taped(tape, &core.clock()),
                None => RuntimeHooks::live(),
            };
            let run = Run::new(core, driver.clone(), configuration, mode, hooks);
            body(run.clone()).await;
            let outcome = run.outcome();
            replay(driver.finish());
            // The Kotlin tests' teardown.
            run.parent.cancel(job_was_cancelled());
            replay(driver.drain_current().await);
            Ok(outcome)
        })
    });
    match result {
        Ok(outcome) => outcome,
        Err(error) => panic!("scenario failed: {error:?}"),
    }
}

// ---- The probe's row ----------------------------------------------------------------------

fn clean(text: &JavaString, batch: &BatchRef) -> String {
    text.to_string_lossy()
        .replace(&batch.id.to_string_lossy(), "<batch>")
        .split('\n')
        .filter(|line| !line.starts_with("\tat ") && !line.starts_with("\t... "))
        .collect::<Vec<_>>()
        .join("\n")
}
fn level(level: Level) -> &'static str {
    match level {
        Level::Error => "ERROR",
        Level::Warn => "WARN",
        Level::Info => "INFO",
        Level::Debug => "DEBUG",
        _ => "TRACE",
    }
}
fn test_text(result: &TestResult) -> String {
    let t = &result.test;
    format!(
        "Test(pkg={}, clazz={}, method={}, metaProperties=[])",
        t.pkg.to_string_lossy(),
        t.clazz.to_string_lossy(),
        t.method.to_string_lossy()
    )
}
pub fn throwable(cause: Option<&Failure>, batch: &BatchRef) -> Value {
    let mut chain = vec![];
    let mut current = cause;
    while let Some(failure) = current {
        chain.push(json!({
            "class": failure.class.to_string_lossy(),
            "message": failure.message.as_ref().map(|m| clean(m, batch)),
        }));
        current = failure.cause.as_deref();
    }
    Value::Array(chain)
}
fn job_json(state: &JobState) -> (bool, bool, bool) {
    match state {
        JobState::Active | JobState::Completing => (true, false, false),
        JobState::Cancelling(_) => (false, false, true),
        JobState::Completed(Completion::Success) => (false, true, false),
        JobState::Completed(_) => (false, true, true),
    }
}

impl Outcome {
    /// The row DeviceActorProbe writes for this scenario.
    pub fn row(&self, name: &str) -> Value {
        let batch = &self.batch;
        let trace: Vec<Value> = self
            .entries
            .iter()
            .map(|entry| match entry {
                Entry::Log(record) => json!({
                    "t": record.at_millis,
                    "log": {
                        "logger": record.logger,
                        "level": level(record.level),
                        "message": clean(&record.message, batch),
                        "throwable": throwable(record.cause.as_ref(), batch),
                    },
                }),
                Entry::Pool { at, message, via } => {
                    let mut body = Map::new();
                    let kind = match message {
                        FromDevice::IsReady(_) => "IsReady",
                        FromDevice::CompletedTestBatch(..) => "CompletedTestBatch",
                        FromDevice::ReturnTestBatch(..) => "ReturnTestBatch",
                        FromDevice::Terminated(_) => "Terminated",
                    };
                    body.insert("type".into(), kind.into());
                    body.insert("via".into(), via.as_str().into());
                    match message {
                        FromDevice::CompletedTestBatch(device, results) => {
                            let texts = |r: &[TestResult]| Value::from(r.iter().map(test_text).collect::<Vec<_>>());
                            body.insert("finished".into(), texts(&results.finished));
                            body.insert("failed".into(), texts(&results.failed));
                            body.insert("uncompleted".into(), texts(&results.uncompleted));
                            body.insert("sameDevice".into(), (results.device == *device).into());
                        }
                        FromDevice::ReturnTestBatch(_, returned, reason) => {
                            body.insert("sameBatch".into(), Arc::ptr_eq(returned, batch).into());
                            body.insert("reason".into(), clean(reason, batch).into());
                        }
                        _ => {}
                    }
                    json!({ "t": at, "pool": body })
                }
            })
            .collect();
        let (active, completed, cancelled) = job_json(&self.job);
        let (parent_active, _, parent_cancelled) = job_json(&self.parent);
        json!({
            "scenario": name,
            "trace": trace,
            "calls": { "prepare": self.calls.0, "execute": self.calls.1 },
            "job": { "active": active, "completed": completed, "cancelled": cancelled },
            "parent": { "active": parent_active, "cancelled": parent_cancelled },
            "t": self.now,
            "extra": self.extra,
        })
    }
}
