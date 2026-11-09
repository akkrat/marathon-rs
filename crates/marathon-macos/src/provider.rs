//! Port of vendor/vendor-apple/macos/src/main/kotlin/com/malinskiy/marathon/apple/macos/AppleMacosProvider.kt

use crate::device::{MacosDevice, MacosDeviceOptions};
use indexmap::IndexMap;
use jcompat::{
    file_manager::absolute_file_path,
    logging::{Level, Logger},
};
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
    error::{Failure, VendorError, VendorResult},
    executor::shared::SharedChannel,
    runtime::{EventChannel, VendorFuture},
    vendor::{Device, DeviceEvent, DeviceProvider, DeviceRef, VendorContext},
};
use marathon_error::{ConfigKind, FailureKind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The production implementation owns ConnectionFactory leases. Recording worlds replace only transport I/O.
pub trait MacosConnections: Send + Sync {
    fn create(&self, transport: Transport, context: VendorContext) -> VendorFuture<'_, (Option<Executor>, Bridge)>;
    fn release(&self, executor: Executor) -> VendorFuture<'_, ()>;
}
impl MacosConnections for Arc<ConnectionFactory> {
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
}
pub struct AppleMacosProvider {
    pub options: MacosDeviceOptions,
    pub registry: Arc<AppleDeviceRegistry>,
    connections: Arc<dyn MacosConnections>,
    devices: Mutex<IndexMap<String, Arc<MacosDevice>>>,
    // A89: failed setup/replaced allocations keep ownership, without adding factory release calls.
    retained: Mutex<Vec<Arc<MacosDevice>>>,
    leases: Mutex<Vec<Executor>>,
    channel: Arc<SharedChannel<DeviceEvent>>,
    tasks: RetainedTasks,
    logger: Logger,
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
        let logger = options.logging.logger("AppleMacosProvider");
        Arc::new(Self {
            options,
            registry,
            connections,
            devices: Mutex::default(),
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
        let deps = &self.options.dependencies;
        let binary = AppleBinaryEnvironment::new(
            executor,
            &deps.configuration,
            self.options.vendor.timeout_configuration.clone(),
            deps.io.clone(),
            self.options.logging.clone(),
        );
        let mut udid = binary.system_profiler.get_provisioning_udid(&context).await?;
        if marathon_apple::bin::kt::trim(&udid).is_empty() {
            udid = binary.ioreg.get_udid(&context).await?;
        }
        let device = MacosDevice::new(udid, transport, binary, bridge, self.options.clone());
        self.retained
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(device.clone());
        let start = context
            .hooks
            .instant("core.Track.trackProviderDevicePreparing")
            .map_err(crate::clock_error)?;
        device.setup(&context).await?;
        let finish = context
            .hooks
            .instant("core.Track.trackProviderDevicePreparing")
            .map_err(crate::clock_error)?;
        deps.tracker
            .device_provider_init(&device.info().serial_number, start, finish)
            .await?;
        self.connect(device, &context).await
    }
    async fn connect(&self, device: Arc<MacosDevice>, _context: &VendorContext) -> VendorResult<()> {
        let previous = self
            .devices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(device.info().serial_number, device.clone());
        if let Some(previous) = previous {
            self.log(
                Level::Error,
                "replaced existing device MacosDevice with new MacosDevice.".into(),
            );
            previous.dispose().await?;
            self.registry.remove(&DeviceRef(previous));
        }
        let event = DeviceEvent::Connected(self.registry.register(device));
        let channel = self.channel.clone();
        self.tasks.spawn(&*self.options.dependencies.io, async move {
            channel.try_send(event).map_err(|e| {
                VendorError::Other(
                    e.cause
                        .unwrap_or_else(|| failure(FailureKind::ChannelSendClosed, "Channel was closed")),
                )
            })
        });
        Ok(())
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
            self.log(Level::Debug, "Initializing AppleMacosProvider".into());
            self.log(
                Level::Info,
                describe_timeout(&self.options.vendor.timeout_configuration),
            );
            let input = self.read_devices()?;
            let mut hosts = IndexMap::new();
            for worker in input.workers {
                hosts.insert(worker.transport, worker.devices);
            }
            if self.options.logging.enabled(&self.logger, Level::Debug) {
                let local = "Local";
                self.log(
                    Level::Debug,
                    format!(
                        "Establishing communication with [{}]",
                        hosts
                            .keys()
                            .map(|t| transport_text(t, local))
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
                            Level::Warn,
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
            self.log(Level::Debug, "Terminating AppleMacosProvider".into());
            self.channel.close(None);
            let results = futures_util::future::join_all(self.snapshot().into_iter().map(|device| async move {
                let outcome = async {
                    device.dispose().await?;
                    self.connections.release(device.executor()).await
                }
                .await;
                if let Err(error) = outcome
                    && error.failure().kind.is_fatal()
                {
                    return Err(error);
                }
                self.registry.remove(&DeviceRef(device.clone()));
                self.log(Level::Debug, format!("Disposed device {}", device.udid()));
                Ok(())
            }))
            .await;
            for result in results {
                result?;
            }
            self.devices.lock().unwrap_or_else(|e| e.into_inner()).clear();
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
