//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/FakeSimulatorWorker.kt
#![allow(dead_code)]
use jcompat::jtime::{Duration, Instant, duration_of, instant_of};
use marathon_apple::cmd::{BaseCommand, SessionScope, failure};
use marathon_core::{
    error::{Cancellation, Failure, VendorError, VendorResult},
    runtime::{
        CancellationSignal, VendorClock, VendorFuture, VendorRuntime, VendorTask, VendorTaskBody, VendorTimeoutBody,
    },
    time::{ClockResult, MonotonicClock, ReplayHooks, WallClock},
    vendor::VendorContext,
};
use marathon_error::{FailureKind, InjectedKind};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::watch;

pub fn duration(millis: i64) -> Duration {
    duration_of(millis.div_euclid(1000), millis.rem_euclid(1000) * 1_000_000).unwrap()
}
#[derive(Default)]
pub struct Runtime;
struct Task(watch::Sender<Option<VendorResult<()>>>);
impl VendorTask for Task {
    fn cancel(&self, _: Cancellation) { /* test tasks use their injected session signal */
    }
    fn join(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let mut rx = self.0.subscribe();
            loop {
                if let Some(result) = rx.borrow().clone() {
                    return result;
                }
                rx.changed().await.map_err(|e| {
                    VendorError::Other(failure(FailureKind::Injected(InjectedKind::TestTask), e.to_string()))
                })?;
            }
        })
    }
}
impl VendorRuntime for Runtime {
    fn spawn_cancellable(&self, body: VendorTaskBody) -> Arc<dyn VendorTask> {
        // Test tasks ignore cancel(), as in spawn; the body still gets its own signal.
        self.spawn(body(Arc::new(SessionScope::default())))
    }
    fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn VendorTask> {
        let task = Arc::new(Task(watch::channel(None).0));
        let complete = task.clone();
        tokio::spawn(async move {
            complete.0.send_replace(Some(body.await));
        });
        task
    }
    fn spawn_blocking(&self, body: Box<dyn FnOnce() -> VendorResult<()> + Send>) -> Arc<dyn VendorTask> {
        let task = Arc::new(Task(watch::channel(None).0));
        let complete = task.clone();
        tokio::task::spawn_blocking(move || {
            complete.0.send_replace(Some(body()));
        });
        task
    }
}
pub struct Clock {
    start: tokio::time::Instant,
}
impl Default for Clock {
    fn default() -> Self {
        Self {
            start: tokio::time::Instant::now(),
        }
    }
}
impl Clock {
    pub fn now(&self) -> i64 {
        self.start.elapsed().as_millis() as i64
    }
}
static SCOPE: AtomicU64 = AtomicU64::new(1);
impl VendorClock for Clock {
    fn now_millis(&self) -> i64 {
        self.now()
    }
    fn delay(&self, millis: i64, cancellation: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if let Some(reason) = cancellation.reason() {
                return Err(VendorError::Cancelled(reason));
            }
            if millis <= 0 {
                return Ok(());
            }
            tokio::select! { biased;
                reason = cancellation.cancelled() => Err(VendorError::Cancelled(reason)),
                _ = tokio::time::sleep(std::time::Duration::from_millis(millis as u64)) => Ok(()),
            }
        })
    }
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let scope = SCOPE.fetch_add(1, Ordering::SeqCst);
            let child = Arc::new(SessionScope::default());
            let expired = Cancellation::Timeout {
                scope,
                detail: failure(FailureKind::Injected(InjectedKind::TimeoutCancellation), "timeout"),
            };
            if millis <= 0 {
                return Err(VendorError::Cancelled(expired));
            }
            let mut future = body(child.clone());
            tokio::select! { biased;
                reason = parent.cancelled() => { child.cancel(reason.clone()); let _ = future.await; Err(VendorError::Cancelled(reason)) },
                result = &mut future => result,
                _ = tokio::time::sleep(std::time::Duration::from_millis(millis as u64)) => {
                    child.cancel(expired.clone()); let _ = future.await; Err(VendorError::Cancelled(expired))
                },
            }
        })
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, Option<()>> {
        Box::pin(async move {
            // Remember this body's signal, so only this invocation's expiry is caught.
            let own = Arc::new(Mutex::new(None));
            let observed = own.clone();
            let result = self
                .with_timeout(
                    parent,
                    millis,
                    Box::new(move |child| {
                        *observed.lock().unwrap() = Some(child.clone());
                        body(child)
                    }),
                )
                .await;
            match result {
                Ok(()) => Ok(Some(())),
                Err(VendorError::Cancelled(reason @ Cancellation::Timeout { .. }))
                    if millis <= 0
                        || own
                            .lock()
                            .unwrap()
                            .as_ref()
                            .and_then(|s: &Arc<dyn CancellationSignal>| s.reason())
                            .as_ref()
                            == Some(&reason) =>
                {
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        })
    }
}
impl WallClock for Clock {
    fn millis(&self, label: &'static str) -> ClockResult<i64> {
        let _ = label;
        Ok(self.now())
    }
    fn instant(&self, _: &'static str) -> ClockResult<Instant> {
        Ok(instant_of(0, self.now() * 1_000_000).unwrap())
    }
}
impl MonotonicClock for Clock {
    fn nanos(&self, _: &'static str) -> ClockResult<i64> {
        Ok(self.now() * 1_000_000)
    }
}
impl ReplayHooks for Clock {
    fn draw_int(&self, _: &'static str) -> ClockResult<i32> {
        unreachable!()
    }
    fn choose_key(&self, label: &'static str, keys: &[String]) -> ClockResult<String> {
        assert_eq!(label, "ios.AppleSimulatorProvider.borrow");
        Ok(keys[0].clone())
    }
    fn allocate_id(&self, label: &'static str) -> ClockResult<String> {
        assert_eq!(label, "ios.SimulatorProvisioner.create");
        Ok(format!(
            "00000000-0000-0000-0000-{:012}",
            IDS.fetch_add(1, Ordering::SeqCst)
        ))
    }
    fn verify_consumed(&self) -> ClockResult<()> {
        Ok(())
    }
}
pub fn context() -> VendorContext {
    let clock = Arc::new(Clock::default());
    VendorContext {
        cancellation: Arc::new(SessionScope::default()),
        clock: clock.clone(),
        hooks: clock,
    }
}
pub fn fake() -> BaseCommand {
    BaseCommand::new(1, Arc::new(Runtime))
}
pub async fn collect(channel: &marathon_apple::cmd::channel::LineChannel) -> VendorResult<Vec<String>> {
    let mut result = Vec::new();
    while let Some(line) = channel.receive().await? {
        result.push(line);
    }
    Ok(result)
}

pub async fn watchdog<T>(future: impl std::future::Future<Output = T>) -> T {
    bounded(60, future).await
}

pub async fn bounded<T>(seconds: u64, future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(seconds), future)
        .await
        .unwrap_or_else(|_| panic!("test exceeded the Kotlin {seconds}-second watchdog"))
}

