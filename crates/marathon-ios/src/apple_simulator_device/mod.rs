//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt

mod boot;
mod execution;
mod listeners;
pub use listeners::{MinimalSimulatorListenerFactory, SimulatorListenerFactory, SimulatorListeners};
mod prepare;
mod setup;
mod worker;

use crate::device::{
    error::{ProvisioningResult, clock_error},
    simulator_factory::{ConstructedSimulator, SimulatorConstruction, SimulatorConstructor, SimulatorSetup},
    timing::ProvisioningTiming,
    worker_provisioning_context::WorkerProvisioningContext,
};
use jcompat::{
    file_manager::FileManager,
    logging::{Level, Logger},
    writers::JavaString,
};
use marathon_apple::{
    apple_device::{AppleDevice, apple_worker_id, storage_path_for},
    apple_test_bundle_identifier::AppleTestBundleIdentifier,
    artifact_processor::ArtifactProcessor,
    bin::{AppleBinaryEnvironment, xcrun::simctl::model::SimctlDevice},
    cmd::{remote::ssh::sshj::config::SshLogging, retained_tasks::RetainedTasks, shared_transfers::SharedTransfers},
    device::connection_factory::{Bridge, Executor},
    execute_test_request::LineListeners,
    model::{AppleTestBundle, Sdk, XcodeVersion},
    plist::PlistDict,
    remote_file_manager::RemoteFileManager,
    worker_commands::ExecutorWorkerCommands,
};
use marathon_config::{Configuration, devices::Transport, vendor::IosConfiguration};
use marathon_core::{
    analytics::Track,
    error::{VendorError, VendorResult},
    executor::shared::SharedDeferred,
    listener::{LineListener, LogProducer},
    model::{BatchRef, DeviceInfo, DevicePoolId, NetworkState, OperatingSystem, TestBatchResults},
    runtime::{Deferred, VendorFuture, VendorRuntime},
    time::{MonotonicClock, Timer, WallClock},
    vendor::{Device, DeviceRef, VendorContext},
};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

