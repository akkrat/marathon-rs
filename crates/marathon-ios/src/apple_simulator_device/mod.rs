//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt

use marathon_logging::{Component, Level};

use marathon_error::FailureKind;
mod boot;
mod episode;
mod execution;
pub use episode::PreparationEpisode;
mod listeners;
pub use listeners::{MinimalSimulatorListenerFactory, SimulatorListenerFactory, SimulatorListeners};
mod prepare;
pub mod recording;
mod setup;
mod worker;

use crate::device::{
    error::{ProvisioningResult, clock_error},
    simulator_factory::{ConstructedSimulator, SimulatorConstruction, SimulatorConstructor, SimulatorSetup},
    timing::ProvisioningTiming,
    worker_provisioning_context::WorkerProvisioningContext,
};
use jcompat::file_manager::FileManager;
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
    abi: String,
    env: Option<Vec<(String, String)>>,
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
    /// U179: preparations still running, which disposal stops and waits for. Disposal takes them under the same lock
    /// that installs it, so none starts after that.
    preparations: Vec<Arc<prepare::PreparationJob>>,
    /// D91: sends to this simulator that may still be running, whoever waits for them, which disposal stops and waits
    /// for. Registered under the same lock as `preparations`, so none starts after disposal.
    transfers: Vec<Arc<marathon_apple::cmd::TransferReceipt>>,
    /// D93 (round 9): pulls from this simulator (artifact jobs, results), registered as its sends are: one still running at
    /// its disposal keeps the on-dispose lifecycle and the release of its lease from running beside it.
    pulls: marathon_apple::cmd::DevicePulls,
    /// D87: the preparation that has not ended in a ready simulator.
    episode: Option<PreparationEpisode>,
    /// D93: the simulator is stopping, sealed by the run's stop or by its first disposal, whichever came first: when, on
    /// the vendor clock, its work is to have ended. Every wait for that work ends there; no new work starts.
    shutdown: Option<Arc<execution::Shutdown>>,
    provider_deadline: Option<i64>,
    /// D93: its disposal could not confirm that its work had ended: its lease is quarantined, not released.
    quarantined: bool,
    last_session: Option<Arc<marathon_apple::xcodebuild_session::XcodebuildSession>>,
    /// D93: every xcodebuild session registered and not confirmed to have ended, the last included. A session leaves only
    /// once it has ended confirmed (`ended() == Some(true)`), so one whose exec or close could not be confirmed keeps the
    /// simulator's stop unconfirmed for the rest of the run, whatever replaces it as the last session.
    sessions: Vec<Arc<marathon_apple::xcodebuild_session::XcodebuildSession>>,
    recordings: Vec<Arc<recording::Recording>>,
    /// D98: a recorder start failed with the busy code while marathon had no recorder of its own on this simulator.
    video_unavailable: bool,
}
impl State {
    /// U179, D91, D93: why no new batch, preparation or send may start, if none may: disposal, or the run's stop.
    /// D93 (rounds 7, 8): the refusal as an error, built by the seal's cause alone (`SealCause::refusal`).
    fn refusal_error(&self, serial: &str) -> Option<VendorError> {
        let why = marathon_apple::cmd::failure(FailureKind::IllegalState, self.refusal(serial)?);
        let cause = self
            .shutdown
            .as_ref()
            .map_or(marathon_apple::cmd::SealCause::Disposal, |s| s.cause);
        Some(cause.refusal(why))
    }
    fn refusal(&self, serial: &str) -> Option<String> {
        if self.disposal.is_some() {
            Some(format!("Device {serial} has been disposed"))
        } else if self.shutdown.is_some() {
            Some(format!("Device {serial} is stopping"))
        } else {
            None
        }
    }
}
pub struct AppleSimulatorDevice {
    me: Weak<Self>,
    udid: String,
    serial: String,
    transport: Transport,
    sdk: Sdk,
    bin: Arc<AppleBinaryEnvironment>,
    /// The lease; commands go through `commands`.
    executor: Executor,
    commands: Arc<marathon_apple::cmd::DeviceCommands>,
    bridge: Bridge,
    shared_transfers: Arc<SharedTransfers>,
    remote: RemoteFileManager,
    storage_path: String,
    services: SimulatorServices,
    timing: ProvisioningTiming,
    logger: Component,
    state: Mutex<State>,
    signal_finish: marathon_core::signal_finish::SignalDeadline,
    retained: RetainedTasks,
    lines: Arc<LineListeners>,
}
impl AppleSimulatorDevice {
    pub fn new(input: SimulatorConstruction, services: SimulatorServices) -> Arc<Self> {
        let SimulatorConstruction {
            transport,
            executor,
            commands,
            bridge,
            bin,
            record,
            sdk,
            shared_transfers,
        } = input;
        let udid = record.udid;
        let serial = format!("{udid}@{}", executor.host().id);
        let storage_path = storage_path_for(&udid);
        let logger = services.logging.component("marathon_ios::apple_simulator_device");
        let remote = RemoteFileManager::new(
            storage_path.clone(),
            udid.clone(),
            Arc::new(ExecutorWorkerCommands {
                executor: commands.clone(),
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
            preparations: Vec::new(),
            transfers: Vec::new(),
            pulls: Default::default(),
            provider_deadline: None,
            episode: None,
            shutdown: None,
            quarantined: false,
            last_session: None,
            sessions: Vec::new(),
            recordings: Vec::new(),
            video_unavailable: false,
        };
        Arc::new_cyclic(move |me| Self {
            me: me.clone(),
            udid,
            serial,
            transport,
            sdk,
            bin,
            executor,
            commands,
            bridge,
            shared_transfers,
            remote,
            storage_path,
            services,
            timing,
            logger,
            state: Mutex::new(state),
            signal_finish: Default::default(),
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
                FailureKind::IllegalState,
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
    /// D93: its disposal could not confirm that its work had ended, so its lease is to be quarantined, not released
    /// (`ProvisioningConnections::quarantine`).
    pub fn quarantined(&self) -> bool {
        self.state().quarantined
    }
    /// D93: how the provider quarantines this simulator's lease, applied when its disposal decides to.
    pub fn on_lease_quarantine(&self, hook: marathon_apple::cmd::device_commands::LeaseQuarantine) {
        self.commands.on_quarantine(hook);
    }
    /// The output-line listeners installed now (D84 witnesses).
    #[doc(hidden)]
    pub fn line_listener_count(&self) -> usize {
        self.lines.len()
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
    /// Joins every retained task: preparations, batches and disposal. Disposal itself stops and waits for preparations
    /// and the batch within its bound (U179), so this is for owners that need the rest too, such as tests.
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
    fn lease_quarantined(&self) -> bool {
        self.quarantined()
    }
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
    /// D84: a cancelled batch is joined within `cancelled_batch_millis`, which also covers its session's stop; a
    /// cancelled preparation returns at once.
    fn cancellation_cleanup_millis(&self) -> Option<i64> {
        Some(self.services.timeouts.cancelled_batch_millis)
    }
    /// D84 (R3-2): a cancelled preparation goes on in a retained task until it is stopped. D74's join stops it and
    /// waits for it (and for a batch still running, and, D91, for sends still running) within the same bound, as disposal
    /// does.
    /// D93: within what is left of the window the run's stop sealed (`seal_for_stop`), not a window of its own.
    fn join_cancelled_work(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if let marathon_apple::cmd::Termination::Unconfirmed(_) = self.finish_work().await {
                self.retire();
            }
            Ok(())
        })
    }
    fn seal_for_stop(&self) {
        self.seal_for_run_stop();
    }
    fn seal_for_signal(&self, finish: marathon_core::signal_finish::SignalFinish) {
        self.signal_seal(finish);
    }
    fn info(&self) -> DeviceInfo {
        self.state().info.clone()
    }
    fn abi(&self) -> String {
        self.state().abi.clone()
    }
    fn worker_id(&self) -> Option<String> {
        apple_worker_id(&self.transport)
    }
    fn component(&self) -> &Component {
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
        result: Arc<Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move { self.execute_batch(configuration, pool, batch, result, context).await })
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move { self.dispose_and_join().await })
    }
    fn preparation_time_left_millis(&self, configuration: &Configuration) -> Option<i64> {
        Some(self.open_preparation(configuration))
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
            tokio::select! {
                biased;
                reason = self.parent.cancelled() => reason,
                reason = self.local.cancelled() => reason,
            }
        })
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::LinkedSignal;
    use marathon_core::{
        executor::{Signal, shared::requested},
        runtime::CancellationSignal,
    };

    #[tokio::test]
    async fn linked_signal_parent_reason_wins_when_both_sources_are_ready() {
        let parent = Signal::new();
        let local = Signal::new();
        let parent_reason = requested("parent");
        parent.cancel(parent_reason.clone());
        local.cancel(requested("local"));
        let signal = LinkedSignal { parent, local };
        assert_eq!(signal.reason(), Some(parent_reason.clone()));
        for _ in 0..256 {
            assert_eq!(
                signal.cancelled().await,
                parent_reason,
                "cancelled() must agree with reason() when both sources are ready"
            );
        }
    }

    #[tokio::test]
    async fn linked_signal_observes_either_source_when_it_alone_is_ready() {
        for cancel_parent in [false, true] {
            let parent = Signal::new();
            let local = Signal::new();
            let reason = requested("only ready source");
            if cancel_parent {
                parent.cancel(reason.clone());
            } else {
                local.cancel(reason.clone());
            }
            let signal = LinkedSignal { parent, local };
            assert_eq!(signal.reason(), Some(reason.clone()));
            assert_eq!(signal.cancelled().await, reason);
        }
    }
}