static IDS: AtomicU64 = AtomicU64::new(1);

use marathon_apple::{
    bin::{AppleBinaryEnvironment, Tools},
    cmd::remote::ssh::sshj::config::SshLogging,
    device::connection_factory::{Bridge, Executor},
};
use marathon_config::{
    devices::{SimulatorProfile, Transport},
    vendor::{AppleTimeoutParameters, apple_timeout_configuration},
};
use marathon_core::{
    executor::shared::Signal,
    model::*,
    vendor::{Device, DeviceRef},
};
use marathon_ios::device::{error::*, simulator_factory::*, simulator_provisioner::*, worker_provisioning_context::*};
use marathon_test_support::apple::{FakeSimulatorWorker, fake_simulator_worker::IPHONE};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize};
use tokio::sync::Semaphore;

pub type TestResult = Result<(), Box<dyn std::error::Error>>;
pub fn error(message: &str) -> VendorError {
    VendorError::Other(failure(FailureKind::IllegalState, message))
}
pub fn worker(host: &str) -> Transport {
    Transport::Ssh {
        addr: host.into(),
        port: 22,
        authentication: None,
        check_reachability: true,
    }
}
pub fn profile() -> SimulatorProfile {
    SimulatorProfile {
        device_type_id: IPHONE.into(),
        runtime_id: None,
        new_name_prefix: "test".into(),
    }
}
pub type Logs = Arc<Mutex<Vec<(String, marathon_logging::Level, String, Option<Failure>)>>>;
pub fn logging() -> (SshLogging, Logs) {
    let records = Logs::default();
    let sink = records.clone();
    let mut registry = marathon_logging::LogSettings::default();
    registry.configure(false, marathon_logging::Vendor::Apple { compact_output: false });
    (
        SshLogging::new(
            Arc::new(Mutex::new(registry)),
            Arc::new(move |logger, level, message, cause| {
                sink.lock()
                    .unwrap()
                    .push((logger.name().to_owned(), level, message, cause));
            }),
        ),
        records,
    )
}
pub fn bin(executor: Executor, logging: SshLogging) -> Arc<AppleBinaryEnvironment> {
    Arc::new(AppleBinaryEnvironment::with_tools(
        Tools {
            executor,
            timeouts: Arc::new(apple_timeout_configuration(AppleTimeoutParameters::empty())),
            runtime: Arc::new(Runtime),
            logging,
        },
        5000,
        5000,
    ))
}
pub struct BridgeMock;
impl marathon_apple::cmd::FileBridge for BridgeMock {
    fn send<'a>(&'a self, _: &'a std::path::Path, _: &'a str) -> VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    fn receive<'a>(&'a self, _: &'a str, _: &'a std::path::Path) -> VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
}
pub struct TestDevice {
    pub serial: String,
    pub disposed: AtomicUsize,
    pub dispose_error: Mutex<Option<VendorError>>,
    logger: marathon_logging::Component,
}
impl TestDevice {
    pub fn new(serial: &str) -> Arc<Self> {
        Arc::new(Self {
            serial: serial.into(),
            disposed: AtomicUsize::new(0),
            dispose_error: Mutex::default(),
            logger: marathon_logging::LogSettings::default().component("TestDevice"),
        })
    }
    pub fn device(self: &Arc<Self>) -> DeviceRef {
        DeviceRef(self.clone())
    }
}
impl Device for TestDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            operating_system: OperatingSystem { version: "18.6".into() },
            serial_number: self.serial.clone(),
            model: "model".into(),
            manufacturer: "Apple".into(),
            network_state: NetworkState::Connected,
            device_features: vec![],
            healthy: true,
        }
    }
    fn abi(&self) -> String {
        "arm64".into()
    }
    fn worker_id(&self) -> Option<String> {
        None
    }
    fn component(&self) -> &marathon_logging::Component {
        &self.logger
    }
    fn prepare<'a>(&'a self, _: Arc<marathon_config::Configuration>, _: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn execute<'a>(
        &'a self,
        _: Arc<marathon_config::Configuration>,
        _: DevicePoolId,
        _: BatchRef,
        _: Arc<dyn marathon_core::runtime::Deferred<TestBatchResults>>,
        _: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.disposed.fetch_add(1, Ordering::SeqCst);
            self.dispose_error.lock().unwrap().clone().map_or(Ok(()), Err)
        })
    }
}
pub struct Connections {
    pub executor: Executor,
    pub released: AtomicUsize,
    pub connected: AtomicUsize,
    pub unavailable: AtomicBool,
    pub release_error: Mutex<Option<VendorError>>,
    pub trace: Arc<Mutex<Vec<String>>>,
    pub connect_error: Mutex<Option<marathon_apple::device::connection_factory::ConnectionError>>,
}
impl ProvisioningConnections for Connections {
    fn connect(
        &self,
        _: Transport,
        _: VendorContext,
    ) -> marathon_apple::device::connection_factory::ConnectionFuture<'_, (Option<Executor>, Bridge)> {
        Box::pin(async move {
            self.trace.lock().unwrap().push("acquire".into());
            self.connected.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = self.connect_error.lock().unwrap().clone() {
                return Err(error);
            }
            Ok((
                (!self.unavailable.load(Ordering::SeqCst)).then(|| self.executor.clone()),
                Arc::new(BridgeMock) as Bridge,
            ))
        })
    }
    fn release(&self, executor: Executor) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            assert!(Arc::ptr_eq(&executor, &self.executor));
            self.trace.lock().unwrap().push("release".into());
            self.released.fetch_add(1, Ordering::SeqCst);
            self.release_error.lock().unwrap().clone().map_or(Ok(()), Err)
        })
    }
}
pub struct NoSetup;
impl SimulatorSetup for NoSetup {
    fn setup<'a>(
        &'a self,
        _: &'a marathon_apple::bin::xcrun::simctl::model::SimctlDevice,
        _: Arc<WorkerProvisioningContext>,
        _: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
pub struct Creating {
    pub trace: Arc<Mutex<Vec<String>>>,
    pub active: AtomicUsize,
    pub peak: AtomicUsize,
    pub started: AtomicUsize,
    pub fail_first: AtomicBool,
    pub cancel_setup: AtomicBool,
    pub block: AtomicBool,
    pub ignore_cancellation: AtomicBool,
    pub gate: Semaphore,
    pub devices: Mutex<Vec<Arc<TestDevice>>>,
}
impl Default for Creating {
    fn default() -> Self {
        Self {
            trace: Arc::default(),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            fail_first: AtomicBool::new(false),
            cancel_setup: AtomicBool::new(false),
            block: AtomicBool::new(false),
            ignore_cancellation: AtomicBool::new(false),
            gate: Semaphore::new(0),
            devices: Mutex::default(),
        }
    }
}
impl SimulatorCreating for Creating {
    type Handle = Arc<TestDevice>;
    fn create<'a>(
        &'a self,
        _: Transport,
        _: Executor,
        _: Bridge,
        udid: String,
        _: Arc<WorkerProvisioningContext>,
        caller: VendorContext,
    ) -> ProvisioningFuture<'a, ConstructedSimulator<Self::Handle>> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(format!("setup:{udid}"));
            let index = self.started.fetch_add(1, Ordering::SeqCst);
            let n = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(n, Ordering::SeqCst);
            let result = async {
                if self.fail_first.load(Ordering::SeqCst) && index == 0 { return Err(error("setup failed").into()); }
                if self.cancel_setup.load(Ordering::SeqCst) { tokio::task::yield_now().await; return Err(VendorError::Cancelled(marathon_apple::cmd::cancelled("cancelled setup")).into()); }
                if self.block.load(Ordering::SeqCst) {
                    if self.ignore_cancellation.load(Ordering::SeqCst) { self.gate.acquire().await.map_err(|e| error(&e.to_string()))?.forget(); }
                    else { tokio::select! { biased; c = caller.cancellation.cancelled() => return Err(VendorError::Cancelled(c).into()), p = self.gate.acquire() => p.map_err(|e| error(&e.to_string()))?.forget() } }
                } else { caller.clock.delay(100, caller.cancellation.clone()).await?; }
                let device = TestDevice::new(&udid); self.devices.lock().unwrap().push(device.clone()); Ok(ConstructedSimulator { device: device.device(), setup: Arc::new(NoSetup), handle: device })
            }.await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            result
        })
    }
}
pub struct Harness {
    pub trace: Arc<Mutex<Vec<String>>>,
    pub created: Arc<AtomicUsize>,
    pub commands: Arc<CreatingCommands>,
    pub worker: Arc<FakeSimulatorWorker>,
    pub connections: Arc<Connections>,
    pub creating: Arc<Creating>,
    pub provisioner: Arc<SimulatorProvisioner<Arc<TestDevice>>>,
    pub bin: Arc<AppleBinaryEnvironment>,
    pub context: VendorContext,
    pub cycle: Arc<WorkerProvisioningContext>,
    pub registrations: Arc<Mutex<Vec<(String, DeviceRef, i64)>>>,
    pub clock: Arc<Clock>,
    pub signal: Arc<Signal>,
    pub logs: Logs,
}
impl Harness {
    pub fn new() -> Self {
        Self::with_concurrency(2)
    }
    pub fn with_concurrency(limit: i32) -> Self {
        let (logging, logs) = logging();
        let clock = Arc::new(Clock::default());
        let signal = Signal::new();
        let context = VendorContext {
            cancellation: signal.clone(),
            clock: clock.clone(),
            hooks: clock.clone(),
        };
        let worker = Arc::new(FakeSimulatorWorker::new(
            Arc::new(Runtime),
            &mut logging.registry.lock().unwrap(),
        ));
        let trace = Arc::new(Mutex::new(Vec::new()));
        let created = Arc::new(AtomicUsize::new(0));
        let commands = Arc::new(CreatingCommands {
            created: created.clone(),
            trace: trace.clone(),
            simulators: worker.simulators.clone(),
            passthrough: AtomicBool::new(false),
        });
        let events = trace.clone();
        *worker.before.lock().unwrap() = Arc::new(move |args| {
            let events = events.clone();
            Box::pin(async move {
                if args.get(2).is_some_and(|s| s == "delete") {
                    events.lock().unwrap().push(format!("delete:{}", args[3]));
                }
                Ok(None)
            })
        });
        let connections = Arc::new(Connections {
            trace: trace.clone(),
            executor: worker.clone(),
            released: AtomicUsize::new(0),
            connected: AtomicUsize::new(0),
            unavailable: AtomicBool::new(false),
            release_error: Mutex::default(),
            connect_error: Mutex::default(),
        });
        let creating = Arc::new(Creating {
            trace: trace.clone(),
            ..Creating::default()
        });
        let registrations = Arc::new(Mutex::new(Vec::new()));
        let registered = registrations.clone();
        let regclock = clock.clone();
        let events = trace.clone();
        let provisioner = Arc::new(
            SimulatorProvisioner::new(
                connections.clone(),
                creating.clone(),
                Arc::new(move |id, device| {
                    assert_eq!(device.device, device.handle.device());
                    events.lock().unwrap().push(format!("register:{}", id.to_owned()));
                    registered.lock().unwrap().push((id, device.device, regclock.now()));
                    Box::pin(async { Ok(()) })
                }),
                limit,
                logging.clone(),
            )
            .unwrap()
            .with_simulator_commands(commands.clone()),
        );
        let bin = bin(worker.clone(), logging);
        let cycle = Arc::new(WorkerProvisioningContext::new(Transport::Local, clock.clone()));
        Self {
            trace,
            created,
            commands,
            worker,
            connections,
            creating,
            provisioner,
            bin,
            context,
            cycle,
            registrations,
            clock,
            signal,
            logs,
        }
    }
    pub async fn profiles(&self, n: usize) -> ProvisioningResult<Vec<FailedSetup>> {
        self.provisioner
            .provision(
                marathon_ios::device::provisioning_plan::ProvisioningPlan {
                    needs_provisioning: vec![profile(); n],
                    ..Default::default()
                },
                worker("worker-1"),
                self.bin.clone(),
                self.cycle.clone(),
                self.context.clone(),
            )
            .await
    }
    pub async fn existing(&self, ids: &[&str]) -> ProvisioningResult<()> {
        self.provisioner
            .existing(
                ids.iter().map(|s| s.to_string()),
                worker("worker-1"),
                self.context.clone(),
            )
            .await
            .map(|_| ())
    }
    pub fn registered(&self) -> usize {
        self.registrations.lock().unwrap().len()
    }
    pub fn deletes(&self) -> usize {
        self.worker
            .commands
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.starts_with(&["xcrun".into(), "simctl".into(), "delete".into()]))
            .count()
    }
}
pub struct NanoClock(pub AtomicI64);
impl MonotonicClock for NanoClock {
    fn nanos(&self, label: &'static str) -> ClockResult<i64> {
        assert_eq!(label, NANO_TIME);
        Ok(self.0.load(Ordering::SeqCst))
    }
}
pub fn inventory(udid: &str) -> marathon_apple::bin::xcrun::simctl::model::SimctlListDevicesOutput {
    marathon_apple::bin::xcrun::simctl::model::decode_list_devices_output(&format!(r#"{{"devicetypes":[],"runtimes":[],"devices":{{"r":[{{"udid":"{udid}","name":"{udid}","state":"Shutdown","isAvailable":true}}]}}}}"#)).unwrap()
}
pub fn facts(shareable: bool) -> WorkerFacts {
    WorkerFacts {
        env: vec![],
        xcode_version: marathon_apple::model::XcodeVersion::new(16, 4, 0),
        arch: "arm64".into(),
        features: vec![],
        supports_transcoding: false,
        shareable,
    }
}

pub fn witness(name: &str) -> serde_json::Value {
    include_str!("../../../../fixtures/oracle/provisioning/probes.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|row| row["case"] == name)
        .unwrap()["value"]
        .clone()
}
pub fn inventory_json(udid: &str) -> String {
    serde_json::json!({"devicetypes":[],"runtimes":[],"devices":{"r":[{"udid":udid,"name":udid,"state":"Shutdown","isAvailable":true}]}}).to_string()
}

pub struct CreatingCommands {
    pub created: Arc<AtomicUsize>,
    pub trace: Arc<Mutex<Vec<String>>>,
    pub simulators: marathon_test_support::apple::fake_simulator_worker::Simulators,
    pub passthrough: AtomicBool,
}
impl SimulatorCommands for CreatingCommands {
    fn create<'a>(
        &'a self,
        bin: &'a AppleBinaryEnvironment,
        name: &'a str,
        kind: &'a str,
        runtime: Option<&'a str>,
        caller: &'a VendorContext,
    ) -> ProvisioningFuture<'a, Option<String>> {
        Box::pin(async move {
            if self.passthrough.load(Ordering::SeqCst) {
                return SimctlCommands.create(bin, name, kind, runtime, caller).await;
            }
            let id = format!("new-{}", self.created.fetch_add(1, Ordering::SeqCst) + 1);
            self.trace.lock().unwrap().push(format!("create:{id}"));
            self.simulators.lock().unwrap().insert(
                id.clone(),
                Arc::new(marathon_test_support::apple::fake_simulator_worker::Simulator::new(
                    &id,
                    kind,
                    runtime.unwrap_or(marathon_test_support::apple::fake_simulator_worker::IOS),
                )),
            );
            Ok(Some(id))
        })
    }
}
