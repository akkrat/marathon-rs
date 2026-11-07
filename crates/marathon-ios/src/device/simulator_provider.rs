//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt
//! Simulator registration, recovery, retry and late admission.

mod formatting;
pub mod recovery;
mod source;

use super::{
    device_tracker::{DeviceTracker, TrackingUpdate},
    error::{ProvisioningError, ProvisioningResult, clock_error},
    provisioning_plan::{CurrentDevices, PlanCalculator},
    simulator_factory::{BinaryFactory, ConstructedSimulator, SimulatorCreating},
    simulator_provisioner::{FailedSetup, ProvisioningConnections, SimulatorProvisioner},
    timing::{ProvisioningTiming, local_transport_identity, profile_text, transport_text},
    worker_provisioning_context::WorkerProvisioningContext,
};
use crate::apple_simulator_device::AppleSimulatorDevice;
use futures_util::future::join_all;
use indexmap::{IndexMap, IndexSet};
use jcompat::{
    logging::{Level, Logger},
    writers::JavaString,
};
use marathon_apple::{
    apple_device::AppleDevice,
    apple_device_registry::AppleDeviceRegistry,
    artifact_processor::ArtifactProcessor,
    bin::xcrun::xcresulttool::describe_timeout,
    cmd::{failure, remote::ssh::sshj::config::SshLogging, retained_tasks::RetainedTasks},
};
use marathon_config::{
    binding::BindingContext,
    devices::{AppleTarget, MarathonDevices, Transport},
    vendor::IosConfiguration,
};
use marathon_core::{
    error::{VendorError, VendorResult},
    executor::shared::{SharedChannel, SharedDeferred, Signal, requested},
    runtime::{Deferred, EventChannel, VendorFuture, VendorRuntime, VendorTask},
    vendor::{DeviceEvent, DeviceProvider, DeviceRef, VendorContext},
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard, Weak},
};

