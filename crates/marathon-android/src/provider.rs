//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AdamDeviceProvider.kt
//! and adam/src/main/kotlin/com/malinskiy/adam/interactor/{StartAdbInteractor,AdbBinaryInteractor}.kt.

use crate::{
    device::{AndroidDevice, ReturnedBoot},
    services::AndroidRuntime,
    support::*,
    tracker::{DeviceStateTracker, MultiServerDeviceStateTracker, TrackingUpdate, is_passing},
};
use futures_util::FutureExt;
use marathon_adb::client::{
    AdamLogging, AdbError, AndroidDebugBridgeClient, ChannelStream, SocketFactory,
    device::{AsyncDeviceMonitorRequest, Device as AdbDevice, DeviceState, ListDevicesRequest},
    misc::GetAdbServerVersionRequest,
};
use marathon_core::{
    error::*,
    execution::scheduler::STOP_MARGIN_MILLIS,
    executor::{
        SharedChannel, Signal,
        bridge::{JoiningTasks, call_vendor},
        shared::requested,
    },
    runtime::*,
    vendor::*,
};
use marathon_error::{AdbKind, FailureKind};
use marathon_logging::{Component, Level};
use std::{
    collections::HashMap,
    net::IpAddr,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
};
#[derive(Clone)]
pub struct AdamDeviceProvider {
    inner: Arc<Inner>,
}
struct Provided {
    device: AndroidDevice,
    reference: DeviceRef,
    task: Arc<dyn VendorTask>,
    ready: Arc<Mutex<bool>>,
    /// A replacement inherits the allowance while adb still lists this serial on the same boot.
    retried_prepare: bool,
}
struct Inner {
    runtime: Arc<AndroidRuntime>,
    factory: Arc<SocketFactory>,
    logger: Component,
    channel: Arc<SharedChannel<DeviceEvent>>,
    // D18: keyed by (adb server, serial); Kotlin keys by serial alone (docs/known-kotlin-bugs.md N27).
    devices: Mutex<HashMap<(String, String), Provided>>,
    clients: Mutex<Vec<AndroidDebugBridgeClient>>,
    setups: Mutex<Vec<Arc<dyn VendorTask>>>,
    all_devices: Mutex<Vec<(DeviceRef, AndroidDevice)>>,
    trackers: MultiServerDeviceStateTracker,
    provider: Mutex<Option<Arc<dyn VendorTask>>>,
    shutdown: tokio::sync::Mutex<bool>,
    signal_finish: marathon_core::signal_finish::SignalDeadline,
    /// D88: why device monitoring ended, if it did other than at termination; the device channel fails with the same.
    monitor_failure: Mutex<Option<Failure>>,
}
impl AdamDeviceProvider {
    pub fn new(runtime: Arc<AndroidRuntime>) -> VendorResult<Self> {
        let factory = Arc::new(SocketFactory::new(
            marathon_adb::client::transport::DEFAULT_CONNECT_TIMEOUT_MS,
            millis(runtime.configuration.timeout_configuration.socket_idle_timeout)?,
        ));
        let logger = runtime.component("marathon_android::provider");
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
                signal_finish: Default::default(),
                monitor_failure: Mutex::default(),
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
    /// D88: device monitoring has ended with `cause`. Published (kept for `published_failure`, and the device channel
    /// failed with it) before anything is logged, so that neither a log sink that fails nor a termination that comes
    /// while it writes can lose it. The first cause stays.
    fn publish_failure(&self, cause: Failure) {
        lock(&self.inner.monitor_failure).get_or_insert_with(|| cause.clone());
        self.inner.channel.close(Some(cause));
        for (_, device) in &*lock(&self.inner.all_devices) {
            device.seal();
        }
    }
    fn log(&self, level: Level, message: &str, error: Option<&Failure>) {
        self.inner.runtime.emit(&self.inner.logger, level, message, error)
    }
    async fn initialize_inner(&self, context: VendorContext) -> VendorResult<()> {
        let runtime = &self.inner.runtime;
        let mut clients = Vec::new();
        for endpoint in &runtime.configuration.adb_servers {
            let host = endpoint.host.to_owned();
            let port = u16::try_from(endpoint.port).map_err(|_| {
                other(
                    FailureKind::IllegalArgument,
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
                            .ok_or_else(|| other(FailureKind::UnknownHost, host.as_str().to_owned()))
                    })
                    .await?
                }
            };
            let registry = runtime.dependencies.logging.clone();
            let sink = runtime.log.clone();
            let registered = registry.clone();
            let logging = AdamLogging::new(registry, move |name, level, text, error| {
                let logger = lock(&registered).component(name);
                let f = error.map(|e| throwable(e.to_failure()));
                sink(&logger, level, text.as_str(), f.as_ref())
            });
            let client = AndroidDebugBridgeClient::new(ip, port, self.inner.factory.clone(), Some(logging))
                .with_host_name(&host);
            let connectable = self.reachable(&client, &context).await?;
            if connectable {
                clients.push(client)
            } else {
                self.log(
                    Level::ERROR,
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
        let mut monitor = lock(&self.inner.provider);
        if EventChannel::state(self.inner.channel.as_ref()).closed_for_send {
            return Err(VendorError::Cancelled(requested("device provider is closed")));
        }
        let task = runtime
            .dependencies
            .io
            .spawn_cancellable_interruptible(Box::new(move |signal| {
                Box::pin(async move {
                    let stopped = signal.clone();
                    let result = std::panic::AssertUnwindSafe(this.monitor(VendorContext {
                        cancellation: signal,
                        ..context
                    }))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|payload| Err(VendorError::Other(marathon_core::executor::panic_failure(payload))));
                    // U211 (D88): the monitor is the only source of device events and the only one to release a batch held
                    // by an outage or a grace period (D22, D41). If it ends for any reason but the provider's termination,
                    // the device channel fails with the cause, which ends the run (D84) instead of leaving it waiting. The
                    // monitor publishes its own failure before it joins its servers' tasks; this covers a panic outside its
                    // loop. First-wins, and before the log line below.
                    if let Err(e) = &result
                        && stopped.reason().is_none()
                    {
                        this.publish_failure(e.failure().clone());
                    }
                    if let Err(e) = &result
                        && !matches!(e, VendorError::Cancelled(_))
                    {
                        // A log sink that panics costs only this line.
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            this.log(
                                Level::ERROR,
                                "Task stopped due to unrecoverable error",
                                Some(e.failure()),
                            )
                        }));
                    }
                    result
                })
            }));
        *monitor = Some(task);
        Ok(())
    }
    /// `isConnectable`: asks for the version and, if a loopback server refuses the connection, starts it and asks
    /// again. Initial setup and the reconnect after an outage (D22) share this path.
    async fn reachable(&self, client: &AndroidDebugBridgeClient, context: &VendorContext) -> VendorResult<bool> {
        match self.version(client, context).await {
            Ok(()) => Ok(true),
            Err(e) if is_connect(&e) => {
                let sdk = self.inner.runtime.configuration.android_sdk.as_deref();
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
            Level::DEBUG,
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
                .spawn_cancellable_interruptible(Box::new(move |signal| {
                    Box::pin(async move {
                        let context = VendorContext {
                            cancellation: signal,
                            ..context
                        };
                        // Each server keeps its own state: a failure here never reaches another server's task (D18).
                        // U211: a panic is such a failure too, rather than a server whose tracking ends unseen.
                        let tracked = std::panic::AssertUnwindSafe(this.track_server(client, &tx, context))
                            .catch_unwind()
                            .await
                            .unwrap_or_else(|payload| {
                                Err(VendorError::Other(marathon_core::executor::panic_failure(payload)))
                            });
                        if let Err(e) = tracked {
                            let _ = tx.send(Err(e));
                        }
                        Ok(())
                    })
                }));
            monitors.push(task);
        }
        drop(tx);
        // Cancels the servers' tasks if this future is dropped before it has ended them itself, below.
        let monitors = JoiningTasks(monitors);
        // U211 review: a panic here is this monitor's error, so that the servers' tasks are still ended and joined below,
        // rather than left tracking (and, once the client is closed, reconnecting) for the rest of the process.
        let result = std::panic::AssertUnwindSafe(async {
            let mut retiring: HashMap<_, Vec<Arc<dyn VendorTask>>> = HashMap::new();
            let mut cleanup_limits = HashMap::new();
            loop {
                let event = cancellable(context.cancellation.clone(), async { Ok(rx.recv().await) }).await?;
                let Some(event) = event else { return Ok(()) };
                let (client, updates, retried_prepare, replaced, continued) = match event? {
                    Tracked::Devices(client, devices) => {
                        let updates = self.inner.trackers.update(&client, &devices);
                        (client, updates, false, None, None)
                    }
                    Tracked::Replace(client, device, reason, replaced) => {
                        let serial = device.adb_serial().to_owned();
                        let tracker = self.inner.trackers.tracker(&client);
                        let key = (server_name(&client), serial.clone());
                        let retried = lock(&self.inner.devices)
                            .get(&key)
                            .filter(|p| p.device.is_same(&device))
                            .map(|p| p.retried_prepare);
                        let Some(retried) = retried else {
                            if let Some(replaced) = replaced {
                                let _ = replaced.send(ReplacementAck::Superseded);
                            }
                            continue;
                        };
                        // Never with an acknowledgement to drop (x86, item 6): a recovery's replacement is sent by its
                        // server's tracking task, which then sends nothing until it has the answer, and a recovery whose
                        // device a list showed as anything but `device` was cancelled there and sends none, so the
                        // state read here is from a list that showed it as `device`.
                        if tracker.get_state(&serial) != Some(DeviceState::Device) {
                            continue;
                        }
                        // D87: D85's one more setup continues the exhausted preparation's deadline. Its first attempt
                        // always runs, so a device that rebooted still gets ready; it gets no retries past the deadline,
                        // so D85 cannot double the time a device that keeps failing holds its place.
                        let continued = match reason {
                            Replacement::PrepareFailed => device.preparation_deadline(),
                            _ => None,
                        };
                        let retried_prepare = match reason {
                            Replacement::PrepareFailed if !retried && self.can_retry_prepare(&device, &context) => {
                                self.log(
                                    Level::WARN,
                                    &format!(
                                        "Preparing device {serial} exhausted its retries. Setting it up once more"
                                    ),
                                    None,
                                );
                                true
                            }
                            Replacement::PrepareFailed => continue,
                            Replacement::Unready | Replacement::InterruptedSetup => retried,
                            Replacement::SetupLost => false,
                        };
                        (
                            client,
                            vec![
                                (serial.clone(), TrackingUpdate::Disconnected),
                                (serial, TrackingUpdate::Connected),
                            ],
                            retried_prepare,
                            replaced,
                            continued,
                        )
                    }
                    Tracked::Handled(done) => {
                        let _ = done.send(());
                        continue;
                    }
                };
                let server = server_name(&client);
                for (serial, update) in updates {
                    match update {
                        TrackingUpdate::Connected => {
                            let predecessors = retiring
                                .get(&(server.clone(), serial.clone()))
                                .cloned()
                                .unwrap_or_default();
                            let cleanup_limit = cleanup_limits
                                .entry((client.host(), client.port()))
                                .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(RECOVERIES_PER_SERVER)));
                            let device = AndroidDevice::with_remote_cleanup_limit(
                                client.clone(),
                                serial.clone(),
                                self.inner.trackers.tracker(&client),
                                self.inner.runtime.clone(),
                                Some(cleanup_limit.clone()),
                            );
                            if let Some(deadline) = continued {
                                device.continue_preparation(deadline);
                            }
                            let start = context
                                .hooks
                                .instant("core.Track.trackProviderDevicePreparing")
                                .map_err(|e| other(FailureKind::ReplayClock, e.message))?;
                            let reference = device.reference();
                            {
                                let mut devices = lock(&self.inner.all_devices);
                                if EventChannel::state(self.inner.channel.as_ref()).closed_for_send {
                                    return Err(VendorError::Cancelled(requested("device provider is closed")));
                                }
                                devices.push((reference.clone(), device.clone()));
                            }
                            let connected_reference = reference.clone();
                            let ready = Arc::new(Mutex::new(false));
                            let completed = ready.clone();
                            let connected = device.clone();
                            let channel = self.inner.channel.clone();
                            let c = context.clone();
                            let barriers = self.inner.runtime.dependencies.barriers.clone();
                            let key = serial.clone();
                            let task = {
                                let mut setups = lock(&self.inner.setups);
                                if EventChannel::state(self.inner.channel.as_ref()).closed_for_send {
                                    return Err(VendorError::Cancelled(requested("device provider is closed")));
                                }
                                let task =
                                    self.inner
                                        .runtime
                                        .dependencies
                                        .io
                                        .spawn_cancellable_interruptible(Box::new(move |signal| {
                                            Box::pin(async move {
                                                for task in predecessors {
                                                    cancellable(signal.clone(), async {
                                                        let _ = task.join().await;
                                                        Ok(())
                                                    })
                                                    .await?;
                                                }
                                                let permit = cancellable(signal.clone(), async {
                                                    barriers
                                                        .enter("android.AdamDeviceProvider.connected", &key)
                                                        .await
                                                        .map_err(|e| other(FailureKind::Replay, e.message))
                                                })
                                                .await?;
                                                let result = async {
                                                    connected
                                                        .setup(VendorContext {
                                                            cancellation: signal,
                                                            ..c
                                                        })
                                                        .await?;
                                                    let mut ready = lock(&completed);
                                                    connected.ensure_running()?;
                                                    channel
                                                        .try_send(DeviceEvent::Connected(connected_reference))
                                                        .map_err(|_| {
                                                            other(FailureKind::ChannelSendClosed, "Channel was closed")
                                                        })?;
                                                    *ready = true;
                                                    Ok(())
                                                }
                                                .await;
                                                permit.finish().map_err(|e| other(FailureKind::Replay, e.message))?;
                                                if let Err(e) = &result
                                                    && !matches!(e, VendorError::Cancelled(_))
                                                {
                                                    connected.log(
                                                        Level::ERROR,
                                                        "Task stopped due to unrecoverable error",
                                                        Some(e.failure()),
                                                    );
                                                }
                                                result
                                            })
                                        }));
                                setups.push(task.clone());
                                task
                            };
                            lock(&self.inner.devices).insert(
                                (server.clone(), serial.clone()),
                                Provided {
                                    device: device.clone(),
                                    reference,
                                    task,
                                    ready,
                                    retried_prepare,
                                },
                            );
                            let finish = context
                                .hooks
                                .instant("core.Track.trackProviderDevicePreparing")
                                .map_err(|e| other(FailureKind::ReplayClock, e.message))?;
                            // U211 (D51): analytics never stop device tracking; only the monitor's own cancellation
                            // ends it here.
                            if let Err(error) = self
                                .inner
                                .runtime
                                .dependencies
                                .tracker
                                .device_provider_init(&device.serial_number(), start, finish)
                                .await
                            {
                                if let Some(reason) = context.cancellation.reason() {
                                    return Err(VendorError::Cancelled(reason));
                                }
                                self.log(
                                    Level::WARN,
                                    &format!("Tracking the setup of device {serial} failed; device tracking goes on"),
                                    Some(error.failure()),
                                );
                            }
                        }
                        TrackingUpdate::Disconnected => {
                            // D24: dropped from the map, so borrow cannot hand it out once disposed (N27).
                            let provided = lock(&self.inner.devices).remove(&(server.clone(), serial.clone()));
                            if let Some(Provided {
                                device,
                                reference,
                                task,
                                ready,
                                ..
                            }) = provided
                            {
                                device.mark_disconnected();
                                // D48: an emptied pool's wait goes on from what the outage or grace has already used.
                                if let Some(since) = self.inner.trackers.tracker(&client).away_since(&serial) {
                                    device.set_return_wait_used(context.clock.now_millis().saturating_sub(since));
                                }
                                let was_ready = {
                                    let ready = lock(&ready);
                                    device.seal();
                                    *ready
                                };
                                if !was_ready {
                                    task.cancel(requested("device disconnected"));
                                }
                                self.inner
                                    .channel
                                    .try_send(DeviceEvent::Disconnected(reference))
                                    .map_err(|_| other(FailureKind::ChannelSendClosed, "Channel was closed"))?;
                                // D41 releases the old batch's gate after this tracked update is handled.
                                // Keep monitoring while it ends. A setup for the same serial waits for the
                                // old cleanup before it uses that serial's remote state.
                                let pending = retiring.entry((server.clone(), serial.clone())).or_default();
                                pending.retain(|task| task.join().now_or_never().is_none());
                                pending.push(device.start_cleanup(true));
                            }
                        }
                        TrackingUpdate::NothingToDo => {}
                    }
                    if update != TrackingUpdate::NothingToDo {
                        self.log(
                            Level::DEBUG,
                            &format!("Device {serial} changed state to {}", update.name()),
                            None,
                        )
                    }
                }
                if let Some(replaced) = replaced {
                    let _ = replaced.send(ReplacementAck::Applied);
                }
            }
        })
        .catch_unwind()
        .await
        .unwrap_or_else(|payload| Err(VendorError::Other(marathon_core::executor::panic_failure(payload))));
        // D88 (review round 2): published before the servers' tasks are joined, which can take a while (a reconnect
        // reaping its process); a normal finish that cancels this monitor and closes the channel meanwhile must not lose it.
        if let Err(error) = &result
            && context.cancellation.reason().is_none()
        {
            self.publish_failure(error.failure().clone());
        }
        for task in &monitors.0 {
            task.cancel(requested("device monitor ended"));
        }
        monitors.join().await;
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
        let mut recoveries = Recoveries::new(self.inner.runtime.dependencies.io.clone(), context.clone());
        let result = self.follow(client, tx, context, &mut recoveries).await;
        recoveries.shutdown().await;
        result
    }
    async fn follow(
        &self,
        client: AndroidDebugBridgeClient,
        tx: &TrackedSender,
        context: VendorContext,
        recoveries: &mut Recoveries,
    ) -> VendorResult<()> {
        let tracker = self.inner.trackers.tracker(&client);
        let wait = self.inner.runtime.configuration.wait_for_devices_timeout_millis;
        let name = server_name(&client);
        let clock = context.clock.clone();
        let mut stream = client
            .execute_channel(AsyncDeviceMonitorRequest, None)
            .map_err(adb_error)?;
        let mut settling: Option<Settling<'_>> = None;
        let mut blips = Blips {
            clock: clock.as_ref(),
            cancellation: context.cancellation.clone(),
            tracker: tracker.clone(),
            open: vec![],
            latest: vec![],
        };
        loop {
            let step = cancellable(context.cancellation.clone(), async {
                tokio::select! {
                    next = stream.next() => Ok(TrackStep::Next(next)),
                    elapsed = settled(&mut settling) => elapsed.map(|()| TrackStep::Settled),
                    over = blips.next_over() => over.map(TrackStep::BlipOver),
                    done = recoveries.next() => Ok(TrackStep::Recovered(done)),
                    () = tracker.prepare_failed.notified(), if settling.is_none() => Ok(TrackStep::PrepareFailed),
                }
            })
            .await?;
            let cause = match step {
                TrackStep::Next(Some(Ok(devices))) => {
                    settling = self
                        .on_device_list(&client, devices, settling, &mut blips, recoveries, tx, &context)
                        .await?;
                    continue;
                }
                TrackStep::Settled => {
                    if let Some(window) = settling.take() {
                        self.finish_outage(
                            &client,
                            window.kept,
                            window.latest,
                            &mut blips,
                            recoveries,
                            tx,
                            &context,
                        )
                        .await?;
                    }
                    continue;
                }
                TrackStep::Recovered(done) => {
                    self.recovered(&client, done, recoveries, tx, &context).await?;
                    continue;
                }
                TrackStep::PrepareFailed => {
                    self.retry_failed_prepares(&client, &tracker, tx, &context).await?;
                    continue;
                }
                TrackStep::BlipOver(serial) => {
                    self.log(
                        Level::WARN,
                        &format!("Device {serial} did not return in time; disconnecting it"),
                        None,
                    );
                    blips.close(&serial);
                    send(tx, Tracked::Devices(client.clone(), blips.shown()))?;
                    // Only after the disconnect: a batch released sooner fails on the missing device.
                    handled(tx, context.cancellation.clone()).await?;
                    tracker.release(&serial);
                    continue;
                }
                TrackStep::Next(Some(Err(e))) => Some(adb_error(e)),
                TrackStep::Next(None) => None,
            };
            // N19: a closed client is the end of the run, not an outage.
            if client.socket_factory().is_closed() {
                return Ok(());
            }
            let (message, diagnostic) = tracking_warning(&name, cause.as_ref());
            self.log(Level::WARN, &message, diagnostic);
            tracker.server_lost(clock.now_millis());
            // D41: the outage's own bound now covers a device in its grace period, and the server gate holds its batches.
            blips.abandon();
            // U170: and its devices are recovered again once it is over.
            recoveries.abandon();
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
                    self.log(Level::INFO, &format!("adb server {name} is back"), None);
                    settling = Some(Settling {
                        kept,
                        latest: vec![],
                        timer: clock.delay(wait, context.cancellation.clone()),
                    });
                    recovered
                }
                None => {
                    self.log(
                        Level::ERROR,
                        &format!("adb server {name} did not return in {wait} ms; disconnecting its devices"),
                        None,
                    );
                    send(tx, Tracked::Devices(client.clone(), vec![]))?;
                    // Only after the disconnects: a batch released sooner fails on the dead server, again and again.
                    handled(tx, context.cancellation.clone()).await?;
                    tracker.server_back();
                    settling = None;
                    self.reconnect(&client, context.clone()).await?
                }
            };
            stream = new_stream;
            settling = self
                .on_device_list(&client, devices, settling, &mut blips, recoveries, tx, &context)
                .await?;
        }
    }
    fn can_retry_prepare(&self, device: &AndroidDevice, context: &VendorContext) -> bool {
        let limit = self
            .inner
            .runtime
            .dependencies
            .configuration
            .device_initialization_timeout_millis;
        device
            .prepare_failed_at()
            .is_some_and(|at| context.clock.now_millis().saturating_sub(at) < limit)
    }
    /// A failed prepare is a pool event, so adb may never send another list. Coalesced wakes are safe because
    /// the failure belongs to the device instance, and the event loop verifies that instance before replacing it.
    async fn retry_failed_prepares(
        &self,
        client: &AndroidDebugBridgeClient,
        tracker: &DeviceStateTracker,
        tx: &TrackedSender,
        context: &VendorContext,
    ) -> VendorResult<()> {
        handled(tx, context.cancellation.clone()).await?;
        let server = server_name(client);
        let mut sent = false;
        {
            let devices = lock(&self.inner.devices);
            for ((_, serial), p) in devices.iter().filter(|((name, _), _)| *name == server) {
                if !p.retried_prepare
                    && self.can_retry_prepare(&p.device, context)
                    && tracker.get_state(serial) == Some(DeviceState::Device)
                    && !tracker.is_held(serial)
                    && !tracker.is_restoring(serial)
                {
                    send(
                        tx,
                        Tracked::Replace(client.clone(), p.device.clone(), Replacement::PrepareFailed, None),
                    )?;
                    sent = true;
                }
            }
        }
        // Let the replacements finish before another list can hold any of these serials.
        if sent {
            handled(tx, context.cancellation.clone()).await?;
        }
        Ok(())
    }
    /// A list from the stream. While the window after a reconnect is open, a device that was there before the outage
    /// counts as present: a restarted server lists it late, often `offline` first.
    #[allow(clippy::too_many_arguments)]
    async fn on_device_list<'a>(
        &self,
        client: &AndroidDebugBridgeClient,
        devices: Vec<AdbDevice>,
        settling: Option<Settling<'a>>,
        blips: &mut Blips<'a>,
        recoveries: &mut Recoveries,
        tx: &TrackedSender,
        context: &VendorContext,
    ) -> VendorResult<Option<Settling<'a>>> {
        let Some(mut window) = settling else {
            self.offer(client, devices, &[], blips, recoveries, tx, context).await?;
            return Ok(None);
        };
        let listed = |serial: &str| {
            devices
                .iter()
                .any(|d| d.serial == *serial && d.state == DeviceState::Device)
        };
        let missing: Vec<String> = window.kept.iter().filter(|s| !listed(s)).cloned().collect();
        if missing.is_empty() {
            self.finish_outage(client, window.kept, devices, blips, recoveries, tx, context)
                .await?;
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
        self.offer(client, held, &[], blips, recoveries, tx, context).await?;
        window.latest = devices;
        Ok(Some(window))
    }
    /// D41: a list on its way to the loop that owns the device events. A device with an actor that adb reports as passing
    /// through `offline` stays `device` in what is sent on, for a limited time, so that it keeps its actor: a pool ends
    /// with its last device. A real blip reads `offline`, an empty list for up to a second, `offline`, `device`, and an
    /// emulator that was killed reads `offline`, an empty list, and nothing more, so a device missing from the list during
    /// its grace has a shorter limit. Devices in `exempt` have had the outage's window and get no grace on top of it.
    ///
    /// A device that is back is probed and restored outside this loop (U170). A device whose recovery is under way and that
    /// the list no longer shows as `device` has moved on: its recovery is cancelled and decides nothing.
    ///
    /// A list with nothing passing through, with no grace open and no recovery under way, is sent as it is, with no
    /// request and no wait.
    #[allow(clippy::too_many_arguments)]
    async fn offer<'a>(
        &self,
        client: &AndroidDebugBridgeClient,
        devices: Vec<AdbDevice>,
        exempt: &[String],
        blips: &mut Blips<'a>,
        recoveries: &mut Recoveries,
        tx: &TrackedSender,
        context: &VendorContext,
    ) -> VendorResult<()> {
        if blips.open.is_empty() && recoveries.ledger.is_empty() && !devices.iter().any(|d| is_passing(d.state)) {
            return send(tx, Tracked::Devices(client.clone(), devices));
        }
        // What this decides by must include all that was sent before.
        handled(tx, context.cancellation.clone()).await?;
        let tracker = self.inner.trackers.tracker(client);
        let server = server_name(client);
        let provided = |serial: &str| {
            lock(&self.inner.devices)
                .get(&(server.clone(), serial.to_owned()))
                .filter(|p| *lock(&p.ready))
                .map(|p| p.device.clone())
        };
        let (returned, gone) = blips.update(&devices);
        let listed = |serial: &str| {
            devices
                .iter()
                .any(|d| d.serial == serial && d.state == DeviceState::Device)
        };
        let superseded: Vec<(String, Recovery)> = recoveries
            .ledger
            .pending()
            .into_iter()
            .filter(|(serial, _)| !listed(serial))
            .filter_map(|(serial, _)| recoveries.obsolete(&serial).map(|kind| (serial, kind)))
            .collect();
        for device in devices.iter().filter(|d| is_passing(d.state)) {
            let serial = &device.serial;
            // A device still in setup goes through disconnect and connect as ever: its setup has failed by now.
            if blips.is_open(serial)
                || returned.contains(serial)
                || exempt.contains(serial)
                || tracker.get_state(serial) != Some(DeviceState::Device)
                || provided(serial).is_none()
            {
                continue;
            }
            if tracker.blips(serial) >= BLIPS_PER_DEVICE {
                self.log(
                    Level::WARN,
                    &format!("Device {serial} went offline too often; disconnecting it"),
                    None,
                );
                continue;
            }
            self.log(
                Level::WARN,
                &format!(
                    "Device {serial} is in state {:?}; waiting up to {BLIP_GRACE_MILLIS} ms for it to return",
                    device.state
                ),
                None,
            );
            tracker.hold(serial, context.clock.now_millis());
            blips.open(serial);
        }
        blips.latest = devices;
        send(tx, Tracked::Devices(client.clone(), blips.shown()))?;
        if returned.is_empty() && gone.is_empty() && superseded.is_empty() {
            return Ok(());
        }
        handled(tx, context.cancellation.clone()).await?;
        // A returned device stays held until its recovery is over.
        for serial in &returned {
            match provided(serial) {
                Some(device) => recoveries.request(serial, Recovery::Blip, device),
                None => tracker.release(serial),
            }
        }
        for serial in &gone {
            tracker.release(serial);
        }
        // Only after the disconnects: a batch released sooner fails on the missing device. A device that went back into a
        // grace keeps that grace's hold.
        for (serial, kind) in superseded {
            match kind {
                Recovery::Blip if blips.is_open(&serial) => {}
                Recovery::Blip => tracker.release(&serial),
                Recovery::Outage => tracker.restored(&serial),
            }
        }
        Ok(())
    }
    /// U170: a recovery has ended. It counts only if it is still the current one of a device that is still provided:
    /// anything that happened to the device since has cancelled it, and decides instead.
    async fn recovered(
        &self,
        client: &AndroidDebugBridgeClient,
        done: Done,
        recoveries: &mut Recoveries,
        tx: &TrackedSender,
        context: &VendorContext,
    ) -> VendorResult<()> {
        let Some((kind, device)) = recoveries.finish(&done) else {
            return Ok(());
        };
        let serial = done.serial;
        let current = lock(&self.inner.devices)
            .get(&(server_name(client), serial.clone()))
            .is_some_and(|p| *lock(&p.ready) && p.device.is_same(&device));
        if !current {
            return Ok(());
        }
        let tracker = self.inner.trackers.tracker(client);
        // A recovery fails only by a cancellation, which a current one sees only as the loop ends, or a panic. After a
        // panic use the same fallback as an unanswered probe; ending the loop would end the server's tracking.
        let outcome = match done.outcome {
            Ok(outcome) => outcome,
            Err(e @ VendorError::Cancelled(_)) => return Err(e),
            Err(e) => {
                self.log(
                    Level::WARN,
                    &format!("Recovering device {serial} failed"),
                    Some(e.failure()),
                );
                match kind {
                    Recovery::Blip => Recovered::Replace(Replacement::Unready),
                    Recovery::Outage => Recovered::Resumed,
                }
            }
        };
        match outcome {
            Recovered::Resumed => {
                if kind == Recovery::Blip {
                    self.log(Level::INFO, &format!("Device {serial} is back"), None);
                }
                kind.release_wait(&tracker, &serial);
            }
            Recovered::Replace(reason) => {
                // Only the event owner can confirm that the old instance has been marked lost,
                // either by this replacement or by the event that superseded it.
                let (replaced, confirmation) = tokio::sync::oneshot::channel();
                send(
                    tx,
                    Tracked::Replace(client.clone(), device.clone(), reason, Some(replaced)),
                )?;
                let ack = cancellable(context.cancellation.clone(), async { Ok(confirmation.await.ok()) }).await?;
                if !kind.finish_replacement(&tracker, &serial, ack) {
                    return Ok(());
                }
                let why = if reason == Replacement::SetupLost {
                    "lost its setup"
                } else {
                    "returned as if restarted"
                };
                self.log(
                    Level::WARN,
                    &format!("Device {serial} {why}; setting it up again"),
                    None,
                );
            }
        }
        if device.prepare_failed_at().is_some() {
            tracker.prepare_failed.notify_one();
        }
        Ok(())
    }
    /// The real list decides now: a device not on it is disconnected the normal way. A device whose setup the outage
    /// interrupted never had an actor, so it goes through the connect path again. The others keep theirs, with no event,
    /// so no serial ever has two actors.
    ///
    /// U170: the devices kept are restored outside this loop, each kept waiting until its own restoration is over.
    #[allow(clippy::too_many_arguments)]
    async fn finish_outage(
        &self,
        client: &AndroidDebugBridgeClient,
        kept: Vec<String>,
        devices: Vec<AdbDevice>,
        blips: &mut Blips<'_>,
        recoveries: &mut Recoveries,
        tx: &TrackedSender,
        context: &VendorContext,
    ) -> VendorResult<()> {
        handled(tx, context.cancellation.clone()).await?;
        let server = server_name(client);
        let (interrupted, resumed) = {
            let provided = lock(&self.inner.devices);
            let mine = || provided.iter().filter(|((s, _), _)| *s == server);
            let interrupted: Vec<AndroidDevice> = mine()
                .filter(|((_, serial), p)| kept.contains(serial) && !*lock(&p.ready))
                .map(|(_, p)| p.device.clone())
                .collect();
            let resumed: Vec<AndroidDevice> = mine()
                .filter(|(_, p)| *lock(&p.ready))
                .map(|(_, p)| p.device.clone())
                .collect();
            (interrupted, resumed)
        };
        self.offer(client, devices, &kept, blips, recoveries, tx, context)
            .await?;
        for device in interrupted {
            send(
                tx,
                Tracked::Replace(client.clone(), device, Replacement::InterruptedSetup, None),
            )?;
        }
        handled(tx, context.cancellation.clone()).await?;
        let restoring: Vec<AndroidDevice> = resumed.into_iter().filter(|d| d.info().healthy).collect();
        let serials: Vec<String> = restoring.iter().map(|d| d.adb_serial().to_owned()).collect();
        self.inner.trackers.tracker(client).server_back_restoring(&serials);
        for device in restoring {
            let serial = device.adb_serial().to_owned();
            recoveries.request(&serial, Recovery::Outage, device);
        }
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
        let runtime = &self.inner.runtime;
        let clock = &runtime.dependencies.clock;
        let allowance =
            crate::cleanup::allowance(&runtime.configuration.timeout_configuration).saturating_add(STOP_MARGIN_MILLIS);
        let deadline = clock.now_millis().saturating_add(allowance);
        let signal = &self.inner.signal_finish;
        let ended = || signal.wait(clock.as_ref(), deadline, |finish| finish.devices);
        let left = || {
            signal
                .limit(deadline, |finish| finish.devices)
                .saturating_sub(clock.now_millis())
        };
        let provider = {
            let mut monitor = lock(&self.inner.provider);
            let provider = monitor.take();
            if let Some(task) = &provider {
                task.cancel(requested("provider terminated"));
            }
            self.inner.channel.close(None);
            provider
        };
        let failed = self.published_failure().is_some();
        let devices = lock(&self.inner.all_devices).clone();
        for (_, device) in &devices {
            device.seal();
        }
        for task in &*lock(&self.inner.setups) {
            task.cancel(requested("provider terminated"));
        }
        let devices_done = async {
            futures_util::future::join_all(devices.iter().map(|(_, device)| device.dispose_inner(failed))).await;
        };
        let provider_done = async {
            if let Some(task) = provider {
                JoiningTasks(vec![task]).join().await;
            }
            // Registration closes with the channel. A registered device's setup could have been spawned
            // after the first scan of the setup handles, so join after the monitor ends.
            let setups = lock(&self.inner.setups).clone();
            for task in &setups {
                task.cancel(requested("provider terminated"));
            }
            JoiningTasks(setups).join().await;
        };
        let drained = tokio::select! { biased;
            _ = async { tokio::join!(devices_done, provider_done); } => true,
            _ = ended() => false,
        };
        self.inner.factory.close();
        let staging_stop = Signal::new();
        runtime.pull_staging.seal();
        let staged = if left() > 0 {
            tokio::select! { biased;
                result = runtime.pull_staging.close(runtime, staging_stop.clone()) => Some(result),
                _ = ended() => None,
            }
        } else {
            None
        };
        let staging_expired = staged.is_none();
        match staged {
            Some(Err(error)) => self.log(
                Level::WARN,
                "Unable to remove Android pull staging",
                Some(error.failure()),
            ),
            None => {
                staging_stop.cancel(requested("Android provider shutdown deadline reached"));
            }
            _ => {}
        }
        if !drained || staging_expired {
            for task in &*lock(&self.inner.setups) {
                task.interrupt(requested("Android provider shutdown deadline reached"));
            }
            let abandoned = if !drained {
                "device cleanup, monitor or setup work and staging removal"
            } else {
                "staging removal"
            };
            self.log(
                Level::WARN,
                &format!("Android provider shutdown timed out. Abandoning {abandoned}."),
                None,
            );
        }
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
        Box::pin(async move { Ok(self.borrow_untried(Vec::new(), context).await?.0) })
    }
    fn borrow_untried(&self, tried: Vec<String>, context: VendorContext) -> VendorFuture<'_, (DeviceRef, String)> {
        Box::pin(async move {
            loop {
                let available: Vec<_> = lock(&self.inner.devices)
                    .iter()
                    // D41: a device in its grace period cannot take work now. Never true in a healthy run.
                    .filter(|(_, p)| *lock(&p.ready) && !p.device.is_held() && !p.device.stopping())
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
                            // D53: a device is the same one for remote parsing while its server and serial are, as for D18.
                            let identity: String = format!("{serial}@{server}").as_str().into();
                            (key, identity, reference.clone())
                        })
                        .collect();
                    let untried: Vec<_> = available
                        .iter()
                        .filter(|(_, id, _)| !tried.contains(id))
                        .cloned()
                        .collect();
                    let available = if untried.is_empty() { available } else { untried };
                    let keys = available
                        .iter()
                        .map(|(s, _, _)| s.as_str().to_owned())
                        .collect::<Vec<_>>();
                    let selected = context
                        .hooks
                        .choose_key("android.AdamDeviceProvider.borrow", &keys)
                        .map_err(|e| other(FailureKind::ReplayClock, e.message))?;
                    if let Some((_, identity, device)) = available.into_iter().find(|(s, _, _)| selected == s.as_str())
                    {
                        return Ok((device, identity));
                    }
                }
                // D88: a borrow that ends without a device (its caller's deadline cancels it) ends with the failure device
                // monitoring has published by then, if any, as iOS's does (D84).
                if let Err(error) = context.clock.delay(200, context.cancellation.clone()).await {
                    return Err(self.published_failure().map_or(error, VendorError::Other));
                }
            }
        })
    }
    fn seal_for_signal(&self, finish: marathon_core::signal_finish::SignalFinish) {
        self.inner.signal_finish.set(finish);
        self.inner.channel.close(None);
        if let Some(monitor) = lock(&self.inner.provider).as_ref() {
            monitor.cancel(requested("provider terminated"));
        }
        for setup in &*lock(&self.inner.setups) {
            setup.cancel(requested("provider terminated"));
        }
        for (_, device) in &*lock(&self.inner.all_devices) {
            device.signal_seal(finish);
        }
    }
    fn terminate(&self) -> VendorFuture<'_, ()> {
        let this = self.clone();
        Box::pin(call_vendor(
            self.inner.runtime.dependencies.io.as_ref(),
            Signal::new(),
            move |_| Box::pin(async move { this.terminate_inner().await }),
        ))
    }
    fn published_failure(&self) -> Option<Failure> {
        lock(&self.inner.monitor_failure).clone()
    }
    fn subscribe(&self) -> Arc<dyn EventChannel<DeviceEvent>> {
        self.inner.channel.clone()
    }
}
/// D22: the wait between attempts to reach a lost adb server.
const RECONNECT_MIN_MILLIS: i64 = 1000;
const RECONNECT_MAX_MILLIS: i64 = 8000;
/// D41: how long a device in a passing state keeps its actor. An adbd restart takes about 1.2 s on a real emulator, and
/// adb retries a lost TCP device after 250 ms and then every 3 s, so this covers four attempts. See `docs/deviations.md`.
const BLIP_GRACE_MILLIS: i64 = 10_000;
/// D41: how long such a device may be missing from the list. The emulator's transport is scanned for once a second: the
/// empty list lasted up to 1 s in every blip measured, and a killed emulator does not come back.
const BLIP_MISSING_MILLIS: i64 = 3000;
/// D41: grace periods per device per run, so that flapping cannot reset the grace for ever. The next `offline` disconnects
/// the device, as it always did.
const BLIPS_PER_DEVICE: u32 = 5;
type TrackedSender = tokio::sync::mpsc::UnboundedSender<VendorResult<Tracked>>;
/// What the per-server tracking tasks send to the one loop that owns the device events.
enum Tracked {
    Devices(AndroidDebugBridgeClient, Vec<AdbDevice>),
    Replace(
        AndroidDebugBridgeClient,
        AndroidDevice,
        Replacement,
        Option<tokio::sync::oneshot::Sender<ReplacementAck>>,
    ),
    /// Answered once everything sent before it has been handled.
    Handled(tokio::sync::oneshot::Sender<()>),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Replacement {
    PrepareFailed,
    InterruptedSetup,
    Unready,
    SetupLost,
}
#[derive(Clone, Copy)]
enum ReplacementAck {
    Applied,
    Superseded,
}
enum TrackStep {
    Next(Option<Result<Vec<AdbDevice>, AdbError>>),
    Settled,
    BlipOver(String),
    Recovered(Done),
    PrepareFailed,
}
/// The window after a reconnect, in which devices from before the outage may still appear.
struct Settling<'a> {
    kept: Vec<String>,
    latest: Vec<AdbDevice>,
    timer: VendorFuture<'a, ()>,
}
/// Waits for the end of the window; for ever if none is open.
async fn settled(settling: &mut Option<Settling<'_>>) -> VendorResult<()> {
    match settling {
        Some(window) => (&mut window.timer).await,
        None => std::future::pending().await,
    }
}
/// D41: one device's grace period.
struct Blip<'a> {
    serial: String,
    limit: VendorFuture<'a, ()>,
    /// Running while the device is missing from the list.
    missing: Option<VendorFuture<'a, ()>>,
}
/// D41: the grace periods open on one adb server, so one server's never reach another's.
struct Blips<'a> {
    clock: &'a dyn VendorClock,
    cancellation: Arc<dyn CancellationSignal>,
    tracker: Arc<DeviceStateTracker>,
    open: Vec<Blip<'a>>,
    latest: Vec<AdbDevice>,
}
impl<'a> Blips<'a> {
    fn is_open(&self, serial: &str) -> bool {
        self.open.iter().any(|blip| blip.serial == serial)
    }
    fn open(&mut self, serial: &str) {
        self.open.push(Blip {
            serial: serial.to_owned(),
            limit: self.clock.delay(BLIP_GRACE_MILLIS, self.cancellation.clone()),
            missing: None,
        });
    }
    fn close(&mut self, serial: &str) {
        self.open.retain(|blip| blip.serial != serial);
    }
    /// An outage takes over: its own window decides about these devices.
    fn abandon(&mut self) {
        self.open.clear();
        self.tracker.release_all();
    }
    /// Reads a new list: the serials that are `device` again, and those in a state that is not passing.
    fn update(&mut self, devices: &[AdbDevice]) -> (Vec<String>, Vec<String>) {
        let (mut returned, mut gone) = (vec![], vec![]);
        for blip in std::mem::take(&mut self.open) {
            match devices.iter().find(|d| d.serial == blip.serial).map(|d| d.state) {
                Some(DeviceState::Device) => returned.push(blip.serial),
                Some(state) if is_passing(state) => self.open.push(Blip { missing: None, ..blip }),
                Some(_) => gone.push(blip.serial),
                None => {
                    let missing = blip
                        .missing
                        .unwrap_or_else(|| self.clock.delay(BLIP_MISSING_MILLIS, self.cancellation.clone()));
                    self.open.push(Blip {
                        missing: Some(missing),
                        ..blip
                    });
                }
            }
        }
        (returned, gone)
    }
    /// The latest list, with the devices in their grace period as `device`.
    fn shown(&self) -> Vec<AdbDevice> {
        self.shown_of(&self.latest)
    }
    fn shown_of(&self, devices: &[AdbDevice]) -> Vec<AdbDevice> {
        let mut shown: Vec<AdbDevice> = devices.iter().filter(|d| !self.is_open(&d.serial)).cloned().collect();
        shown.extend(self.open.iter().map(|blip| AdbDevice {
            serial: blip.serial.clone(),
            state: DeviceState::Device,
        }));
        shown
    }
    /// The next device whose grace is over; for ever if there is none.
    async fn next_over(&mut self) -> VendorResult<String> {
        std::future::poll_fn(|cx| {
            for blip in &mut self.open {
                let limits = std::iter::once(&mut blip.limit).chain(blip.missing.as_mut());
                for limit in limits {
                    if let std::task::Poll::Ready(result) = limit.as_mut().poll(cx) {
                        return std::task::Poll::Ready(result.map(|()| blip.serial.clone()));
                    }
                }
            }
            std::task::Poll::Pending
        })
        .await
    }
}
/// U170: how many devices of one adb server recover at once. A recovery is a `getprop` and the restore of a logcat
/// stream and a few reverse port forwards, where a server carries a logcat and an instrumentation stream for every
/// device all through a run, so 8 is light for it. The bound keeps a server that has just restarted with dozens of
/// devices from getting all of their requests at once, and leaves 7 slots working while one device does not answer.
const RECOVERIES_PER_SERVER: usize = 8;
/// U170: what a device that came back still needs. It runs outside the tracking loop, which meanwhile goes on reading the
/// list and ending other devices' grace periods.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Recovery {
    /// D41: back from its grace period. Whether it restarted, and if not, its logcat and reverse port forwards. Its hold
    /// is released at the end.
    Blip,
    /// D22: still listed when its server came back. Its boot check, logcat and reverse port forwards precede its gate.
    Outage,
}
impl Recovery {
    fn release_wait(self, tracker: &DeviceStateTracker, serial: &str) {
        match self {
            Self::Blip => tracker.release(serial),
            Self::Outage => tracker.restored(serial),
        }
    }