#[derive(Clone, Debug)]
pub struct SimulatorTimeouts {
    pub session_close_millis: i64,
    pub cancelled_batch_millis: i64,
    pub boot_retry_pause_millis: i64,
    pub boot_poll_interval_millis: i64,
}
impl Default for SimulatorTimeouts {
    fn default() -> Self {
        Self {
            session_close_millis: 60_000,
            cancelled_batch_millis: 70_000,
            boot_retry_pause_millis: 10_000,
            boot_poll_interval_millis: 1_000,
        }
    }
}
/// Run-owned services captured by the 5.4 construction adapter. Construction performs no I/O.
#[derive(Clone)]
pub struct SimulatorServices {
    pub configuration: Arc<Configuration>,
    pub vendor: Arc<IosConfiguration>,
    pub runtime: Arc<dyn VendorRuntime>,
    pub context: VendorContext,
    pub monotonic: Arc<dyn MonotonicClock>,
    pub wall: Arc<dyn WallClock>,
    pub timer: Arc<dyn Timer>,
    pub files: Arc<FileManager>,
    pub output: Arc<dyn marathon_core::output::OutputFiles>,
    pub logs: marathon_core::logging::LogSender,
    pub result_wall: marathon_core::logging::WallSource,
    pub listeners: Arc<dyn SimulatorListenerFactory>,
    pub identifier: Arc<AppleTestBundleIdentifier>,
    pub artifacts: Arc<ArtifactProcessor>,
    pub track: Track,
    pub logging: SshLogging,
    pub timeouts: SimulatorTimeouts,
}
pub struct AppleSimulatorConstructor(pub SimulatorServices);
impl SimulatorConstructor for AppleSimulatorConstructor {
    type Handle = Arc<AppleSimulatorDevice>;
    fn construct(&self, input: SimulatorConstruction) -> ProvisioningResult<ConstructedSimulator<Self::Handle>> {
        let device = AppleSimulatorDevice::new(input, self.0.clone());
        Ok(ConstructedSimulator {
            handle: device.clone(),
            device: DeviceRef(device.clone()),
            setup: device,
        })
    }
}
pub type Admission = Arc<dyn Fn() -> VendorResult<Option<String>> + Send + Sync>;
struct State {
    info: DeviceInfo,
    abi: JavaString,
    env: Option<Vec<(JavaString, JavaString)>>,
    home: Option<String>,
    root_path: String,
    log_file: String,
    descriptor_path: String,
    descriptor: Option<PlistDict>,
    runtime_version: Option<String>,
    runtime_build_version: String,
    version: String,
    device_type: String,
    xcode_version: Option<XcodeVersion>,
    supports_transcoding: bool,
    bundle: Option<Arc<AppleTestBundle>>,
    disposal: Option<Arc<SharedDeferred<()>>>,
    lost: bool,
    retired: bool,
    previous_batch_finished_at: Option<i64>,
    admission: Option<Admission>,
    policy: marathon_apple::device::batch_failure_policy::BatchFailurePolicy,
    running: Option<Arc<execution::BatchJob>>,
    last_session: Option<Arc<marathon_apple::xcodebuild_session::XcodebuildSession>>,
}
pub struct AppleSimulatorDevice {
    me: Weak<Self>,
    udid: String,
    serial: String,
    transport: Transport,
    sdk: Sdk,
    bin: Arc<AppleBinaryEnvironment>,
    executor: Executor,
    bridge: Bridge,
    shared_transfers: Arc<SharedTransfers>,
    remote: RemoteFileManager,
    storage_path: String,
    services: SimulatorServices,
    timing: ProvisioningTiming,
    logger: Logger,
    state: Mutex<State>,
    retained: RetainedTasks,
    lines: Arc<LineListeners>,
}
impl AppleSimulatorDevice {
    pub fn new(input: SimulatorConstruction, services: SimulatorServices) -> Arc<Self> {
        let SimulatorConstruction {
            transport,
            executor,
            bridge,
            bin,
            record,
            sdk,
            shared_transfers,
        } = input;
        let udid = record.udid;
        let serial = format!("{udid}@{}", executor.host().id);
        let storage_path = storage_path_for(&udid);
        let logger = services
            .logging
            .logger("com.malinskiy.marathon.apple.ios.AppleSimulatorDevice");
        let remote = RemoteFileManager::new(
            storage_path.clone(),
            udid.clone(),
            Arc::new(ExecutorWorkerCommands {
                executor: executor.clone(),
                timeouts: Arc::new(services.vendor.timeout_configuration.clone()),
            }),
            services.logging.clone(),
        );
        let timing = ProvisioningTiming::new(services.timer.clone(), services.logging.clone());
        let state = State {
            info: DeviceInfo {
                operating_system: OperatingSystem {
                    version: "Unknown".into(),
                },
                serial_number: serial.as_str().into(),
                model: "Unknown".into(),
                manufacturer: "Unknown".into(),
                network_state: NetworkState::Connected,
                device_features: vec![],
                healthy: true,
            },
            abi: "Unknown".into(),
            env: None,
            home: None,
            root_path: String::new(),
            log_file: String::new(),
            descriptor_path: String::new(),
            descriptor: None,
            runtime_version: None,
            runtime_build_version: String::new(),
            version: String::new(),
            device_type: String::new(),
            xcode_version: None,
            supports_transcoding: false,
            bundle: None,
            disposal: None,
            lost: false,
            retired: false,
            previous_batch_finished_at: None,
            admission: None,
            policy: marathon_apple::device::batch_failure_policy::BatchFailurePolicy::new(
                3,
                services.vendor.max_batches_without_progress,
            ),
            running: None,
            last_session: None,
        };
        Arc::new_cyclic(move |me| Self {
            me: me.clone(),
            udid,
            serial,
            transport,
            sdk,
            bin,
            executor,
            bridge,
            shared_transfers,
            remote,
            storage_path,
            services,
            timing,
            logger,
            state: Mutex::new(state),
            retained: RetainedTasks::default(),
            lines: Arc::default(),
        })
    }
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn this(&self) -> VendorResult<Arc<Self>> {
        self.me.upgrade().ok_or_else(|| {
            VendorError::Other(marathon_apple::cmd::failure(
                "java.lang.IllegalStateException",
                "Device allocation has been released",
            ))
        })
    }
    fn log(&self, level: Level, message: impl Into<String>, error: Option<marathon_core::error::Failure>) {
        if self.services.logging.enabled(&self.logger, level) {
            (self.services.logging.sink)(&self.logger, level, message.into(), error);
        }
    }
    pub fn serial(&self) -> &str {
        &self.serial
    }
    pub fn executor(&self) -> Executor {
        self.executor.clone()
    }
    pub fn connected(&self) -> bool {
        self.executor.connected()
    }
    pub fn transport(&self) -> &Transport {
        &self.transport
    }
    pub fn set_admission(&self, admission: Option<Admission>) {
        self.state().admission = admission;
    }
    pub fn lost(&self) -> bool {
        let state = self.state();
        state.lost && !state.retired && state.disposal.is_none()
    }
    pub fn retired(&self) -> bool {
        let (retired, session) = {
            let state = self.state();
            (state.retired, state.last_session.clone())
        };
        retired || session.is_some_and(|s| s.abandoned())
    }
    pub fn retire(&self) {
        self.state().retired = true;
    }
    pub fn disposed(&self) -> bool {
        self.state().disposal.as_ref().is_some_and(|d| d.completed().is_some())
    }
    /// D44: lost, or on the way out, whether or not its disposal has finished.
    pub fn leaving(&self) -> bool {
        let state = self.state();
        state.lost || state.disposal.is_some()
    }
    pub fn waiting_since(&self) -> Option<i64> {
        let state = self.state();
        if state.running.is_some() || state.lost || state.retired || state.disposal.is_some() {
            None
        } else {
            state.previous_batch_finished_at
        }
    }
    fn mark_lost(&self) {
        let mut state = self.state();
        let open = state.last_session.as_ref().is_some_and(|s| !s.closed());
        if open {
            state.retired = true;
        } else {
            state.lost = true;
        }
    }
    /// Retained cancellation-ignoring preparation/batch work is joined explicitly by its run owner.
    pub async fn join_retained(&self) -> VendorResult<()> {
        for task in self.retained.take() {
            task.join().await?;
        }
        Ok(())
    }
    pub fn root_path(&self) -> String {
        self.state().root_path.clone()
    }
    pub fn log_file(&self) -> String {
        self.state().log_file.clone()
    }
    pub fn supports_transcoding(&self) -> bool {
        self.state().supports_transcoding
    }
    pub fn xcode_version(&self) -> Option<XcodeVersion> {
        self.state().xcode_version
    }
}
impl SimulatorSetup for AppleSimulatorDevice {
    fn setup<'a>(
        &'a self,
        record: &'a SimctlDevice,
        cycle: Arc<WorkerProvisioningContext>,
        caller: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move { self.setup_record(record, cycle, &caller).await })
    }
}
impl Device for AppleSimulatorDevice {
    fn info(&self) -> DeviceInfo {
        self.state().info.clone()
    }
    fn abi(&self) -> JavaString {
        self.state().abi.clone()
    }
    fn worker_id(&self) -> Option<JavaString> {
        apple_worker_id(&self.transport)
    }
    fn logger(&self) -> &Logger {
        &self.logger
    }
    fn prepare<'a>(&'a self, _: Arc<Configuration>, context: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async move { self.prepare_device(context).await })
    }
    fn execute<'a>(
        &'a self,
        configuration: Arc<Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<dyn Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move { self.execute_batch(configuration, pool, batch, result, context).await })
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move { self.dispose_and_join().await })
    }
}
impl LogProducer for AppleSimulatorDevice {
    fn add_line_listener(&self, listener: Arc<dyn LineListener>) -> VendorResult<()> {
        self.lines.add(listener);
        Ok(())
    }
    fn remove_line_listener(&self, listener: &Arc<dyn LineListener>) -> VendorResult<()> {
        self.lines.remove(listener);
        Ok(())
    }
}

struct LinkedSignal {
    parent: Arc<dyn marathon_core::runtime::CancellationSignal>,
    local: Arc<marathon_core::executor::shared::Signal>,
}
impl marathon_core::runtime::CancellationSignal for LinkedSignal {
    fn reason(&self) -> Option<marathon_core::error::Cancellation> {
        self.parent.reason().or_else(|| self.local.reason())
    }
    fn cancelled(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = marathon_core::error::Cancellation> + Send + '_>> {
        Box::pin(async move {
            tokio::select! { reason = self.parent.cancelled() => reason, reason = self.local.cancelled() => reason }
        })
    }
}