pub type Simulator = Arc<AppleSimulatorDevice>;
/// Original retry/reconnect call boundaries. A production caller must supply the 5.5b policy.
pub trait ProviderRecovery: Send + Sync {
    fn attach(&self, _provider: Weak<AppleSimulatorProvider>) {}
    /// 5.5b owns detached connection cancellation and its late-result release.
    fn connect_worker(
        &self,
        transport: Transport,
        connections: Arc<dyn ProvisioningConnections>,
        caller: VendorContext,
    ) -> marathon_apple::device::connection_factory::ConnectionFuture<
        '_,
        marathon_apple::device::connection_factory::Executor,
    >;
    fn reconnect(&self, provider: Arc<AppleSimulatorProvider>, caller: VendorContext) -> VendorFuture<'_, ()>;
    fn admit(&self, id: &JavaString, device: &Simulator) -> ProvisioningResult<()>;
    fn offer(&self, transport: &Transport, targets: &[AppleTarget]) -> bool;
    fn missing(
        &self,
        transport: Transport,
        targets: Vec<AppleTarget>,
        started: i64,
        caller: VendorContext,
    ) -> VendorFuture<'_, ()>;
    fn connect_failed(
        &self,
        transport: Transport,
        targets: Vec<AppleTarget>,
        started: i64,
        error: ProvisioningError,
        caller: VendorContext,
    ) -> VendorFuture<'_, ()>;
    /// D33: targets whose setup failed while their worker stayed reachable.
    fn setup_failed(&self, transport: Transport, targets: Vec<AppleTarget>, started: i64) -> VendorFuture<'_, ()>;
    /// D33: a snapshot removed this simulator, so a retry waiting for it is void.
    fn forget_setup(&self, id: &JavaString);
    fn terminate(&self) -> VendorFuture<'_, ()>;
}
/// Which of the provider's setup cycles is running, for its timing line.
#[derive(Clone, Copy)]
enum SetupCycle {
    First,
    Late {
        attempt: i32,
        connected: i64,
    },
    /// D33: a timed retry of targets whose setup failed.
    Retried,
}
/// What a setup cycle couldn't set up, by who takes it from there.
#[derive(Default)]
struct UnfinishedSetup {
    /// The worker couldn't be reached: its connection retry (5.5b) owns these.
    missing: Vec<AppleTarget>,
    /// The worker answered and setup failed: D33's bounded retry owns these.
    failed: Vec<AppleTarget>,
}
#[derive(Clone)]
pub struct ProviderServices {
    pub vendor: Arc<IosConfiguration>,
    pub runtime: Arc<dyn VendorRuntime>,
    pub context: VendorContext,
    pub binding: Arc<BindingContext>,
    pub connections: Arc<dyn ProvisioningConnections>,
    pub binaries: BinaryFactory,
    pub factory: Arc<dyn SimulatorCreating<Handle = Simulator>>,
    pub artifacts: Arc<ArtifactProcessor>,
    pub registry: Arc<AppleDeviceRegistry>,
    pub timing: ProvisioningTiming,
    pub logging: SshLogging,
    pub recovery: Arc<dyn ProviderRecovery>,
}
/// D33: events leave in the order they were raised, whatever order their publishing tasks run in. The send happens
/// under the lock of the pop: on a multi-threaded runtime a task between the two would be overtaken.
struct Outbox<T>(Mutex<VecDeque<T>>);
impl<T> Default for Outbox<T> {
    fn default() -> Self {
        Self(Mutex::default())
    }
}
impl<T> Outbox<T> {
    fn push(&self, event: T) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push_back(event);
    }
    fn send_oldest<R>(&self, send: impl FnOnce(T) -> R) -> Option<R> {
        let mut queue = self.0.lock().unwrap_or_else(|e| e.into_inner());
        queue.pop_front().map(send)
    }
}
/// A replacement whose connect waits for its predecessor's disposal. A removal in that window can't publish its
/// disconnect first, or the later connect would put a removed, disposed simulator into the pool.
struct PendingConnect {
    handle: Simulator,
    disconnect: bool,
}
/// D44: a simulator whose recovery failed. Its old handle stays in the device map and the scan leaves it alone: whoever
/// got the failure sets it up again, and any planned cycle on its worker may do so.
struct Parked {
    handle: Simulator,
    /// Run time of the failure, or of the devices update that added it back. The worker retry's limit counts from here.
    since: i64,
    /// Whether its replacement keeps the old lease for release at termination (A90).
    retained: bool,
}
#[derive(Clone, Copy)]
struct ParkedOn {
    since: i64,
    removed: bool,
}
#[derive(Default)]
struct State {
    connecting: Vec<PendingConnect>,
    devices: IndexMap<JavaString, ConstructedSimulator<Simulator>>,
    removed: IndexSet<JavaString>,
    skipped_removed: IndexSet<JavaString>,
    recovering: Vec<Simulator>,
    parked: Vec<Parked>,
    exhausted: IndexSet<JavaString>,
    replaced: Vec<Simulator>,
    initialized: bool,
    identities: Vec<(Weak<AppleSimulatorDevice>, marathon_core::time::ObjectIdentity)>,
    monitoring: Option<Arc<dyn VendorTask>>,
    termination: Option<Arc<SharedDeferred<()>>>,
}
pub struct AppleSimulatorProvider {
    me: Weak<Self>,
    services: ProviderServices,
    logger: Logger,
    local_identity: JavaString,
    state: Mutex<State>,
    tracker: Mutex<Option<DeviceTracker>>,
    updates: tokio::sync::Mutex<()>,
    lifecycle: tokio::sync::Mutex<()>,
    update_tasks: RetainedTasks,
    /// D33: one planned cycle per worker at a time, so that a timed retry can't plan a simulator another cycle is
    /// setting up, or shut it down as unused. Other workers don't share a gate. A recovery's re-provisioning has no
    /// plan and doesn't take it: it sets up simulators that are registered, which no plan picks.
    setup_gates: Mutex<IndexMap<Transport, Arc<tokio::sync::Mutex<()>>>>,
    provisioner: SimulatorProvisioner<Simulator>,
    planner: PlanCalculator,
    channel: Arc<SharedChannel<DeviceEvent>>,
    outbox: Arc<Outbox<DeviceEvent>>,
    monitor_cancel: Arc<Signal>,
    events: RetainedTasks,
    operations: RetainedTasks,
}
impl AppleSimulatorProvider {
    pub fn new(mut services: ProviderServices) -> ProvisioningResult<Arc<Self>> {
        services.connections =
            super::detached::DetachedConnections::new(services.connections, services.runtime.clone());
        let binding = &services.binding;
        services.binding = Arc::new(BindingContext {
            profile: marathon_config::binding::Profile::Marathondevices,
            marathonfile: binding.marathonfile.clone(),
            marathonfile_dir: binding.marathonfile_dir.clone(),
            process_dir: binding.process_dir.clone(),
            environment: binding.environment.clone(),
            now: binding.now,
            analytics_tracking: binding.analytics_tracking,
            bugsnag_reporting: binding.bugsnag_reporting,
        });
        let local_identity = local_transport_identity(services.context.hooks.as_ref())?;
        let planner = PlanCalculator::new(
            services.logging.clone(),
            services.timing.clone(),
            services.context.hooks.as_ref(),
        )?;
        let owner = Arc::new(Mutex::new(Weak::<Self>::new()));
        let weak = owner.clone();
        let provisioner = SimulatorProvisioner::new(
            services.connections.clone(),
            services.factory.clone(),
            Arc::new(move |id, device| {
                let provider = weak.lock().unwrap_or_else(|e| e.into_inner()).upgrade();
                Box::pin(async move {
                    let provider = provider.ok_or_else(|| {
                        other(
                            "java.lang.IllegalStateException",
                            "Provider allocation has been released",
                        )
                    })?;
                    provider.register(id, device).await
                })
            }),
            services.vendor.threading_configuration.provisioning_concurrency,
            services.logging.clone(),
            services.context.hooks.as_ref(),
        )?;
        let provider = Arc::new_cyclic(|me| Self {
            me: me.clone(),
            logger: services.logging.logger("AppleSimulatorProvider"),
            tracker: Mutex::new(Some(DeviceTracker::new(services.logging.clone()))),
            services: services.clone(),
            local_identity,
            state: Mutex::default(),
            updates: tokio::sync::Mutex::new(()),
            lifecycle: tokio::sync::Mutex::new(()),
            update_tasks: RetainedTasks::default(),
            setup_gates: Mutex::default(),
            provisioner,
            planner,
            channel: SharedChannel::new(),
            outbox: Arc::default(),
            monitor_cancel: Signal::new(),
            events: RetainedTasks::default(),
            operations: RetainedTasks::default(),
        });
        *owner.lock().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(&provider);
        services.recovery.attach(Arc::downgrade(&provider));
        Ok(provider)
    }
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn this(&self) -> VendorResult<Arc<Self>> {
        self.me.upgrade().ok_or_else(|| {
            other(
                "java.lang.IllegalStateException",
                "Provider allocation has been released",
            )
        })
    }
    fn log(&self, level: Level, message: impl FnOnce() -> String, error: Option<marathon_core::error::Failure>) {
        if self.services.logging.enabled(&self.logger, level) {
            (self.services.logging.sink)(&self.logger, level, message(), error);
        }
    }
    pub fn devices(&self) -> Vec<ConstructedSimulator<Simulator>> {
        self.state().devices.values().cloned().collect()
    }
    /// Recovery marks allocation identities, not serials reused by replacements.
    pub fn set_recovering(&self, device: Simulator, recovering: bool) {
        let mut state = self.state();
        state.recovering.retain(|d| !Arc::ptr_eq(d, &device));
        if recovering {
            state.recovering.push(device);
        }
    }
    pub fn set_exhausted(&self, id: JavaString, exhausted: bool) {
        let mut state = self.state();
        if exhausted {
            state.exhausted.insert(id);
        } else {
            state.exhausted.shift_remove(&id);
        }
    }
    pub fn skipped_removed(&self, id: &JavaString) -> bool {
        self.state().skipped_removed.contains(id)
    }
    fn notify(&self, event: DeviceEvent) {
        self.outbox.push(event);
        self.publish();
    }
    /// A disconnect that follows the connect its device is still waiting to raise.
    fn notify_removed(&self, device: &ConstructedSimulator<Simulator>) {
        {
            let mut state = self.state();
            if let Some(pending) = state
                .connecting
                .iter_mut()
                .find(|pending| Arc::ptr_eq(&pending.handle, &device.handle))
            {
                pending.disconnect = true;
                return;
            }
            self.outbox.push(DeviceEvent::Disconnected(device.device.clone()));
        }
        self.publish();
    }
    fn publish(&self) {
        let outbox = self.outbox.clone();
        let channel = self.channel.clone();
        let logging = self.services.logging.clone();
        let logger = self.logger.clone();
        self.events.spawn(self.services.runtime.as_ref(), async move {
            let Some(Err(e)) = outbox.send_oldest(|event| channel.try_send(event)) else {
                return Ok(());
            };
            let detail = e.cause.unwrap_or_else(|| {
                failure(
                    "kotlinx.coroutines.channels.ClosedSendChannelException",
                    "Channel was closed",
                )
            });
            // A root launch reports a failed send independently of its registering caller.
            if logging.enabled(&logger, Level::Error) {
                (logging.sink)(&logger, Level::Error, String::new(), Some(detail.clone()));
            }
            Err(VendorError::Other(detail))
        });
    }
    pub async fn register(&self, id: JavaString, device: ConstructedSimulator<Simulator>) -> ProvisioningResult<()> {
        self.services.recovery.admit(&id, &device.handle)?;
        let serial = device.handle.serial().to_owned();
        let previous = {
            let mut state = self.state();
            let previous = state.devices.get(&id).cloned();
            let out = previous.as_ref().is_some_and(|p| p.handle.retired());
            let old_serial = previous.as_ref().map(|d| d.handle.serial().to_owned());
            if previous.is_some() && (out || state.exhausted.contains(&id)) {
                return Err(other(
                    "java.lang.IllegalStateException",
                    format!(
                        "Simulator {} is out of the run, its new setup is dropped",
                        old_serial.as_deref().unwrap_or(&serial)
                    ),
                )
                .into());
            }
            if state.removed.contains(&id) {
                state.skipped_removed.insert(id);
                return Err(other(
                    "java.lang.IllegalStateException",
                    format!("Simulator {serial} has left the devices file, its new setup is dropped"),
                )
                .into());
            }
            state.skipped_removed.shift_remove(&id);
            let previous = state.devices.insert(id.clone(), device.clone());
            if let Some(previous) = &previous {
                // A90 also holds for a worker-loss predecessor replaced later, by a retry of its recovery.
                let parked = state
                    .parked
                    .iter()
                    .find(|p| Arc::ptr_eq(&p.handle, &previous.handle))
                    .map(|p| p.retained);
                state.parked.retain(|p| !Arc::ptr_eq(&p.handle, &previous.handle));
                let recovering = state.recovering.iter().any(|d| Arc::ptr_eq(d, &previous.handle));
                if parked.unwrap_or(!recovering) {
                    state.replaced.push(previous.handle.clone());
                }
            }
            self.services.registry.register(device.handle.clone());
            // A removal that sees this device in the map finds its connect raised, or waiting to be.
            if previous.is_none() {
                self.outbox.push(DeviceEvent::Connected(device.device.clone()));
            } else {
                state.connecting.push(PendingConnect {
                    handle: device.handle.clone(),
                    disconnect: false,
                });
            }
            previous
        };
        let Some(previous) = previous else {
            self.publish();
            return Ok(());
        };
        let replaced: ProvisioningResult<()> = async {
            self.services.registry.remove(&previous.device);
            if self.services.logging.enabled(&self.logger, Level::Error) {
                let previous_text = self.object_text(&previous.handle)?;
                let current_text = self.object_text(&device.handle)?;
                self.log(
                    Level::Error,
                    || format!("replaced existing device {previous_text} with new {current_text}."),
                    None,
                );
            }
            previous.device.0.dispose().await?;
            Ok(())
        }
        .await;
        let events = {
            let mut state = self.state();
            let disconnect = state
                .connecting
                .iter()
                .position(|pending| Arc::ptr_eq(&pending.handle, &device.handle))
                .is_some_and(|index| state.connecting.swap_remove(index).disconnect);
            // Another instance holds the id by now, and the pool has never seen this one. Its events would carry the
            // same serial and could only harm the newer device.
            let superseded = state
                .devices
                .get(&id)
                .is_some_and(|current| !Arc::ptr_eq(&current.handle, &device.handle));
            let connect = replaced.is_ok() && !superseded;
            let disconnect = disconnect && !superseded;
            if connect {
                self.outbox.push(DeviceEvent::Connected(device.device.clone()));
            }
            // Under the same lock: a removal arriving from here on publishes after these.
            if disconnect {
                self.outbox.push(DeviceEvent::Disconnected(device.device.clone()));
            }
            usize::from(connect) + usize::from(disconnect)
        };
        for _ in 0..events {
            self.publish();
        }
        replaced
    }
    fn object_text(&self, device: &Simulator) -> ProvisioningResult<String> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 62);
        let token = {
            let mut state = self.state();
            state.identities.retain(|(weak, _)| weak.strong_count() > 0);
            match state
                .identities
                .iter()
                .find(|(weak, _)| weak.ptr_eq(&Arc::downgrade(device)))
            {
                Some((_, token)) => *token,
                None => {
                    let token =
                        marathon_core::time::ObjectIdentity(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
                    state.identities.push((Arc::downgrade(device), token));
                    token
                }
            }
        };
        let suffix = self
            .services
            .context
            .hooks
            .identity_suffix("ios.AppleSimulatorProvider.identities", token)
            .map_err(clock_error)?;
        let mut text: JavaString = "com.malinskiy.marathon.apple.ios.AppleSimulatorDevice@".into();
        text.append(suffix);
        Ok(text.to_string_lossy())
    }
    async fn process(&self, snapshot: MarathonDevices, caller: VendorContext) -> ProvisioningResult<()> {
        let _serial = tokio::select! { biased;
            reason = caller.cancellation.cancelled() => return Err(VendorError::Cancelled(reason).into()),
            lock = self.updates.lock() => lock,
        };
        // Tracker logs through its callback; never run it under the registration lock.
        let mut tracker = self
            .tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| other("java.lang.IllegalStateException", "Concurrent tracker update"))?;
        let updates = tracker.update(&snapshot);
        *self.tracker.lock().unwrap_or_else(|e| e.into_inner()) = Some(tracker);
        let updates = updates?;
        let mut removals = Vec::new();
        let mut additions = Vec::new();
        for (transport, updates) in updates {
            self.log(
                Level::Debug,
                || {
                    let lines = updates
                        .iter()
                        .map(|u| formatting::update_text(u).to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!(
                        "Processing updates from {}:\n- {lines}",
                        transport_text(&transport, &self.local_identity)
                    )
                },
                None,
            );
            let mut added = Vec::new();
            for update in updates {
                match update {
                    TrackingUpdate::Disconnected(AppleTarget::Physical { udid } | AppleTarget::Simulator { udid }) => {
                        let id = apple_id(&udid, &transport);
                        let device = {
                            let mut state = self.state();
                            state.removed.insert(id.clone());
                            state.devices.get(&id).cloned()
                        };
                        self.services.recovery.forget_setup(&id);
                        if let Some(device) = device {
                            removals.push(device);
                        }
                    }
                    TrackingUpdate::Disconnected(AppleTarget::Host) => self.log(
                        Level::Warn,
                        || "host devices are not support by apple simulator provider".into(),
                        None,
                    ),
                    TrackingUpdate::Disconnected(AppleTarget::SimulatorProfile(_)) => self.log(
                        Level::Warn,
                        || "simulator profile devices do not support disconnect".into(),
                        None,
                    ),
                    TrackingUpdate::Connected(target) => {
                        if let AppleTarget::Physical { udid } | AppleTarget::Simulator { udid } = &target {
                            let id = apple_id(udid, &transport);
                            self.state().removed.shift_remove(&id);
                            self.restart_parked(&id)?;
                        }
                        added.push(target);
                    }
                }
            }
            if !added.is_empty() {
                additions.push((transport, added));
            }
        }
        let siblings = Signal::new();
        let caller = VendorContext {
            cancellation: Arc::new(LinkedSignal {
                parent: caller.cancellation,
                local: siblings.clone(),
            }),
            ..caller
        };
        let disposals = removals.into_iter().map(|device| {
            let siblings = siblings.clone();
            async move {
                self.notify_removed(&device);
                if device.handle.connected() {
                    let result = device.device.0.dispose().await.map_err(ProvisioningError::from);
                    if let Some(reason) = result.as_ref().err().and_then(ProvisioningError::cancellation) {
                        siblings.cancel(reason);
                    }
                    self.isolate(result, || format!("Failed to dispose of {}", device.handle.serial()))?;
                }
                Ok::<_, ProvisioningError>(())
            }
        });
        let provisioning = additions.into_iter().map(|(transport, targets)| {
            let caller = caller.clone();
            let siblings = siblings.clone();
            async move {
                let result = self.initialize_transport(targets, transport.clone(), caller).await;
                if let Some(reason) = result.as_ref().err().and_then(ProvisioningError::cancellation) {
                    siblings.cancel(reason);
                }
                self.isolate(result, || {
                    format!(
                        "Failed to initialize worker {}",
                        transport_text(&transport, &self.local_identity)
                    )
                })
            }
        });
        let (disposed, provisioned) = tokio::join!(join_all(disposals), join_all(provisioning));
        for result in disposed.into_iter().chain(provisioned) {
            result?;
        }
        self.log(
            Level::Debug,
            || format!("Providing {} devices", self.state().devices.len()),
            None,
        );
        Ok(())
    }
    fn isolate(&self, result: ProvisioningResult<()>, message: impl FnOnce() -> String) -> ProvisioningResult<()> {
        match result {
            Err(e) if e.is_cancelled() => Err(e),
            Err(e) => {
                self.log(Level::Error, message, Some(e.output_failure()));
                Ok(())
            }
            Ok(()) => Ok(()),
        }
    }
    async fn initialize_transport(
        &self,
        targets: Vec<AppleTarget>,
        transport: Transport,
        caller: VendorContext,
    ) -> ProvisioningResult<()> {
        if self.services.recovery.offer(&transport, &targets) {
            return Ok(());
        }
        let started = self.services.timing.elapsed()?;
        let connection = self
            .services
            .recovery
            .connect_worker(transport.clone(), self.services.connections.clone(), caller.clone())
            .await;
        let executor = match connection {
            Ok(executor) => executor,
            Err(e) => {
                let e = ProvisioningError::from(e);
                if e.is_cancelled() {
                    return Err(e);
                }
                return self
                    .services
                    .recovery
                    .connect_failed(transport, targets, started, e, caller)
                    .await
                    .map_err(Into::into);
            }
        };
        let result = self
            .setup(
                &transport,
                executor.clone(),
                &targets,
                started,
                SetupCycle::First,
                caller.clone(),
            )
            .await;
        // Planning lease is independent of each device lease, even after a failed plan.
        let released = self.services.connections.release(executor).await;
        // A failed release costs this lease, not the retries of what the cycle didn't set up. It stays the cycle's
        // error, which the caller logs.
        let (unfinished, failure) = match result {
            Ok(unfinished) => (unfinished, None),
            Err(error) if error.is_cancelled() => {
                released?;
                return Err(error);
            }
            Err(error) => (
                UnfinishedSetup {
                    missing: Vec::new(),
                    failed: targets,
                },
                Some(error),
            ),
        };
        if !unfinished.missing.is_empty() {
            self.services
                .recovery
                .missing(transport.clone(), unfinished.missing, started, caller)
                .await?;
        }
        if !unfinished.failed.is_empty() {
            self.services
                .recovery
                .setup_failed(transport, unfinished.failed, started)
                .await?;
        }
        released?;
        failure.map_or(Ok(()), Err)
    }
    async fn setup(
        &self,
        transport: &Transport,
        executor: marathon_apple::device::connection_factory::Executor,
        targets: &[AppleTarget],
        started: i64,
        kind: SetupCycle,
        caller: VendorContext,
    ) -> ProvisioningResult<UnfinishedSetup> {
        let connected = self.services.timing.elapsed()?;
        let gate = self
            .setup_gates
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(transport.clone())
            .or_default()
            .clone();
        // The lock comes first: an uncontended gate changes nothing, even for a caller that is being cancelled.
        let _gate = tokio::select! { biased;
            gate = gate.lock() => gate,
            reason = caller.cancellation.cancelled() => return Err(VendorError::Cancelled(reason).into()),
        };
        let bin = (self.services.binaries)(executor.clone());
        let cycle = Arc::new(WorkerProvisioningContext::new(transport.clone(), caller.hooks.clone()));
        let plan = self
            .planner
            .plan(
                transport,
                &bin,
                targets,
                &cycle,
                || {
                    self.devices()
                        .into_iter()
                        .filter(|d| d.handle.transport() == transport)
                        .map(|d| d.handle.udid().to_owned())
                        .collect()
                },
                self.services.vendor.lifecycle_configuration.shutdown_unused,
                || self.current(transport),
                &caller,
            )
            .await;
        let plan = match plan {
            Ok(plan) => plan,
            Err(e) if e.is_cancelled() || executor.connected() => return Err(e),
            Err(e) => {
                self.log(
                    Level::Warn,
                    || {
                        format!(
                            "Lost the connection to worker {} while planning its simulators ({})",
                            transport.id().to_string_lossy(),
                            describe_failure(&e.failure())
                        )
                    },
                    None,
                );
                return Ok(UnfinishedSetup {
                    missing: targets
                        .iter()
                        .filter(|t| matches!(t, AppleTarget::Simulator { .. } | AppleTarget::SimulatorProfile(_)))
                        .cloned()
                        .collect(),
                    failed: Vec::new(),
                });
            }
        };
        let planned = self.services.timing.elapsed()?;
        let failures = self
            .provisioner
            .provision(plan.clone(), transport.clone(), bin, cycle, caller)
            .await?;
        match kind {
            SetupCycle::Late { attempt, connected: connected_late } => self.services.timing.info(|| Ok(format!("Worker {}: connected late on attempt {attempt} in {} ms, planned {} simulators in {} ms, provisioned them in {} ms", transport.id().to_string_lossy(), connected_late.wrapping_sub(started), plan.existing_simulators.len() + plan.needs_provisioning.len(), planned.wrapping_sub(connected_late), self.services.timing.elapsed()?.wrapping_sub(planned))))?,
            SetupCycle::Retried => self.services.timing.info(|| Ok(format!("Worker {}: set up again after a failed setup, planned {} simulators in {} ms, provisioned them in {} ms", transport.id().to_string_lossy(), plan.existing_simulators.len() + plan.needs_provisioning.len(), planned.wrapping_sub(connected), self.services.timing.elapsed()?.wrapping_sub(planned))))?,
            SetupCycle::First => self.services.timing.cycle(
                transport,
                connected.wrapping_sub(started),
                planned.wrapping_sub(connected),
                plan.existing_simulators.len() + plan.needs_provisioning.len(),
                planned,
            )?,
        }
        let lost = !executor.connected();
        let mut unfinished = UnfinishedSetup::default();
        for failure in failures {
            let (target, unreachable, refused) = match failure {
                FailedSetup::Existing {
                    udid,
                    unreachable,
                    refused,
                } => (
                    targets
                        .iter()
                        .find(|t| matches!(t, AppleTarget::Simulator { udid: id } if id.to_string_lossy() == udid))
                        .cloned()
                        .or_else(|| {
                            plan.reused_profiles
                                .get(&udid)
                                .cloned()
                                .map(AppleTarget::SimulatorProfile)
                        }),
                    unreachable,
                    refused,
                ),
                FailedSetup::Created {
                    profile,
                    unreachable,
                    refused,
                } => (Some(AppleTarget::SimulatorProfile(profile)), unreachable, refused),
            };
            let Some(target) = target else { continue };
            if unreachable || lost {
                unfinished.missing.push(target);
            } else if !refused {
                unfinished.failed.push(target);
            }
        }
        Ok(unfinished)
    }
    /// D44: the devices file changed, and that is bounded by its owner, so a simulator added back starts over.
    fn restart_parked(&self, id: &JavaString) -> ProvisioningResult<()> {
        if !self.state().parked.iter().any(|p| parked_id(p) == *id) {
            return Ok(());
        }
        let now = self.services.timing.elapsed()?;
        if let Some(parked) = self.state().parked.iter_mut().find(|p| parked_id(p) == *id) {
            parked.since = now;
        }
        Ok(())
    }
    /// D44 (A80): the map keeps a simulator that left or was lost, and a parse on one fails an attempt of the three that
    /// a run has. As Android's D24: only what can take work now, and a healthy run's keys are all of them, in map order.
    fn borrowable(&self) -> IndexMap<JavaString, ConstructedSimulator<Simulator>> {
        let mut devices = self.state().devices.clone();
        // Outside the lock: an executor's connected check can suspend its caller in the source.
        devices.retain(|_, d| d.handle.connected() && !d.handle.retired() && !d.handle.leaving());
        let state = self.state();
        devices.retain(|id, d| {
            !state.removed.contains(id)
                && !state.exhausted.contains(id)
                && !state.recovering.iter().any(|old| Arc::ptr_eq(old, &d.handle))
                && !state.parked.iter().any(|p| Arc::ptr_eq(&p.handle, &d.handle))
        });
        devices
    }
    fn parked_on(&self, transport: &Transport) -> IndexMap<JavaString, ParkedOn> {
        let state = self.state();
        state
            .parked
            .iter()
            .filter(|p| p.handle.transport() == transport)
            .map(|p| {
                let id = parked_id(p);
                let removed = state.removed.contains(&id);
                (
                    id,
                    ParkedOn {
                        since: p.since,
                        removed,
                    },
                )
            })
            .collect()
    }
    pub fn current(&self, transport: &Transport) -> ProvisioningResult<CurrentDevices> {
        let mut current = CurrentDevices {
            eligible_disposed: Vec::new(),
            kept: Vec::new(),
        };
        for device in self.devices().into_iter().filter(|d| d.handle.transport() == transport) {
            let id = apple_id(&device.handle.udid().into(), transport);
            let free = device.handle.disposed() && device.handle.connected();
            let eligible = !device.handle.retired() && {
                let state = self.state();
                (free || state.parked.iter().any(|p| Arc::ptr_eq(&p.handle, &device.handle)))
                    && !state.recovering.iter().any(|d| Arc::ptr_eq(d, &device.handle))
                    && !state.exhausted.contains(&id)
                    && !state.removed.contains(&id)
                    && state.devices.get(&id).is_some_and(|d| d.device == device.device)
            };
            if eligible {
                current.eligible_disposed.push(device.handle.udid().into());
            } else {
                current.kept.push(device.handle.udid().into());
            }
        }
        Ok(current)
    }
    /// Dispatch explicit update callers as well as the monitoring task off core.
    pub async fn process_update(&self, snapshot: MarathonDevices, caller: VendorContext) -> VendorResult<()> {
        let this = self.this()?;
        let caller = VendorContext {
            cancellation: Arc::new(LinkedSignal {
                parent: caller.cancellation,
                local: self.monitor_cancel.clone(),
            }),
            ..caller
        };
        let done = SharedDeferred::new();
        let result = done.clone();
        self.update_tasks.spawn(self.services.runtime.as_ref(), async move {
            result.complete(
                this.process(snapshot, caller)
                    .await
                    .map_err(ProvisioningError::into_vendor),
            );
            Ok(())
        });
        done.wait().await.map(|_| ())
    }
    async fn dispatch<T: Clone + Send + Sync + 'static>(
        &self,
        work: impl std::future::Future<Output = VendorResult<T>> + Send + 'static,
    ) -> VendorResult<T> {
        let done = SharedDeferred::new();
        let result = done.clone();
        self.operations.spawn(self.services.runtime.as_ref(), async move {
            result.complete(work.await);
            Ok(())
        });
        done.wait().await.map(|value| (*value).clone())
    }
    async fn initialize_inner(self: Arc<Self>, caller: VendorContext) -> VendorResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
        self.log(Level::Debug, || "Initializing AppleSimulatorProvider".into(), None);
        self.log(
            Level::Info,
            || describe_timeout(&self.services.vendor.timeout_configuration),
            None,
        );
        {
            let mut state = self.state();
            if state.initialized || state.termination.is_some() {
                return Err(other(
                    "java.lang.IllegalStateException",
                    "AppleSimulatorProvider is already initialized or terminated",
                ));
            }
            state.initialized = true;
        }
        if let Some(reason) = caller.cancellation.reason() {
            return Err(VendorError::Cancelled(reason));
        }
        let initial = source::load(&self).await?;
        if let Some(reason) = caller.cancellation.reason() {
            return Err(VendorError::Cancelled(reason));
        }
        if let marathon_config::vendor::DeviceProvider::Dynamic { host, port } = &self.services.vendor.device_provider
            && initial.is_none()
        {
            self.log(
                Level::Debug,
                || {
                    format!(
                        "Using dynamic device provider at Dynamic(host={}, port={port})",
                        host.to_string_lossy()
                    )
                },
                None,
            );
        }
        let this = self.clone();
        let context = VendorContext {
            cancellation: self.monitor_cancel.clone(),
            ..self.services.context.clone()
        };
        let task = self
            .services
            .runtime
            .spawn(Box::pin(async move { this.monitor(initial, context).await }));
        self.state().monitoring = Some(task);
        Ok(())
    }
    async fn terminate_inner(&self) -> VendorResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
        self.log(Level::Debug, || "Terminating AppleSimulatorProvider".into(), None);
        self.monitor_cancel.cancel(requested("Provider terminated"));
        let monitoring = self.state().monitoring.take();
        if let Some(task) = monitoring {
            let _ = task.join().await;
        }
        for task in self.update_tasks.take() {
            let _ = task.join().await;
        }
        if let Err(error) = self.services.recovery.terminate().await {
            self.log(
                Level::Error,
                || "Failed to terminate provider recovery".into(),
                Some(error.failure().clone()),
            );
        }
        // A disconnect a recovery raised has its publishing task; it goes out before the channel closes.
        for event in self.events.take() {
            let _ = event.join().await;
        }
        self.channel.close(None);
        let context = VendorContext {
            cancellation: Signal::new(),
            ..self.services.context.clone()
        };
        self.services
            .artifacts
            .close_and_join(300_000, 60_000, &context)
            .await?;
        join_all(self.devices().into_iter().map(|device| async move {
            dispose_then_release(
                device.device.0.dispose(),
                self.services.connections.release(device.handle.executor()),
            )
            .await;
            self.services.registry.remove(&device.device);
            self.log(
                Level::Debug,
                || format!("Disposed device {}", device.handle.udid()),
                None,
            );
        }))
        .await;
        let replaced = {
            let mut state = self.state();
            state.devices.clear();
            std::mem::take(&mut state.replaced)
        };
        for device in replaced {
            let _ = self.services.connections.release(device.executor()).await;
        }
        for event in self.events.take() {
            let _ = event.join().await;
        }
        Ok(())
    }
}
impl DeviceProvider for AppleSimulatorProvider {
    fn initialize(&self, caller: VendorContext) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let this = self.this()?;
            self.dispatch(async move { this.initialize_inner(caller).await }).await
        })
    }
    fn borrow(&self, caller: VendorContext) -> VendorFuture<'_, DeviceRef> {
        Box::pin(async move {
            let this = self.this()?;
            self.dispatch(async move {
                loop {
                    let devices = this.borrowable();
                    if !devices.is_empty() {
                        let keys: Vec<_> = devices.keys().cloned().collect();
                        let key = caller
                            .hooks
                            .choose_key("ios.AppleSimulatorProvider.borrow", &keys)
                            .map_err(clock_error)?;
                        return devices
                            .get(&key)
                            .map(|d| d.device.clone())
                            .ok_or_else(|| other("ReplayError", "Borrow selection is not a device key"));
                    }
                    caller.clock.delay(200, caller.cancellation.clone()).await?;
                }
            })
            .await
        })
    }
    fn subscribe(&self) -> Arc<dyn EventChannel<DeviceEvent>> {
        self.channel.clone()
    }
    fn terminate(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let (done, start) = {
                let mut state = self.state();
                match &state.termination {
                    Some(done) => (done.clone(), false),
                    None => {
                        let done = SharedDeferred::new();
                        state.termination = Some(done.clone());
                        (done, true)
                    }
                }
            };
            if start {
                let this = self.this()?;
                let result = done.clone();
                self.operations.spawn(self.services.runtime.as_ref(), async move {
                    result.complete(this.terminate_inner().await);
                    Ok(())
                });
            }
            done.wait().await.map(|_| ())
        })
    }
}
fn parked_id(parked: &Parked) -> JavaString {
    apple_id(&parked.handle.udid().into(), parked.handle.transport())
}
pub fn apple_id(udid: &JavaString, transport: &Transport) -> JavaString {
    let mut id = udid.clone();
    id.push_str("@");
    id.append(transport.id());
    id
}
fn other(class: &str, message: impl AsRef<str>) -> VendorError {
    VendorError::Other(failure(class, message.as_ref()))
}