    /// A superseded instance is already lost. Its wait must end, but only an applied replacement is logged.
    fn finish_replacement(self, tracker: &DeviceStateTracker, serial: &str, ack: Option<ReplacementAck>) -> bool {
        let Some(ack) = ack else { return false };
        self.release_wait(tracker, serial);
        matches!(ack, ReplacementAck::Applied)
    }
}
#[derive(Debug, PartialEq, Eq)]
enum Recovered {
    Resumed,
    Replace(Replacement),
}
/// What a recovery task reports, by its id.
struct Done {
    id: u64,
    serial: String,
    outcome: VendorResult<Recovered>,
}
struct Queued<T> {
    id: u64,
    serial: String,
    kind: Recovery,
    payload: T,
}
struct Running<T, H> {
    id: u64,
    serial: String,
    kind: Recovery,
    payload: T,
    handle: H,
}
/// U170: the recoveries of one adb server, apart from the tasks that run them. One per device at most, a newer one
/// replacing the older; at most `limit` running, counting those cancelled and not yet ended; and a completion counts only
/// if it is still the device's current recovery, so a stale one never releases a gate.
struct Ledger<T, H> {
    limit: usize,
    next: u64,
    running: Vec<Running<T, H>>,
    ending: Vec<(u64, H)>,
    queued: std::collections::VecDeque<Queued<T>>,
}
impl<T: Clone, H: Clone> Ledger<T, H> {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            next: 0,
            running: vec![],
            ending: vec![],
            queued: std::collections::VecDeque::new(),
        }
    }
    fn is_empty(&self) -> bool {
        self.running.is_empty() && self.queued.is_empty()
    }
    /// The devices with a recovery queued or running.
    fn pending(&self) -> Vec<(String, Recovery)> {
        let running = self.running.iter().map(|r| (r.serial.clone(), r.kind));
        running
            .chain(self.queued.iter().map(|q| (q.serial.clone(), q.kind)))
            .collect()
    }
    /// Queues a recovery for `serial`, replacing any it has. Returns the task to cancel, if one was running.
    fn request(&mut self, serial: &str, kind: Recovery, payload: T) -> Option<H> {
        let cancelled = self.obsolete(serial).and_then(|(_, handle)| handle);
        self.next += 1;
        self.queued.push_back(Queued {
            id: self.next,
            serial: serial.to_owned(),
            kind,
            payload,
        });
        cancelled
    }
    /// Drops the recovery of `serial`: what it was, and its task to cancel if it was running.
    fn obsolete(&mut self, serial: &str) -> Option<(Recovery, Option<H>)> {
        if let Some(at) = self.queued.iter().position(|q| q.serial == serial) {
            return self.queued.remove(at).map(|q| (q.kind, None));
        }
        let at = self.running.iter().position(|r| r.serial == serial)?;
        let running = self.running.remove(at);
        self.ending.push((running.id, running.handle.clone()));
        Some((running.kind, Some(running.handle)))
    }
    /// Drops them all, as when the server goes away: the tasks to cancel.
    fn abandon(&mut self) -> Vec<H> {
        self.queued.clear();
        std::mem::take(&mut self.running)
            .into_iter()
            .map(|r| {
                self.ending.push((r.id, r.handle.clone()));
                r.handle
            })
            .collect()
    }
    /// The next recovery to start, if a slot is free.
    fn startable(&mut self) -> Option<Queued<T>> {
        if self.running.len() + self.ending.len() >= self.limit {
            return None;
        }
        self.queued.pop_front()
    }
    fn started(&mut self, queued: Queued<T>, handle: H) {
        self.running.push(Running {
            id: queued.id,
            serial: queued.serial,
            kind: queued.kind,
            payload: queued.payload,
            handle,
        });
    }
    /// A task has ended. Its recovery and payload if it is still the current one for its device.
    fn finish(&mut self, id: u64) -> Option<(Recovery, T)> {
        if let Some(at) = self.ending.iter().position(|(ending, _)| *ending == id) {
            self.ending.remove(at);
            return None;
        }
        let at = self.running.iter().position(|r| r.id == id)?;
        let running = self.running.remove(at);
        Some((running.kind, running.payload))
    }
    /// Every task, running or ending, for the shutdown.
    fn take_all(&mut self) -> Vec<H> {
        self.queued.clear();
        let running = self.running.drain(..).map(|r| r.handle);
        let tasks: Vec<H> = running.collect();
        tasks.into_iter().chain(self.ending.drain(..).map(|(_, h)| h)).collect()
    }
}
/// U170: one adb server's recoveries and the tasks that run them.
struct Recoveries {
    ledger: Ledger<AndroidDevice, Arc<dyn VendorTask>>,
    io: Arc<dyn VendorRuntime>,
    context: VendorContext,
    done: tokio::sync::mpsc::UnboundedSender<Done>,
    reports: tokio::sync::mpsc::UnboundedReceiver<Done>,
}
impl Recoveries {
    fn new(io: Arc<dyn VendorRuntime>, context: VendorContext) -> Self {
        let (done, reports) = tokio::sync::mpsc::unbounded_channel();
        Self {
            ledger: Ledger::new(RECOVERIES_PER_SERVER),
            io,
            context,
            done,
            reports,
        }
    }
    fn request(&mut self, serial: &str, kind: Recovery, device: AndroidDevice) {
        if let Some(task) = self.ledger.request(serial, kind, device) {
            task.cancel(requested("device recovery superseded"));
        }
        self.start();
    }
    fn obsolete(&mut self, serial: &str) -> Option<Recovery> {
        let (kind, task) = self.ledger.obsolete(serial)?;
        if let Some(task) = task {
            task.cancel(requested("device recovery superseded"));
        }
        Some(kind)
    }
    fn abandon(&mut self) {
        for task in self.ledger.abandon() {
            task.cancel(requested("adb server lost"));
        }
    }
    fn finish(&mut self, done: &Done) -> Option<(Recovery, AndroidDevice)> {
        let current = self.ledger.finish(done.id);
        self.start();
        current
    }
    /// Starts what the bound allows.
    fn start(&mut self) {
        while let Some(queued) = self.ledger.startable() {
            let (id, serial, kind, device) = (queued.id, queued.serial.clone(), queued.kind, queued.payload.clone());
            let (done, context) = (self.done.clone(), self.context.clone());
            let task = self.io.spawn_cancellable_interruptible(Box::new(move |signal| {
                Box::pin(async move {
                    let context = VendorContext {
                        cancellation: signal,
                        ..context
                    };
                    let recovery = async {
                        match device.returned_boot(&context, kind == Recovery::Blip).await? {
                            ReturnedBoot::SetupLost => return Ok(Recovered::Replace(Replacement::SetupLost)),
                            ReturnedBoot::Unready if kind == Recovery::Blip => {
                                return Ok(Recovered::Replace(Replacement::Unready));
                            }
                            _ => {}
                        }
                        match kind {
                            Recovery::Blip => device.resume_after_blip(context).await?,
                            Recovery::Outage => device.resume_after_server_return(context).await?,
                        }
                        Ok(Recovered::Resumed)
                    };
                    report(id, serial, &done, recovery).await;
                    Ok(())
                })
            }));
            self.ledger.started(queued, task);
        }
    }
    /// The next report; never, while nothing runs.
    async fn next(&mut self) -> Done {
        match self.reports.recv().await {
            Some(done) => done,
            // This holds a sender, so the channel never closes.
            None => std::future::pending().await,
        }
    }
    /// The tracking loop has ended. Each task ends at its next await once cancelled, so the join is short.
    async fn shutdown(&mut self) {
        let tasks = self.ledger.take_all();
        for task in &tasks {
            task.cancel(requested("device monitor ended"));
        }
        JoiningTasks(tasks).join().await;
    }
}
impl Drop for Recoveries {
    fn drop(&mut self) {
        drop(JoiningTasks(self.ledger.take_all()));
    }
}
/// U170: runs a recovery and always reports it. The ledger's slot and the device's hold are freed only by the report, and
/// the runtime's own catch of a panic is seen only by a join, which the loop does at its end; so a panic is reported here,
/// as a failed recovery.
async fn report(
    id: u64,
    serial: String,
    done: &tokio::sync::mpsc::UnboundedSender<Done>,
    recovery: impl std::future::Future<Output = VendorResult<Recovered>>,
) {
    let outcome = match std::panic::AssertUnwindSafe(recovery).catch_unwind().await {
        Ok(outcome) => outcome,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("device recovery panicked");
            Err(VendorError::Other(failure(FailureKind::Runtime, message)))
        }
    };
    let _ = done.send(Done { id, serial, outcome });
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
        FailureKind::Adb(AdbKind::ConnectRefused),
        FailureKind::Adb(AdbKind::ConnectTimeout),
        FailureKind::Connect,
    ]
    .iter()
    .any(|c| e.failure().kind == (*c))
}
fn no_devices(message: &str) -> VendorError {
    VendorError::NoDevices(failure(FailureKind::NoDevices, message))
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
pub async fn start_adb(sdk: Option<&std::path::Path>, port: u16, context: VendorContext) -> VendorResult<bool> {
    let home = sdk.map(std::path::Path::to_path_buf).or_else(|| {
        std::env::var_os("ANDROID_HOME")
            .or_else(|| std::env::var_os("ANDROID_SDK_ROOT"))
            .map(PathBuf::from)
    });
    let binary = if let Some(home) = home {
        home.join("platform-tools/adb")
    } else {
        let result = marathon_process::Command::new("which")
            .arg("adb")
            .output()
            .await
            .map_err(io)?;
        PathBuf::from(
            jcompat::jstring::lines(String::from_utf8_lossy(&result.stdout).into_owned().as_str())
                .first()
                .map(|s| s.to_owned())
                .unwrap_or_default(),
        )
    };
    if !tokio::fs::metadata(&binary).await.is_ok_and(|m| m.is_file()) {
        return Ok(false);
    }
    let binary = std::path::absolute(binary).map_err(io)?;
    let mut child = marathon_process::Command::new(binary)
        .args(["-P", &port.to_string(), "start-server"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn_adb_client()
        .map_err(io)?;
    tokio::select! {
        biased;
        result = child.wait() => result.map(|exit| exit.success()).map_err(io),
        reason = context.cancellation.cancelled() => {
            let _ = child.terminate();
            let grace = std::time::Duration::from_secs(1);
            if tokio::time::timeout(grace, child.wait()).await.is_err() {
                let _ = child.start_kill();
                let _ = tokio::time::timeout(grace, child.wait()).await;
            }
            Err(VendorError::Cancelled(reason))
        }
    }
}

// The empty header is EOF in track-devices, not a rejection sent by the server.
fn tracking_warning<'a>(name: &str, cause: Option<&'a VendorError>) -> (String, Option<&'a Failure>) {
    let failure = cause.map(VendorError::failure);
    if failure.is_some_and(|failure| {
        failure.kind == FailureKind::Adb(AdbKind::RequestRejected)
            && failure.message.as_deref() == Some("Unexpected string length: ")
    }) {
        (
            format!("Lost device tracking on adb server {name}: the adb server closed the connection; reconnecting"),
            None,
        )
    } else {
        (
            format!("Lost device tracking on adb server {name}; reconnecting"),
            failure,
        )
    }
}

#[cfg(test)]
mod kind_tests {
    use super::*;
    #[test]
    fn tracking_eof_diagnostic_does_not_rewrite_other_errors_or_data_x56() {
        let original = other(FailureKind::Adb(AdbKind::RequestRejected), "Unexpected string length: ");
        let (message, cause) = tracking_warning("local", Some(&original));
        assert_eq!(
            message,
            "Lost device tracking on adb server local: the adb server closed the connection; reconnecting"
        );
        assert!(cause.is_none());
        assert_eq!(
            original.failure().message.as_deref(),
            Some("Unexpected string length: ")
        );
        for error in [
            other(
                FailureKind::Adb(AdbKind::RequestRejected),
                "Unexpected string length: x",
            ),
            other(FailureKind::Io, "Unexpected string length: "),
        ] {
            let (message, cause) = tracking_warning("local", Some(&error));
            assert_eq!(message, "Lost device tracking on adb server local; reconnecting");
            assert_eq!(cause, Some(error.failure()));
        }
    }
    #[test]
    fn an_acknowledged_superseded_replacement_releases_its_wait_without_claiming_setup_d85() {
        let serials = ["old".to_owned(), "other".to_owned()];
        for kind in [Recovery::Blip, Recovery::Outage] {
            let tracker = DeviceStateTracker::default();
            let waiting = |serial| match kind {
                Recovery::Blip => tracker.is_held(serial),
                Recovery::Outage => tracker.is_restoring(serial),
            };
            match kind {
                Recovery::Blip => {
                    tracker.hold("old", 1);
                    tracker.hold("other", 1);
                }
                Recovery::Outage => {
                    tracker.server_lost(1);
                    tracker.server_back_restoring(&serials);
                }
            }
            assert!(!kind.finish_replacement(&tracker, "old", None));
            assert!(waiting("old"));
            assert!(!kind.finish_replacement(&tracker, "old", Some(ReplacementAck::Superseded)));
            assert!(!waiting("old"));
            assert!(waiting("other"));
        }
    }

    /// U170: one recovery per device, a newer one replacing it, and a completion counts only while it is current: one
    /// superseded, dropped or abandoned with its server reports into nothing, so it can release no gate.
    #[test]
    fn a_stale_recovery_completes_into_nothing_u170() {
        let mut ledger: Ledger<(), u64> = Ledger::new(8);
        assert_eq!(ledger.request("a", Recovery::Blip, ()), None);
        let first = ledger.startable().unwrap();
        let first_id = first.id;
        ledger.started(first, first_id);
        // The device went offline and came back again: the running task is to be cancelled, and its report is stale.
        assert_eq!(ledger.request("a", Recovery::Blip, ()), Some(first_id));
        assert_eq!(ledger.pending(), vec![("a".to_owned(), Recovery::Blip)]);
        let second = ledger.startable().unwrap();
        let second_id = second.id;
        ledger.started(second, second_id);
        assert_eq!(ledger.finish(first_id), None);
        assert_eq!(ledger.finish(second_id), Some((Recovery::Blip, ())));
        assert!(ledger.is_empty());
        // The device left the list: dropped, running or queued.
        ledger.request("b", Recovery::Outage, ());
        let b = ledger.startable().unwrap();
        let b_id = b.id;
        ledger.started(b, b_id);
        assert_eq!(ledger.obsolete("b"), Some((Recovery::Outage, Some(b_id))));
        assert_eq!(ledger.finish(b_id), None);
        ledger.request("c", Recovery::Blip, ());
        assert_eq!(ledger.obsolete("c"), Some((Recovery::Blip, None)));
        assert!(ledger.startable().is_none());
        // The server went away: everything is abandoned.
        ledger.request("d", Recovery::Outage, ());
        let d = ledger.startable().unwrap();
        let d_id = d.id;
        ledger.started(d, d_id);
        ledger.request("e", Recovery::Outage, ());
        assert_eq!(ledger.abandon(), vec![d_id]);
        assert!(ledger.is_empty());
        assert_eq!(ledger.finish(d_id), None);
        assert_eq!(ledger.obsolete("d"), None);
    }
    /// U170: at most `limit` tasks at once, those cancelled and not yet ended included, and the rest wait in order.
    #[test]
    fn recoveries_are_bounded_counting_those_still_ending_u170() {
        let mut ledger: Ledger<(), u64> = Ledger::new(RECOVERIES_PER_SERVER);
        for device in 0..20 {
            ledger.request(&format!("emulator-{device}"), Recovery::Outage, ());
        }
        let mut running = vec![];
        while let Some(queued) = ledger.startable() {
            let id = queued.id;
            running.push((queued.serial.clone(), id));
            ledger.started(queued, id);
        }
        assert_eq!(running.len(), RECOVERIES_PER_SERVER);
        assert_eq!(running[0].0, "emulator-0");
        // A cancelled task still holds its slot until it reports.
        let (serial, id) = running.remove(0);
        assert_eq!(ledger.obsolete(&serial), Some((Recovery::Outage, Some(id))));
        assert!(ledger.startable().is_none());
        assert_eq!(ledger.finish(id), None);
        let next = ledger.startable().unwrap();
        assert_eq!(next.serial, format!("emulator-{RECOVERIES_PER_SERVER}"));
        let next_id = next.id;
        ledger.started(next, next_id);
        assert!(ledger.startable().is_none());
        assert_eq!(ledger.finish(next_id), Some((Recovery::Outage, ())));
        assert!(ledger.startable().is_some());
        assert_eq!(ledger.take_all().len(), RECOVERIES_PER_SERVER - 1);
        assert!(ledger.is_empty());
    }
    /// U170 (review M-x64-1): a recovery that panics still reports, so its slot frees and the loop decides about its
    /// device, instead of both waiting for a report that never comes.
    #[tokio::test]
    async fn a_panicking_recovery_still_reports_and_frees_its_slot_u170() {
        let mut ledger: Ledger<(), u64> = Ledger::new(1);
        ledger.request("a", Recovery::Blip, ());
        ledger.request("b", Recovery::Blip, ());
        let a = ledger.startable().unwrap();
        let a_id = a.id;
        ledger.started(a, a_id);
        assert!(ledger.startable().is_none());
        let (done, mut reports) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            report(a_id, "a".to_owned(), &done, async {
                if a_id < u64::MAX {
                    panic!("probe panicked");
                }
                Ok(Recovered::Resumed)
            })
            .await;
        });
        task.await.expect("report must not unwind");
        let report = reports.try_recv().expect("a panicking recovery reports");
        assert_eq!((report.id, report.serial.as_str()), (a_id, "a"));
        let error = report.outcome.expect_err("a panic is a failed recovery");
        assert!(!matches!(error, VendorError::Cancelled(_)));
        assert_eq!(error.failure().message.as_deref(), Some("probe panicked"));
        assert_eq!(ledger.finish(report.id), Some((Recovery::Blip, ())));
        assert_eq!(ledger.startable().map(|b| b.serial), Some("b".to_owned()));
    }
    #[test]
    fn connect_retry_membership_is_the_exact_existing_set() {
        for kind in [
            FailureKind::Adb(AdbKind::ConnectRefused),
            FailureKind::Adb(AdbKind::ConnectTimeout),
            FailureKind::Connect,
        ] {
            assert!(is_connect(&VendorError::Other(failure(kind, "test"))));
        }
        for kind in [
            FailureKind::Socket,
            FailureKind::Io,
            FailureKind::Adb(AdbKind::RequestRejected),
        ] {
            assert!(!is_connect(&VendorError::Other(failure(kind, "test"))));
        }
    }
}
