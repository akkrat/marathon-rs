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
    AdamLogging, AdbError, AndroidDebugBridgeClient, ChannelStream, SocketFactory,
    device::{AsyncDeviceMonitorRequest, Device as AdbDevice, DeviceState, ListDevicesRequest},
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
            let connectable = self.reachable(&client, &context).await?;
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
    /// `isConnectable`: asks for the version and, if a loopback server refuses the connection, starts it and asks
    /// again. Initial setup and the reconnect after an outage (D22) share this path.
    async fn reachable(&self, client: &AndroidDebugBridgeClient, context: &VendorContext) -> VendorResult<bool> {
        match self.version(client, context).await {
            Ok(()) => Ok(true),
            Err(e) if is_connect(&e) => {
                let sdk = self.inner.runtime.configuration.android_sdk.as_ref();
                if client.host().is_loopback() && start_adb(sdk, client.port(), context.clone()).await? {
                    match self.version(client, context).await {
                        Ok(()) => Ok(true),
                        Err(e) if is_connect(&e) => Ok(false),
                        Err(e) => Err(e),
                    }
                } else {
                    Ok(false)
                }
            }
            Err(e) => Err(e),
        }
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
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<VendorResult<Tracked>>();
        let mut monitors = Vec::new();
        let clients = lock(&self.inner.clients).clone();
        for client in clients {
            let tx = tx.clone();
            let this = self.clone();
            let context = context.clone();
            let task = self
                .inner
                .runtime
                .dependencies
                .io
                .spawn_cancellable(Box::new(move |signal| {
                    Box::pin(async move {
                        let context = VendorContext {
                            cancellation: signal,
                            ..context
                        };
                        // Each server keeps its own state: a failure here never reaches another server's task (D18).
                        if let Err(e) = this.track_server(client, &tx, context).await {
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
                let (client, devices) = match event? {
                    Tracked::Devices(client, devices) => (client, devices),
                    Tracked::Handled(done) => {
                        let _ = done.send(());
                        continue;
                    }
                };
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
        for task in &monitors {
            task.cancel(requested("device monitor ended"));
        }
        for task in monitors {
            let _ = task.join().await;
        }
        result
    }
    /// D22: follows one adb server's device list for the whole run, across outages. Kotlin's loop means to survive
    /// `adb kill-server`, but a failed track-devices stream cancels it (N16).
    ///
    /// During an outage the devices stay as they are and their batches wait (`AndroidDevice::execute`):
    /// a pool ends with its last device, so removing and adding a device across the outage would end the run. A server
    /// gets `waitForDevicesTimeoutMillis` to return, and its devices the same again to be listed once it has, which is
    /// what a restarted server needs; those not back by then go through the normal disconnect. That bound is what ends
    /// a run whose server never returns.
    async fn track_server(
        &self,
        client: AndroidDebugBridgeClient,
        tx: &TrackedSender,
        context: VendorContext,
    ) -> VendorResult<()> {
        let tracker = self.inner.trackers.tracker(&client);
        let wait = self.inner.runtime.configuration.wait_for_devices_timeout_millis;
        let name = server_name(&client);
        let clock = context.clock.clone();
        let mut stream = client
            .execute_channel(AsyncDeviceMonitorRequest, None)
            .map_err(adb_error)?;
        let mut settling: Option<Settling<'_>> = None;
        loop {
            let step = cancellable(context.cancellation.clone(), async {
                match settling.as_mut() {
                    Some(window) => tokio::select! {
                        next = stream.next() => Ok(TrackStep::Next(next)),
                        elapsed = &mut window.timer => elapsed.map(|()| TrackStep::Settled),
                    },
                    None => Ok(TrackStep::Next(stream.next().await)),
                }
            })
            .await?;
            let cause = match step {
                TrackStep::Next(Some(Ok(devices))) => {
                    settling = self.on_device_list(&client, devices, settling, tx, &context).await?;
                    continue;
                }
                TrackStep::Settled => {
                    if let Some(window) = settling.take() {
                        self.finish_outage(&client, window.kept, window.latest, tx, &context)
                            .await?;
                    }
                    continue;
                }
                TrackStep::Next(Some(Err(e))) => Some(adb_error(e)),
                TrackStep::Next(None) => None,
            };
            // N19: a closed client is the end of the run, not an outage.
            if client.socket_factory().is_closed() {
                return Ok(());
            }
            self.log(
                Level::Warn,
                &format!("Lost device tracking on adb server {name}; reconnecting"),
                cause.as_ref().map(VendorError::failure),
            );
            tracker.set_server_available(false);
            handled(tx, context.cancellation.clone()).await?;
            let kept = tracker.device_serials();
            let recovered = timed(&context, wait, |signal| {
                let context = VendorContext {
                    cancellation: signal,
                    ..context.clone()
                };
                let client = &client;
                Box::pin(async move { self.reconnect(client, context).await })
            })
            .await?;
            let (new_stream, devices) = match recovered {
                Some(recovered) => {
                    self.log(Level::Info, &format!("adb server {name} is back"), None);
                    settling = Some(Settling {
                        kept,
                        latest: vec![],
                        timer: clock.delay(wait, context.cancellation.clone()),
                    });
                    recovered
                }
                None => {
                    self.log(
                        Level::Error,
                        &format!("adb server {name} did not return in {wait} ms; disconnecting its devices"),
                        None,
                    );
                    send(tx, Tracked::Devices(client.clone(), vec![]))?;
                    // Only after the disconnects: a batch released sooner fails on the dead server, again and again.
                    handled(tx, context.cancellation.clone()).await?;
                    tracker.set_server_available(true);
                    settling = None;
                    self.reconnect(&client, context.clone()).await?
                }
            };
            stream = new_stream;
            settling = self.on_device_list(&client, devices, settling, tx, &context).await?;
        }
    }
    /// A list from the stream. While the window after a reconnect is open, a device that was there before the outage
    /// counts as present: a restarted server lists it late, often `offline` first.
    async fn on_device_list<'a>(
        &self,
        client: &AndroidDebugBridgeClient,
        devices: Vec<AdbDevice>,
        settling: Option<Settling<'a>>,
        tx: &TrackedSender,
        context: &VendorContext,
    ) -> VendorResult<Option<Settling<'a>>> {
        let Some(mut window) = settling else {
            send(tx, Tracked::Devices(client.clone(), devices))?;
            return Ok(None);
        };
        let listed = |serial: &String| {
            devices
                .iter()
                .any(|d| d.serial == *serial && d.state == DeviceState::Device)
        };
        let missing: Vec<String> = window.kept.iter().filter(|s| !listed(s)).cloned().collect();
        if missing.is_empty() {
            self.finish_outage(client, window.kept, devices, tx, context).await?;
            return Ok(None);
        }
        let mut held: Vec<AdbDevice> = devices
            .iter()
            .filter(|d| !missing.contains(&d.serial))
            .cloned()
            .collect();
        held.extend(missing.into_iter().map(|serial| AdbDevice {
            serial,
            state: DeviceState::Device,
        }));
        send(tx, Tracked::Devices(client.clone(), held))?;
        window.latest = devices;
        Ok(Some(window))
    }
    /// The real list decides now: a device not on it is disconnected the normal way. A device whose setup the outage
    /// interrupted never had an actor, so it goes through the connect path again. The others keep theirs, with no event,
    /// so no serial ever has two actors.
    async fn finish_outage(
        &self,
        client: &AndroidDebugBridgeClient,
        kept: Vec<String>,
        devices: Vec<AdbDevice>,
        tx: &TrackedSender,
        context: &VendorContext,
    ) -> VendorResult<()> {
        handled(tx, context.cancellation.clone()).await?;
        let server = server_name(client);
        let (interrupted, resumed) = {
            let provided = lock(&self.inner.devices);
            let mine = || provided.iter().filter(|((s, _), _)| *s == server);
            let interrupted: Vec<String> = mine()
                .filter(|((_, serial), p)| kept.contains(serial) && !p.ready.load(Ordering::Acquire))
                .map(|((_, serial), _)| serial.clone())
                .collect();
            let resumed: Vec<AndroidDevice> = mine()
                .filter(|(_, p)| p.ready.load(Ordering::Acquire))
                .map(|(_, p)| p.device.clone())
                .collect();
            (interrupted, resumed)
        };
        if !interrupted.is_empty() {
            let without = devices
                .iter()
                .filter(|d| !interrupted.contains(&d.serial))
                .cloned()
                .collect();
            send(tx, Tracked::Devices(client.clone(), without))?;
        }
        send(tx, Tracked::Devices(client.clone(), devices))?;
        handled(tx, context.cancellation.clone()).await?;
        for device in resumed {
            if device.info().healthy {
                device.resume_after_server_return(context.clone()).await?;
            }
        }
        self.inner.trackers.tracker(client).set_server_available(true);
        Ok(())
    }
    /// Until the server answers and its track-devices stream gives a first list, with a capped backoff on the
    /// vendor clock. An error here only means the server is not back yet.
    async fn reconnect(
        &self,
        client: &AndroidDebugBridgeClient,
        context: VendorContext,
    ) -> VendorResult<(ChannelStream<AsyncDeviceMonitorRequest>, Vec<AdbDevice>)> {
        let mut backoff = RECONNECT_MIN_MILLIS;
        loop {
            context.clock.delay(backoff, context.cancellation.clone()).await?;
            backoff = (backoff * 2).min(RECONNECT_MAX_MILLIS);
            match self.reachable(client, &context).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e @ VendorError::Cancelled(_)) => return Err(e),
                Err(_) => continue,
            }
            let mut stream = client
                .execute_channel(AsyncDeviceMonitorRequest, None)
                .map_err(adb_error)?;
            let first = cancellable(context.cancellation.clone(), async { Ok(stream.next().await) }).await?;
            if let Some(Ok(devices)) = first {
                return Ok((stream, devices));
            }
        }
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
/// D22: the wait between attempts to reach a lost adb server.
const RECONNECT_MIN_MILLIS: i64 = 1000;
const RECONNECT_MAX_MILLIS: i64 = 8000;
type TrackedSender = tokio::sync::mpsc::UnboundedSender<VendorResult<Tracked>>;
/// What the per-server tracking tasks send to the one loop that owns the device events.
enum Tracked {
    Devices(AndroidDebugBridgeClient, Vec<AdbDevice>),
    /// Answered once everything sent before it has been handled.
    Handled(tokio::sync::oneshot::Sender<()>),
}
enum TrackStep {
    Next(Option<Result<Vec<AdbDevice>, AdbError>>),
    Settled,
}
/// The window after a reconnect, in which devices from before the outage may still appear.
struct Settling<'a> {
    kept: Vec<String>,
    latest: Vec<AdbDevice>,
    timer: VendorFuture<'a, ()>,
}
fn send(tx: &TrackedSender, message: Tracked) -> VendorResult<()> {
    tx.send(Ok(message))
        .map_err(|_| VendorError::Cancelled(requested("device monitor ended")))
}
/// Cancellable: the loop that answers joins this task before it drops the question, so it may never answer.
async fn handled(tx: &TrackedSender, signal: Arc<dyn CancellationSignal>) -> VendorResult<()> {
    let (done, wait) = tokio::sync::oneshot::channel();
    send(tx, Tracked::Handled(done))?;
    cancellable(signal, async {
        wait.await
            .map_err(|_| VendorError::Cancelled(requested("device monitor ended")))
    })
    .await
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
