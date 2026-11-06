//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AdamDeviceProvider.kt
//! and adam/src/main/kotlin/com/malinskiy/adam/interactor/{StartAdbInteractor,AdbBinaryInteractor}.kt.
use crate::{
    device::AndroidDevice,
    services::AndroidRuntime,
    support::*,
    tracker::{MultiServerDeviceStateTracker, TrackingUpdate},
};
use jcompat::{
    file_manager::JavaPath,
    logging::{Level, Logger},
};
use marathon_adb::client::{
    AdamLogging, AndroidDebugBridgeClient, SocketFactory,
    device::{AsyncDeviceMonitorRequest, Device as AdbDevice, ListDevicesRequest},
    misc::GetAdbServerVersionRequest,
};
use marathon_core::{
    error::*,
    executor::{SharedChannel, Signal, bridge::call_vendor, shared::requested},
    runtime::*,
    vendor::*,
};
use std::{
    collections::HashMap,
    net::IpAddr,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
#[derive(Clone)]
pub struct AdamDeviceProvider {
    inner: Arc<Inner>,
}
struct Provided {
    device: AndroidDevice,
    reference: DeviceRef,
    task: Arc<dyn VendorTask>,
    ready: Arc<AtomicBool>,
}
struct Inner {
    runtime: Arc<AndroidRuntime>,
    factory: Arc<SocketFactory>,
    logger: Logger,
    channel: Arc<SharedChannel<DeviceEvent>>,
    // D18: keyed by (adb server, serial); Kotlin keys by serial alone (docs/known-kotlin-bugs.md N27).
    devices: Mutex<HashMap<(String, String), Provided>>,
    clients: Mutex<Vec<AndroidDebugBridgeClient>>,
    setups: Mutex<Vec<Arc<dyn VendorTask>>>,
    all_devices: Mutex<Vec<(DeviceRef, AndroidDevice)>>,
    trackers: MultiServerDeviceStateTracker,
    provider: Mutex<Option<Arc<dyn VendorTask>>>,
    shutdown: tokio::sync::Mutex<bool>,
}
impl AdamDeviceProvider {
    pub fn new(runtime: Arc<AndroidRuntime>) -> VendorResult<Self> {
        let factory = Arc::new(SocketFactory::new(
            marathon_adb::client::transport::DEFAULT_CONNECT_TIMEOUT_MS,
            millis(runtime.configuration.timeout_configuration.socket_idle_timeout)?,
        ));
        let logger = runtime.logger("AdamDeviceProvider");
        Ok(Self {
            inner: Arc::new(Inner {
                runtime,
                factory,
                logger,
                channel: SharedChannel::new(),
                devices: Mutex::default(),
                clients: Mutex::default(),
                setups: Mutex::default(),
                all_devices: Mutex::default(),
                trackers: MultiServerDeviceStateTracker::default(),
                provider: Mutex::default(),
                shutdown: tokio::sync::Mutex::new(false),
            }),
        })
    }
    /// Remote parsers recover the Android API without changing core's Device trait.
    pub fn android_device(&self, reference: &DeviceRef) -> Option<AndroidDevice> {
        lock(&self.inner.all_devices)
            .iter()
            .find(|(r, _)| r == reference)
            .map(|(_, d)| d.clone())
    }
    fn log(&self, level: Level, message: &str, error: Option<&Failure>) {
        self.inner.runtime.emit(&self.inner.logger, level, message, error)
    }
    async fn initialize_inner(&self, context: VendorContext) -> VendorResult<()> {
        let runtime = &self.inner.runtime;
        let mut clients = Vec::new();
        for endpoint in &runtime.configuration.adb_servers {
            let host = endpoint.host.to_string_lossy();
            let port = u16::try_from(endpoint.port).map_err(|_| {
                other(
                    "java.lang.IllegalArgumentException",
                    format!("port out of range:{}", endpoint.port).as_str(),
                )
            })?;
            let ip = match host.trim_matches(['[', ']']).parse::<IpAddr>() {
                Ok(ip) => ip,
                Err(_) => {
                    cancellable(context.cancellation.clone(), async {
                        tokio::net::lookup_host((host.as_str(), port))
                            .await
                            .map_err(io)?
                            .next()
                            .map(|s| s.ip())
                            .ok_or_else(|| other("java.net.UnknownHostException", host.as_str()))
                    })
                    .await?
                }
            };
            let registry = runtime.dependencies.logging.clone();
            let sink = runtime.log.clone();
            let registered = registry.clone();
            let logging = AdamLogging::new(registry, move |name, level, text, error| {
                let logger = lock(&registered).slf4j_logger(name);
                let f = error.map(|e| throwable(e.to_throwable()));
                sink(&logger, level, &text.as_str().into(), f.as_ref())
            });
            let client = AndroidDebugBridgeClient::new(ip, port, self.inner.factory.clone(), Some(logging))
                .with_host_name(&host);
            let connectable = match self.version(&client, &context).await {
                Ok(()) => true,
                Err(e) if is_connect(&e) => {
                    if ip.is_loopback()
                        && start_adb(runtime.configuration.android_sdk.as_ref(), port, context.clone()).await?
                    {
                        match self.version(&client, &context).await {
                            Ok(()) => true,
                            Err(e) if is_connect(&e) => false,
                            Err(e) => return Err(e),
                        }
                    } else {
                        false
                    }
                }
                Err(e) => return Err(e),
            };
            if connectable {
                clients.push(client)
            } else {
                self.log(
                    Level::Error,
                    &format!("adb server {} is unavailable", server_name(&client)),
                    None,
                )
            }
        }
        if clients.is_empty() {
            return Err(no_devices("All adb servers are unavailable"));
        }
        // Under waitForDevicesTimeoutMillis (D21).
        let wait_clients: Vec<_> = clients.iter().map(|c| c.without_idle_limit()).collect();
        let wait_clients = &wait_clients;
        let clock = context.clock.clone();
        let found = timed(
            &context,
            runtime.configuration.wait_for_devices_timeout_millis,
            |signal| {
                Box::pin(async move {
                    loop {
                        let mut any = false;
                        for client in wait_clients {
                            let devices = cancellable(signal.clone(), async {
                                client.execute(ListDevicesRequest, None).await.map_err(adb_error)
                            })
                            .await?;
                            any |= !devices.is_empty();
                        }
                        if any {
                            return Ok(());
                        }
                        clock.delay(500, signal.clone()).await?;
                    }
                })
            },
        )
        .await?;
        if found.is_none() {
            return Err(no_devices("No devices found"));
        }
        *lock(&self.inner.clients) = clients;
        let this = self.clone();
        let task = runtime.dependencies.io.spawn_cancellable(Box::new(move |signal| {
            Box::pin(async move {
                let result = this
                    .monitor(VendorContext {
                        cancellation: signal,
                        ..context
                    })
                    .await;
                if let Err(e) = &result
                    && !matches!(e, VendorError::Cancelled(_))
                {
                    this.log(
                        Level::Error,
                        "CoroutineContext closing due to unrecoverable error",
                        Some(e.failure()),
                    );
                }
                result
            })
        }));
        *lock(&self.inner.provider) = Some(task);
        Ok(())
    }
    async fn version(&self, client: &AndroidDebugBridgeClient, context: &VendorContext) -> VendorResult<()> {
        let version = cancellable(context.cancellation.clone(), async {
            client
                .execute(GetAdbServerVersionRequest, None)
                .await
                .map_err(adb_error)
        })
        .await?;
        self.log(
            Level::Debug,
            &format!("Android Debug Bridge {}: version {version}", server_name(client)),
            None,
        );
        Ok(())
    }
    async fn monitor(&self, context: VendorContext) -> VendorResult<()> {
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<VendorResult<(AndroidDebugBridgeClient, Vec<AdbDevice>)>>();
        let mut monitors = Vec::new();
        let clients = lock(&self.inner.clients).clone();
        for client in clients {
            let tx = tx.clone();
            let task = self
                .inner
                .runtime
                .dependencies
                .io
                .spawn_cancellable(Box::new(move |signal| {
                    Box::pin(async move {
                        let result = async {
                            let mut stream = client
                                .execute_channel(AsyncDeviceMonitorRequest, None)
                                .map_err(adb_error)?;
                            loop {
                                let devices = cancellable(signal.clone(), async {
                                    stream.next().await.transpose().map_err(adb_error)
                                })
                                .await?;
                                match devices {
                                    Some(devices) => {
                                        if tx.send(Ok((client.clone(), devices))).is_err() {
                                            break;
                                        }
                                    }
                                    None => break,
                                }
                            }
                            Ok(())
                        }
                        .await;
                        if let Err(e) = result {
                            let _ = tx.send(Err(e));
                        }
                        Ok(())
                    })
                }));
            monitors.push(task);
        }
        drop(tx);
        let result = async {
            loop {
                let event = cancellable(context.cancellation.clone(), async { Ok(rx.recv().await) }).await?;
                let Some(event) = event else { return Ok(()) };
                let (client, devices) = event?;
                let server = server_name(&client);
                for (serial, update) in self.inner.trackers.update(&client, &devices) {
                    match update {
                        TrackingUpdate::Connected => {
                            let device = AndroidDevice::new(
                                client.clone(),
                                serial.clone(),
                                self.inner.trackers.tracker(&client),
                                self.inner.runtime.clone(),
                            );
                            let start = context
                                .hooks
                                .instant("core.Track.trackProviderDevicePreparing")
                                .map_err(|e| other("ReplayError", e.message))?;
                            let reference = device.reference();
                            let connected_reference = reference.clone();
                            let ready = Arc::new(AtomicBool::new(false));
                            let completed = ready.clone();
                            let connected = device.clone();
                            let channel = self.inner.channel.clone();
                            let c = context.clone();
                            let barriers = self.inner.runtime.dependencies.barriers.clone();
                            let key = serial.clone().into();
                            let task = self
                                .inner
                                .runtime
                                .dependencies
                                .io
                                .spawn_cancellable(Box::new(move |signal| {
                                    Box::pin(async move {
                                        let permit = cancellable(signal.clone(), async {
                                            barriers
                                                .enter("android.AdamDeviceProvider.connected", &key)
                                                .await
                                                .map_err(|e| other("marathon.ReplayError", e.message))
                                        })
                                        .await?;
                                        let result = async {
                                            connected
                                                .setup(VendorContext {
                                                    cancellation: signal,
                                                    ..c
                                                })
                                                .await?;
                                            channel.try_send(DeviceEvent::Connected(connected_reference)).map_err(
                                                |_| {
                                                    other(
                                                        "kotlinx.coroutines.channels.ClosedSendChannelException",
                                                        "Channel was closed",
                                                    )
                                                },
                                            )?;
                                            completed.store(true, Ordering::Release);
                                            Ok(())
                                        }
                                        .await;
                                        permit.finish().map_err(|e| other("marathon.ReplayError", e.message))?;
                                        if let Err(e) = &result
                                            && !matches!(e, VendorError::Cancelled(_))
                                        {
                                            connected.log(
                                                Level::Error,
                                                "CoroutineContext closing due to unrecoverable error",
                                                Some(e.failure()),
                                            );
                                        }
                                        result
                                    })
                                }));
                            lock(&self.inner.setups).push(task.clone());
                            lock(&self.inner.all_devices).push((reference.clone(), device.clone()));
                            lock(&self.inner.devices).insert(
                                (server.clone(), serial.clone()),
                                Provided {
                                    device: device.clone(),
                                    reference,
                                    task,
                                    ready,
                                },
                            );
                            let finish = context
                                .hooks
                                .instant("core.Track.trackProviderDevicePreparing")
                                .map_err(|e| other("ReplayError", e.message))?;
                            self.inner
                                .runtime
                                .dependencies
                                .tracker
                                .device_provider_init(&device.serial_number(), start, finish)
                                .await?;
                        }
                        TrackingUpdate::Disconnected => {
                            // D24: dropped from the map, so borrow cannot hand it out once disposed (N27).
                            let provided = lock(&self.inner.devices).remove(&(server.clone(), serial.clone()));
                            if let Some(Provided {
                                device,
                                reference,
                                task,
                                ready,
                            }) = provided
                            {
                                device.mark_disconnected();
                                if !ready.load(Ordering::Acquire) {
                                    task.cancel(requested("device disconnected"));
                                    let _ = task.join().await;
                                }
                                self.inner
                                    .channel
                                    .try_send(DeviceEvent::Disconnected(reference))
                                    .map_err(|_| {
                                        other(
                                            "kotlinx.coroutines.channels.ClosedSendChannelException",
                                            "Channel was closed",
                                        )
                                    })?;
                                device.dispose().await?;
                            }
                        }
                        TrackingUpdate::NothingToDo => {}
                    }
                    if update != TrackingUpdate::NothingToDo {
                        self.log(
                            Level::Debug,
                            &format!("Device {serial} changed state to {}", update.name()),
                            None,
                        )
                    }
                }
            }
        }
        .await;
        // N16: failure of the merged producer ends providerJob; do not reconnect.
        for task in &monitors {
            task.cancel(requested("device monitor ended"));
        }
        for task in monitors {
            let _ = task.join().await;
        }
        result
    }
    async fn terminate_inner(&self) -> VendorResult<()> {
        let mut closed = self.inner.shutdown.lock().await;
        if *closed {
            return Ok(());
        }
        let provider = lock(&self.inner.provider).take();
        if let Some(task) = &provider {
            task.cancel(requested("provider terminated"));
        }
        self.inner.channel.close(None);
        if let Some(task) = provider {
            let _ = task.join().await;
        }
        let setups = lock(&self.inner.setups).clone();
        for task in &setups {
            task.cancel(requested("provider terminated"));
        }
        for task in setups {
            let _ = task.join().await;
        }
        let devices = lock(&self.inner.all_devices).clone();
        for (_, device) in devices {
            device.stop_logcat().await;
        }
        self.inner.factory.close();
        *closed = true;
        Ok(())
    }
}
impl DeviceProvider for AdamDeviceProvider {
    fn initialize(&self, context: VendorContext) -> VendorFuture<'_, ()> {
        let this = self.clone();
        Box::pin(call_vendor(
            self.inner.runtime.dependencies.io.as_ref(),
            context.cancellation.clone(),
            move |signal| {
                Box::pin(async move {
                    this.initialize_inner(VendorContext {
                        cancellation: signal,
                        ..context
                    })
                    .await
                })
            },
        ))
    }
    fn borrow(&self, context: VendorContext) -> VendorFuture<'_, DeviceRef> {
        Box::pin(async move {
            loop {
                let available: Vec<_> = lock(&self.inner.devices)
                    .iter()
                    .filter(|(_, p)| p.ready.load(Ordering::Acquire))
                    .map(|((server, serial), p)| (server.clone(), serial.clone(), p.reference.clone()))
                    .collect();
                if !available.is_empty() {
                    // The replay key stays the bare serial (as in Kotlin) unless several servers expose it (D18).
                    let available: Vec<_> = available
                        .iter()
                        .map(|(server, serial, reference)| {
                            let shared = available.iter().filter(|(_, other, _)| other == serial).count() > 1;
                            let key = if shared {
                                format!("{serial}@{server}")
                            } else {
                                serial.clone()
                            };
                            (key, reference.clone())
                        })
                        .collect();
                    let keys = available.iter().map(|(s, _)| s.as_str().into()).collect::<Vec<_>>();
                    let selected = context
                        .hooks
                        .choose_key("android.AdamDeviceProvider.borrow", &keys)
                        .map_err(|e| other("ReplayError", e.message))?;
                    if let Some((_, device)) = available.into_iter().find(|(s, _)| selected == s.as_str().into()) {
                        return Ok(device);
                    }
                }
                context.clock.delay(200, context.cancellation.clone()).await?;
            }
        })
    }
    fn terminate(&self) -> VendorFuture<'_, ()> {
        let this = self.clone();
        Box::pin(call_vendor(
            self.inner.runtime.dependencies.io.as_ref(),
            Signal::new(),
            move |_| Box::pin(async move { this.terminate_inner().await }),
        ))
    }
    fn subscribe(&self) -> Arc<dyn EventChannel<DeviceEvent>> {
        self.inner.channel.clone()
    }
}
fn is_connect(e: &VendorError) -> bool {
    [
        "io.netty.channel.AbstractChannel$AnnotatedConnectException",
        "io.netty.channel.ConnectTimeoutException",
        "java.net.ConnectException",
    ]
    .iter()
    .any(|c| e.failure().class == (*c).into())
}
fn no_devices(message: &str) -> VendorError {
    VendorError::NoDevices(failure("com.malinskiy.marathon.exceptions.NoDevicesException", message))
}
fn server_name(client: &AndroidDebugBridgeClient) -> String {
    format!(
        "{}/{}:{}",
        client.host_name().unwrap_or(""),
        crate::device::host_address(client.host()),
        client.port()
    )
}
/// Runs only the selected binary; tests always provide a temp SDK with a fake adb.
pub async fn start_adb(sdk: Option<&JavaPath>, port: u16, context: VendorContext) -> VendorResult<bool> {
    let home = sdk.map(|p| p.to_file_path_buf().map_err(io)).transpose()?.or_else(|| {
        std::env::var_os("ANDROID_HOME")
            .or_else(|| std::env::var_os("ANDROID_SDK_ROOT"))
            .map(PathBuf::from)
    });
    let binary = if let Some(home) = home {
        home.join("platform-tools/adb")
    } else {
        let result = tokio::process::Command::new("which")
            .arg("adb")
            .output()
            .await
            .map_err(io)?;
        PathBuf::from(
            jcompat::jstring::lines(&jcompat::utf8::decode_utf8(&result.stdout).as_str().into())
                .first()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default(),
        )
    };
    if !tokio::fs::metadata(&binary).await.is_ok_and(|m| m.is_file()) {
        return Ok(false);
    }
    let binary = std::path::absolute(binary).map_err(io)?;
    let mut child = tokio::process::Command::new(binary)
        .args(["-P", &port.to_string(), "start-server"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(io)?;
    loop {
        if let Some(exit) = child.try_wait().map_err(io)? {
            return Ok(exit.success());
        }
        if let Err(e) = context.clock.delay(16, context.cancellation.clone()).await {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(e);
        }
    }
}
