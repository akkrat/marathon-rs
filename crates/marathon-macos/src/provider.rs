//! Port of vendor/vendor-apple/macos/src/main/kotlin/com/malinskiy/marathon/apple/macos/AppleMacosProvider.kt

use marathon_logging::{Component, Level};

use crate::device::{MacosDevice, MacosDeviceOptions};
use indexmap::IndexMap;
use jcompat::file_manager::absolute_file_path;
use marathon_apple::{
    apple_device::AppleDevice,
    apple_device_registry::AppleDeviceRegistry,
    bin::{AppleBinaryEnvironment, xcrun::xcresulttool::describe_timeout},
    cmd::{failure, retained_tasks::RetainedTasks},
    device::connection_factory::{Bridge, ConnectionFactory, Executor, transport_text},
};
use marathon_config::{
    binding::{BindingContext, ConfigurationError, Profile},
    devices::{AppleTarget, MarathonDevices, Transport},
    loader::{DevicesProvider, MarathonConfigurationLoader},
};
use marathon_core::{
    analytics::track_device_operation,
    error::{Failure, VendorError, VendorResult},
    executor::shared::SharedChannel,
    runtime::{EventChannel, VendorFuture},
    vendor::{Device, DeviceEvent, DeviceProvider, DeviceRef, VendorContext},
};
use marathon_error::{ConfigKind, FailureKind};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