struct LinkedSignal {
    parent: Arc<dyn marathon_core::runtime::CancellationSignal>,
    local: Arc<Signal>,
}
impl marathon_core::runtime::CancellationSignal for LinkedSignal {
    fn reason(&self) -> Option<marathon_core::error::Cancellation> {
        self.parent.reason().or_else(|| self.local.reason())
    }
    fn cancelled(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = marathon_core::error::Cancellation> + Send + '_>> {
        Box::pin(async move {
            tokio::select! { biased; reason = self.parent.cancelled() => reason, reason = self.local.cancelled() => reason }
        })
    }
}

pub fn describe_failure(error: &marathon_core::error::Failure) -> String {
    std::iter::successors(Some(error), |e| e.cause.as_deref())
        .take(4)
        .map(|e| {
            e.message.as_ref().map(JavaString::to_string_lossy).unwrap_or_else(|| {
                e.class
                    .to_string_lossy()
                    .rsplit(['.', '$'])
                    .next()
                    .unwrap_or("")
                    .to_owned()
            })
        })
        .collect::<Vec<_>>()
        .join(": ")
}
async fn dispose_then_release(
    dispose: impl std::future::Future<Output = VendorResult<()>>,
    release: impl std::future::Future<Output = VendorResult<()>>,
) {
    if dispose.await.is_ok() {
        let _ = release.await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_event_being_sent_holds_back_the_next_one_on_another_thread() {
        let outbox = Arc::new(Outbox::default());
        outbox.push(0);
        outbox.push(1);
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (entered, in_send) = std::sync::mpsc::channel();
        let (overtaken, was_overtaken) = std::sync::mpsc::channel();
        let first = {
            let (outbox, sent) = (outbox.clone(), sent.clone());
            tokio::task::spawn_blocking(move || {
                outbox.send_oldest(|event| {
                    entered.send(()).unwrap();
                    // A negative window: the second send can only get in here if the lock doesn't cover the send.
                    let _ = was_overtaken.recv_timeout(std::time::Duration::from_millis(300));
                    sent.lock().unwrap().push(event);
                });
            })
        };
        let second = {
            let (outbox, sent) = (outbox.clone(), sent.clone());
            tokio::spawn(async move {
                tokio::task::spawn_blocking(move || {
                    in_send.recv_timeout(std::time::Duration::from_secs(60)).unwrap();
                    outbox.send_oldest(|event| {
                        sent.lock().unwrap().push(event);
                        let _ = overtaken.send(());
                    });
                })
                .await
            })
        };
        first.await.unwrap();
        second.await.unwrap().unwrap();
        assert_eq!(*sent.lock().unwrap(), [0, 1]);
    }
    #[tokio::test]
    async fn a82_throwing_disposal_skips_the_device_connection_release() {
        let released = std::sync::atomic::AtomicUsize::new(0);
        dispose_then_release(
            async { Err(other("java.lang.IllegalStateException", "dispose failed")) },
            async {
                released.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        )
        .await;
        let row: serde_json::Value = include_str!("../../../../fixtures/oracle/simulator-provider/probes.jsonl")
            .lines()
            .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap())
            .find(|r| r["case"] == "A82-throwing-dispose-skips-release")
            .unwrap();
        assert_eq!(
            released.load(std::sync::atomic::Ordering::SeqCst) as u64,
            row["value"]["released"].as_u64().unwrap()
        );
    }
}