/// The production implementation owns ConnectionFactory leases. Recording worlds replace only transport I/O.
pub trait MacosConnections: Send + Sync {
    fn transport_log_text(&self, transport: &Transport, local: &str) -> String {
        transport_text(transport, local)
    }
    fn create(&self, transport: Transport, context: VendorContext) -> VendorFuture<'_, (Option<Executor>, Bridge)>;
    fn release(&self, executor: Executor) -> VendorFuture<'_, ()>;
    /// D93: instead of `release`, for a Mac whose work could not be confirmed to have ended: its use of the connection is
    /// kept for the rest of the run and never handed to another acquisition. Keeping it is the default.
    fn quarantine(&self, executor: &Executor) {
        let _ = executor;
    }
}
impl MacosConnections for Arc<ConnectionFactory> {
    fn transport_log_text(&self, transport: &Transport, local: &str) -> String {
        ConnectionFactory::transport_log_text(self, transport, local)
    }
    fn create(&self, transport: Transport, context: VendorContext) -> VendorFuture<'_, (Option<Executor>, Bridge)> {
        Box::pin(async move {
            ConnectionFactory::create(self, transport, context)
                .await
                .map_err(|e| e.into_vendor())
        })
    }
    fn release(&self, executor: Executor) -> VendorFuture<'_, ()> {
        Box::pin(async move { self.dispose(executor).await })
    }
    fn quarantine(&self, executor: &Executor) {
        ConnectionFactory::quarantine(self, executor)
    }
}
pub struct AppleMacosProvider {
    pub options: MacosDeviceOptions,
    pub registry: Arc<AppleDeviceRegistry>,
    connections: Arc<dyn MacosConnections>,
    devices: Mutex<IndexMap<String, Arc<MacosDevice>>>,
    /// U200: set by termination while it holds `devices`, and read by `connect` under the same lock, so a Mac either
    /// joins the map and registry before termination takes them or never joins them.
    terminated: AtomicBool,
    /// Every device constructed, for `join_pending`, including one whose setup failed or that was replaced.
    retained: Mutex<Vec<Arc<MacosDevice>>>,
    leases: Mutex<Vec<Executor>>,
    channel: Arc<SharedChannel<DeviceEvent>>,
    tasks: RetainedTasks,
    logger: Component,
    process_dir: PathBuf,
    environment: Vec<(String, String)>,
}
impl AppleMacosProvider {
    pub fn new(
        options: MacosDeviceOptions,
        registry: Arc<AppleDeviceRegistry>,
        connections: Arc<dyn MacosConnections>,
        process_dir: PathBuf,
        environment: Vec<(String, String)>,
    ) -> Arc<Self> {
        let logger = options.logging.component("marathon_macos::provider");
        Arc::new(Self {
            options,
            registry,
            connections,
            devices: Mutex::default(),
            terminated: AtomicBool::new(false),
            retained: Mutex::default(),
            leases: Mutex::default(),
            channel: SharedChannel::new(),
            tasks: RetainedTasks::default(),
            logger,
            process_dir,
            environment,
        })
    }
    fn log(&self, level: Level, message: String) {
        self.options.logging.log(&self.logger, level, || message, None);
    }
    pub fn snapshot(&self) -> Vec<Arc<MacosDevice>> {
        self.devices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }
    fn read_devices(&self) -> VendorResult<MarathonDevices> {
        let path = self.options.vendor.devices_file.clone().unwrap_or_else(|| {
            PathBuf::from(&format!(
                "{}/Marathondevices",
                self.process_dir.to_string_lossy().into_owned()
            ))
        });
        let absolute = absolute_file_path(&path.to_string_lossy(), &self.process_dir.to_string_lossy());
        let bytes = std::fs::read(&absolute).map_err(|e| {
            let suffix = match e.kind() {
                std::io::ErrorKind::NotFound => "No such file or directory",
                std::io::ErrorKind::PermissionDenied => "Permission denied",
                _ => "Is a directory",
            };
            {
                let mut detail = failure(
                    FailureKind::FileNotFound,
                    format!("{} ({suffix})", path.to_string_lossy().into_owned()),
                );
                let class = match e.kind() {
                    std::io::ErrorKind::NotFound => Some(FailureKind::NoSuchFile),
                    std::io::ErrorKind::PermissionDenied => Some(FailureKind::AccessDenied),
                    _ => None,
                };
                detail.cause = class.map(|class| Box::new(failure(class, path.to_string_lossy())));
                VendorError::Other(detail)
            }
        })?;
        let context = BindingContext {
            profile: Profile::Marathondevices,
            marathonfile: path,
            analytics_tracking: false,
            bugsnag_reporting: false,
            marathonfile_dir: self.process_dir.clone(),
            process_dir: self.process_dir.clone(),
            environment: self.environment.clone(),
            now: jcompat::jtime::Instant::EPOCH,
        };
        let devices = MarathonConfigurationLoader
            .devices_sync_for(DevicesProvider::Macos, &bytes, &context)
            .map_err(|e| {
                let mapping = matches!(e.kind, FailureKind::Config(kind) if kind.is_mapping())
                    || e.kind == FailureKind::Config(ConfigKind::MissingParameter);
                let cause = configuration_failure(&e);
                if mapping {
                    let mut f = failure(
                        FailureKind::NoDevices,
                        format!(
                            "Invalid Marathondevices file {} format",
                            absolute.to_string_lossy().into_owned()
                        ),
                    );
                    f.cause = Some(Box::new(cause));
                    VendorError::NoDevices(f)
                } else {
                    VendorError::Other(cause)
                }
            })?;
        if devices.workers.is_empty() {
            return Err(VendorError::NoDevices(failure(
                FailureKind::NoDevices,
                format!("No workers found in the {}", absolute.to_string_lossy().into_owned()),
            )));
        }
        Ok(devices)
    }
    pub async fn initialize_transport(&self, transport: Transport, context: VendorContext) -> VendorResult<()> {
        let (executor, bridge) = self.connections.create(transport.clone(), context.clone()).await?;
        let Some(executor) = executor else {
            return Ok(());
        };
        self.leases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(executor.clone());
        let device = match self.set_up(executor.clone(), transport, bridge, &context).await {
            Ok(device) => device,
            Err(error) => {
                // A89 (D74): a lease taken for a setup that failed is released at once; the setup's error is kept.
                self.release_logged(executor).await;
                return Err(error);
            }
        };
        self.connect(device, &context).await
    }
    async fn set_up(
        &self,
        executor: Executor,
        transport: Transport,
        bridge: Bridge,
        context: &VendorContext,
    ) -> VendorResult<Arc<MacosDevice>> {
        let deps = &self.options.dependencies;
        // D93: every command of the Mac goes through its own handle on the lease, which its disposal can revoke.
        let commands = marathon_apple::cmd::DeviceCommands::new(executor);
        let binary = AppleBinaryEnvironment::new(
            commands.clone(),
            &deps.configuration,
            self.options.vendor.timeout_configuration.clone(),
            deps.io.clone(),
            self.options.logging.clone(),
        );
        let mut udid = binary.system_profiler.get_provisioning_udid(context).await?;
        if marathon_apple::bin::kt::trim(&udid).is_empty() {
            udid = binary.ioreg.get_udid(context).await?;
        }
        let device = MacosDevice::new(udid, transport, binary, commands, bridge, self.options.clone());
        // D93: a disposal that cannot confirm the Mac's work ended quarantines its lease in the factory at once.
        let connections = self.connections.clone();
        device.on_lease_quarantine(Arc::new(move |executor| connections.quarantine(executor)));
        self.retained
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(device.clone());
        let label = "core.Track.trackProviderDevicePreparing";
        track_device_operation(
            context.cancellation.as_ref(),
            || async { context.hooks.instant(label).map_err(crate::clock_error) },
            device.setup(context),
            |start, finish| {
                let device = &device;
                async move {
                    deps.tracker
                        .device_provider_init(&device.info().serial_number, start, finish)
                        .await
                }
            },
            |error| device.timing_log.warn("setup", &device.info().serial_number, error),
        )
        .await?;
        Ok(device)
    }
    /// A failed release costs only that lease: it is logged, and never replaces the error or outcome of the work
    /// that took it.
    async fn release_logged(&self, executor: Executor) {
        let host = executor.host().id.clone();
        if let Err(error) = self.connections.release(executor).await
            && self.options.logging.enabled(&self.logger, Level::ERROR)
        {
            (self.options.logging.sink)(
                &self.logger,
                Level::ERROR,
                format!("Failed to release connection to worker {host}"),
                Some(error.failure().clone()),
            );
        }
    }
    /// A89, A82 (D74): a Mac's lease is released after its disposal. D93: unless that disposal could not confirm that the
    /// Mac's work had ended; the lease is then quarantined, so that the connection never closes under that work.
    async fn release_after_disposal(&self, device: &MacosDevice) -> VendorResult<()> {
        if device.quarantined() {
            // Its disposal has quarantined the lease in the factory already.
            return Ok(());
        }
        let release = self.connections.release(device.executor());
        let Some(deadline) = device.shutdown_deadline() else {
            return release.await;
        };
        // D93: the wait is held to the Mac's window; a close that outlasts it goes on in the factory's own task.
        let clock = self.options.dependencies.clock.clone();
        let left = deadline.saturating_sub(clock.now_millis()).max(0);
        tokio::select! { biased;
            released = release => released,
            _ = clock.delay(left, marathon_core::executor::shared::Signal::new()) => {
                self.log(
                    Level::DEBUG,
                    format!("Not waiting for the connection of {} to close beyond its window", device.udid()),
                );
                Ok(())
            }
        }
    }
    async fn release_after_disposal_logged(&self, device: &MacosDevice) {
        let host = device.executor().host().id.clone();
        if let Err(error) = self.release_after_disposal(device).await
            && self.options.logging.enabled(&self.logger, Level::ERROR)
        {
            (self.options.logging.sink)(
                &self.logger,
                Level::ERROR,
                format!("Failed to release connection to worker {host}"),
                Some(error.failure().clone()),
            );
        }
    }
    async fn connect(&self, device: Arc<MacosDevice>, _context: &VendorContext) -> VendorResult<()> {
        let previous = {
            let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
            if self.terminated.load(Ordering::SeqCst) {
                None
            } else {
                Some(devices.insert(device.info().serial_number, device.clone()))
            }
        };
        let Some(previous) = previous else {
            // U200: termination came first and will not see this Mac, so its disposal and lease stay here.
            self.log(
                Level::DEBUG,
                format!(
                    "Provider terminated; disposing {} instead of connecting it",
                    device.udid()
                ),
            );
            let disposed = device.dispose().await;
            self.release_after_disposal_logged(&device).await;
            return disposed;
        };
        let mut disposed = Ok(());
        if let Some(previous) = previous {
            self.log(
                Level::ERROR,
                "replaced existing device MacosDevice with new MacosDevice.".into(),
            );
            // A89 (D74): the replaced Mac's lease is released after its disposal, whether or not that failed.
            disposed = previous.dispose().await;
            // D84 (R3-2): not under a preparation the replaced Mac still runs, within its bound.
            let _ = previous.join_preparation().await;
            self.release_after_disposal_logged(&previous).await;
            // A107: a failed disposal no longer stops the replacement. The old Mac leaves the registry and the new one
            // is registered and announced; the (fatal) error is returned afterwards, so it still ends the run.
            self.registry.remove(&DeviceRef(previous));
        }
        // U200: a termination that ran while the predecessor was disposed of has taken this Mac with the map and
        // disposes of it; it must not then be registered or announced.
        let registered = {
            let _devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
            (!self.terminated.load(Ordering::SeqCst)).then(|| self.registry.register(device))
        };
        let Some(registered) = registered else {
            return disposed;
        };
        let event = DeviceEvent::Connected(registered);
        let channel = self.channel.clone();
        self.tasks.spawn(&*self.options.dependencies.io, async move {
            channel.try_send(event).map_err(|e| {
                VendorError::Other(
                    e.cause
                        .unwrap_or_else(|| failure(FailureKind::ChannelSendClosed, "Channel was closed")),
                )
            })
        });
        disposed
    }
    pub async fn join_pending(&self) -> VendorResult<()> {
        let mut outcome = Ok(());
        for task in self.tasks.take() {
            let joined = task.join().await;
            outcome = outcome.and(joined);
        }
        let devices = self.retained.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for device in devices {
            let joined = device.join_pending().await;
            outcome = outcome.and(joined);
        }
        outcome
    }
}
impl DeviceProvider for AppleMacosProvider {
    fn initialize(&self, context: VendorContext) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.log(Level::DEBUG, "Initializing AppleMacosProvider".into());
            self.log(
                Level::INFO,
                describe_timeout(&self.options.vendor.timeout_configuration),
            );
            let input = self.read_devices()?;
            let mut hosts = IndexMap::new();
            for worker in input.workers {
                hosts.insert(worker.transport, worker.devices);
            }
            if self.options.logging.enabled(&self.logger, Level::DEBUG) {
                let local = "Local";
                self.log(
                    Level::DEBUG,
                    format!(
                        "Establishing communication with [{}]",
                        hosts
                            .keys()
                            .map(|t| self.connections.transport_log_text(t, local))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
            let transports = hosts.into_iter().filter_map(|(transport, targets)| {
                let mut keep = false;
                for target in targets {
                    if target == AppleTarget::Host {
                        keep = true;
                    } else {
                        self.log(
                            Level::WARN,
                            "macOS vendor runs do not use anything but host device for testing. Skipping".into(),
                        );
                    }
                }
                keep.then_some(transport)
            });
            use futures_util::{StreamExt, stream::FuturesUnordered};
            let signal = marathon_core::executor::shared::Signal::new();
            let mut child = context.clone();
            child.cancellation = signal.clone();
            let mut pending: FuturesUnordered<_> = transports
                .map(|transport| self.initialize_transport(transport, child.clone()))
                .collect();
            let mut failure = None;
            while !pending.is_empty() {
                let result = tokio::select! { biased;
                    reason = context.cancellation.cancelled(), if failure.is_none() => {
                        signal.cancel(reason.clone());
                        failure = Some(VendorError::Cancelled(reason));
                        continue;
                    },
                    result = pending.next() => result,
                };
                if let Some(Err(error)) = result
                    && failure.is_none()
                {
                    signal.cancel(marathon_apple::cmd::cancelled("Parent job is Cancelling"));
                    failure = Some(error);
                }
            }
            if let Some(error) = failure {
                return Err(error);
            }
            Ok(())
        })
    }
    fn borrow(&self, context: VendorContext) -> VendorFuture<'_, DeviceRef> {
        Box::pin(async move { Ok(self.borrow_untried(Vec::new(), context).await?.0) })
    }
    fn borrow_untried(&self, tried: Vec<String>, context: VendorContext) -> VendorFuture<'_, (DeviceRef, String)> {
        Box::pin(async move {
            loop {
                // D53 (U171): nothing removes a Mac whose connection is lost, and nothing reconnects it, so a parse on one
                // fails an attempt. As iOS's D44: only a Mac that is connected, and a healthy run's keys are all of them.
                let mut devices = self.snapshot();
                devices.retain(|d| d.executor().connected());
                let keys = devices.iter().map(|d| d.info().serial_number).collect::<Vec<_>>();
                let untried: Vec<_> = keys.iter().filter(|k| !tried.contains(k)).cloned().collect();
                let keys = if untried.is_empty() { keys } else { untried };
                if !keys.is_empty() {
                    let key = context
                        .hooks
                        .choose_key("macos.AppleMacosProvider.borrow", &keys)
                        .map_err(crate::clock_error)?;
                    let device = devices.iter().find(|d| d.info().serial_number == key).ok_or_else(|| {
                        VendorError::Other(failure(FailureKind::Replay, "Unknown macOS device selection"))
                    })?;
                    return Ok((DeviceRef(device.clone()), key));
                }
                context.clock.delay(200, context.cancellation.clone()).await?;
            }
        })
    }
    fn terminate(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.log(Level::DEBUG, "Terminating AppleMacosProvider".into());
            self.channel.close(None);
            // D74: termination takes the Macs out first, so a repeated call after a fatal disposal finds none to release
            // again.
            let devices = {
                let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
                self.terminated.store(true, Ordering::SeqCst);
                std::mem::take(&mut *devices)
            };
            let results = futures_util::future::join_all(devices.into_values().map(|device| async move {
                // A82 for macOS (D74): the release follows the disposal whatever its outcome; the disposal's error wins.
                let disposed = device.dispose().await;
                // D84 (R3-2): not under a preparation the run left running, within the Mac's bound.
                let _ = device.join_preparation().await;
                let released = self.release_after_disposal(&device).await;
                let outcome = disposed.and(released);
                // M-x69-2: the Mac has left the map and its lease is released, so it leaves the registry too, even on a
                // fatal disposal.
                self.registry.remove(&DeviceRef(device.clone()));
                if let Err(error) = outcome
                    && error.failure().kind.is_fatal()
                {
                    return Err(error);
                }
                self.log(Level::DEBUG, format!("Disposed device {}", device.udid()));
                Ok(())
            }))
            .await;
            for result in results {
                result?;
            }
            Ok(())
        })
    }
    fn subscribe(&self) -> Arc<dyn EventChannel<DeviceEvent>> {
        self.channel.clone()
    }
}
fn configuration_failure(error: &ConfigurationError) -> Failure {
    let cause = error.causes.iter().rev().fold(None, |cause, c| {
        Some(Box::new(Failure {
            kind: c.kind.clone(),
            message: c.message.to_owned(),
            cause,
            stacktrace: None,
        }))
    });
    Failure {
        kind: error.kind.clone(),
        message: error.message.to_owned(),
        cause,
        stacktrace: None,
    }
}
