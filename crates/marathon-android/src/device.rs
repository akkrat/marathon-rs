//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AdamAndroidDevice.kt,
//! BaseAndroidDevice.kt, AndroidDevice.kt and adam/LogcatManager.kt.
use crate::{
    models::*,
    remote_file_manager::RemoteFileManager,
    requests::*,
    services::AndroidRuntime,
    support::*,
    tracker::{DeviceStateTracker, is_passing},
    version::AndroidVersion,
};
use jcompat::logging::{Level, Logger};
use marathon_adb::client::{
    AndroidDebugBridgeClient, AsyncChannelRequest, ChannelStream, ComplexRequest, Feature,
    device::{DeviceState, FetchDeviceFeaturesRequest, ListDevicesRequest},
    reverse::*,
    shell::v1::ShellCommandRequest,
};
use marathon_core::{
    error::*,
    executor::{
        bridge::call_vendor,
        shared::{Signal, requested},
    },
    listener::{LineListener, LogProducer},
    model::*,
    runtime::*,
    screenshot::Rotation,
    vendor::*,
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
};

/// D22: how often `wait_out_passing_loss` looks again until the tracker has heard of a lost server.
const OUTAGE_POLL_MILLIS: i64 = 100;
/// D41: the logcat stream's end and the list reporting the blip come over different sockets, so after a very short blip
/// the stream may not have ended yet. It has in every real case seen, so this only limits the wait.
const LOGCAT_END_WAIT_MILLIS: i64 = 1000;
/// D49: lost requests per serial per adb server per run that a device listed as `device` and answering survives. Each one
/// charges its batch an attempt, so it is a count with no forgiveness, as D41's is. One after this retires the device.
const REQUEST_LOSSES_PER_DEVICE: u32 = 3;
/// D22, D41, D49: why a request failed the way that means a lost device.
enum LossCause {
    /// The server is away, or the transport is passing through a brief `offline`.
    Passing(&'static str),
    /// The server lists it as `device`, so the tracker sees no change and would never connect it again (N15).
    Listed,
    /// No longer on the list, or in another state.
    Unlisted,
}
pub trait AndroidBatchExecutor: Send + Sync {
    fn execute<'a>(
        &'a self,
        device: &'a AndroidDevice,
        configuration: Arc<marathon_config::Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<dyn Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()>;
}
pub struct PendingBatchExecutor;
impl AndroidBatchExecutor for PendingBatchExecutor {
    fn execute<'a>(
        &'a self,
        _device: &'a AndroidDevice,
        _configuration: Arc<marathon_config::Configuration>,
        _pool: DevicePoolId,
        _batch: BatchRef,
        _result: Arc<dyn Deferred<TestBatchResults>>,
        _context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async {
            Err(other(
                "java.lang.UnsupportedOperationException",
                "Android batch execution is supplied by WP 6.6a",
            ))
        })
    }
}
#[derive(Clone)]
pub struct AndroidDevice {
    inner: Arc<Inner>,
}
struct Inner {
    reference: Mutex<Option<Weak<dyn Device>>>,
    client: AndroidDebugBridgeClient,
    /// D21: for requests under a configured timeout or watchdog, or silent by nature.
    bounded_client: AndroidDebugBridgeClient,
    adb_serial: String,
    runtime: Arc<AndroidRuntime>,
    tracker: Arc<DeviceStateTracker>,
    logger: Logger,
    state: Mutex<State>,
    line_listeners: Mutex<Vec<Arc<dyn LineListener>>>,
    logcat_listeners: Mutex<Vec<Arc<dyn crate::logcat_listener::LogcatListener>>>,
    logcat: Mutex<Option<Arc<dyn VendorTask>>>,
    /// D22: the logcat stream has ended, as it does when the adb server goes away.
    logcat_ended: tokio::sync::watch::Sender<bool>,
    reverse: Mutex<Vec<RemotePortSpec>>,
    disposal: tokio::sync::Mutex<bool>,
    /// D24: this device object was disconnected. A reconnect makes a new object on the same tracker, whose
    /// state then says `device` again, so the tracker alone cannot tell.
    lost: AtomicBool,
    /// D48: what an outage or a grace period had waited for it when it was disconnected.
    return_wait_used: AtomicI64,
}
struct State {
    booted: bool,
    serial: String,
    abi: String,
    version: AndroidVersion,
    external: String,
    model: String,
    manufacturer: String,
    rotation: Rotation,
    features: Vec<DeviceFeature>,
    adb_features: Vec<Feature>,
    props: HashMap<String, String>,
    md5: String,
}
impl AndroidDevice {
    pub fn new(
        client: AndroidDebugBridgeClient,
        adb_serial: String,
        tracker: Arc<DeviceStateTracker>,
        runtime: Arc<AndroidRuntime>,
    ) -> Self {
        let logger = runtime.logger("AndroidDevice");
        Self {
            inner: Arc::new(Inner {
                reference: Mutex::default(),
                bounded_client: client.without_idle_limit(),
                client,
                adb_serial,
                runtime,
                tracker,
                logger,
                state: Mutex::new(State {
                    booted: false,
                    serial: "Unknown".into(),
                    abi: "Unknown".into(),
                    version: AndroidVersion::default(),
                    external: "Unknown".into(),
                    model: "Unknown".into(),
                    manufacturer: "Unknown".into(),
                    rotation: Rotation::Rotation0,
                    features: vec![],
                    adb_features: vec![],
                    props: HashMap::new(),
                    md5: String::new(),
                }),
                line_listeners: Mutex::default(),
                logcat_listeners: Mutex::default(),
                logcat: Mutex::default(),
                logcat_ended: tokio::sync::watch::Sender::new(false),
                reverse: Mutex::default(),
                disposal: tokio::sync::Mutex::new(false),
                lost: AtomicBool::new(false),
                return_wait_used: AtomicI64::new(0),
            }),
        }
    }
    /// D24: from now on this object reports disconnected, whatever happens to its serial later.
    pub(crate) fn mark_disconnected(&self) {
        self.inner.lost.store(true, Ordering::Release);
    }
    pub(crate) fn set_return_wait_used(&self, millis: i64) {
        self.inner.return_wait_used.store(millis, Ordering::Release);
    }
    fn is_live(&self) -> bool {
        !self.inner.lost.load(Ordering::Acquire)
            && self.inner.tracker.get_state(self.adb_serial()) == Some(DeviceState::Device)
    }
    /// D41: in its grace period, so listed as `device` to the rest of the run although adb says otherwise.
    pub(crate) fn is_held(&self) -> bool {
        self.inner.tracker.is_held(self.adb_serial())
    }
    /// D41: how many grace periods this serial has had on its adb server in this run.
    pub fn grace_periods(&self) -> u32 {
        self.inner.tracker.blips(self.adb_serial())
    }
    pub fn reference(&self) -> DeviceRef {
        let mut reference = lock(&self.inner.reference);
        if let Some(device) = reference.as_ref().and_then(Weak::upgrade) {
            return DeviceRef(device);
        }
        let device: Arc<dyn Device> = Arc::new(self.clone());
        *reference = Some(Arc::downgrade(&device));
        DeviceRef(device)
    }
    /// In-memory listener ownership snapshot; no adb request or file work.
    pub fn listener_counts(&self) -> (usize, usize) {
        (
            lock(&self.inner.line_listeners).len(),
            lock(&self.inner.logcat_listeners).len(),
        )
    }
    pub fn adb_reverse_port(&self) -> Option<i32> {
        lock(&self.inner.reverse).iter().find_map(|rule| match rule {
            RemotePortSpec::Tcp(port) => Some(*port),
            _ => None,
        })
    }
    pub fn client(&self) -> &AndroidDebugBridgeClient {
        &self.inner.client
    }
    /// D22: false while the adb server is away, when batches wait.
    pub fn adb_server_available(&self) -> bool {
        self.inner.tracker.is_server_available()
    }
    /// D21: the client for work whose caller has a timeout of its own, which the socket's idle limit must not
    /// beat. See the request audit in `docs/wp/x12-findings.md`.
    pub(crate) fn bounded_client(&self) -> &AndroidDebugBridgeClient {
        &self.inner.bounded_client
    }
    pub fn adb_serial(&self) -> &str {
        &self.inner.adb_serial
    }
    pub fn runtime(&self) -> &Arc<AndroidRuntime> {
        &self.inner.runtime
    }
    pub fn api_level(&self) -> i32 {
        lock(&self.inner.state).version.api_level
    }
    pub fn version(&self) -> AndroidVersion {
        lock(&self.inner.state).version.clone()
    }
    pub fn external_storage_mount(&self) -> String {
        lock(&self.inner.state).external.clone()
    }
    pub fn supported_features(&self) -> Vec<Feature> {
        lock(&self.inner.state).adb_features.clone()
    }
    pub fn initial_rotation(&self) -> Rotation {
        lock(&self.inner.state).rotation
    }
    pub fn file_manager(&self) -> RemoteFileManager<'_> {
        RemoteFileManager { device: self }
    }
    pub fn is_local_emulator(&self) -> bool {
        self.adb_serial().starts_with("emulator-")
    }
    pub fn serial_number(&self) -> String {
        let s = lock(&self.inner.state);
        if s.booted {
            s.serial.clone()
        } else {
            self.prefix(self.adb_serial())
        }
    }
    fn prefix(&self, serial: &str) -> String {
        format!(
            "{}:{}:{serial}",
            host_address(self.client().host()),
            self.client().port()
        )
        .as_str()
        .into()
    }
    pub(crate) fn log_named(&self, name: &str, level: Level, message: &str, error: Option<&Failure>) {
        self.runtime().emit(&self.runtime().logger(name), level, message, error)
    }
    pub(crate) fn log(&self, level: Level, message: &str, error: Option<&Failure>) {
        self.runtime().emit(&self.inner.logger, level, message, error)
    }
    pub(crate) fn md5_command(&self) -> String {
        lock(&self.inner.state).md5.clone()
    }
    fn timeouts(&self) -> &marathon_config::vendor::AndroidTimeoutConfiguration {
        &self.runtime().configuration.timeout_configuration
    }
    async fn request<R: ComplexRequest>(
        &self,
        request: R,
        serial: Option<&str>,
        signal: Arc<dyn CancellationSignal>,
    ) -> VendorResult<R::Output> {
        cancellable(signal, async {
            self.client().execute(request, serial).await.map_err(adb_error)
        })
        .await
    }
    /// [`Self::request`] for a request bounded by its caller (D21).
    async fn bounded_request<R: ComplexRequest>(
        &self,
        request: R,
        signal: Arc<dyn CancellationSignal>,
    ) -> VendorResult<R::Output> {
        cancellable(signal, async {
            self.bounded_client()
                .execute(request, Some(self.adb_serial()))
                .await
                .map_err(adb_error)
        })
        .await
    }
    async fn fetch_props(&self, signal: Arc<dyn CancellationSignal>) -> VendorResult<()> {
        let props = self.request(GetPropRequest, Some(self.adb_serial()), signal).await?;
        lock(&self.inner.state).props = props;
        Ok(())
    }
    pub async fn get_property(&self, name: &str, cached: bool, context: VendorContext) -> VendorResult<Option<String>> {
        if !cached {
            self.fetch_props(context.cancellation.clone()).await?;
        }
        Ok(lock(&self.inner.state).props.get(name).cloned())
    }
    pub async fn setup(&self, context: VendorContext) -> VendorResult<()> {
        lock(&self.inner.state).booted = false;
        let boot_clock = context.clock.clone();
        let boot = timed(&context, millis(self.timeouts().boot)?, |signal| {
            Box::pin(async move {
                loop {
                    // The boot timeout bounds it (D21).
                    let props = self.bounded_request(GetPropRequest, signal.clone()).await?;
                    lock(&self.inner.state).props = props;
                    if lock(&self.inner.state).props.contains_key("sys.boot_completed") {
                        break;
                    }
                    self.log(
                        Level::Debug,
                        &format!("Device {} is still booting...", self.serial_number().to_owned()),
                        None,
                    );
                    boot_clock.delay(1000, signal.clone()).await?;
                }
                self.log(
                    Level::Debug,
                    &format!("Device {} booted!", self.serial_number().to_owned()),
                    None,
                );
                lock(&self.inner.state).booted = true;
                Ok(())
            })
        })
        .await?;
        if boot.is_none() {
            return Err(setup(
                format!(
                    "Unable to configure device {}: not booted",
                    self.serial_number().to_owned()
                )
                .as_str(),
            ));
        }
        {
            let mut s = lock(&self.inner.state);
            s.abi = s
                .props
                .get("ro.product.cpu.abi")
                .cloned()
                .unwrap_or_else(|| "Unknown".into());
            s.version = match (
                s.props.get("ro.build.version.sdk"),
                s.props.get("ro.build.version.codename"),
            ) {
                (Some(sdk), Some(code)) => {
                    let sdk = sdk.to_owned();
                    let api = jcompat::jnum::parse_int(&sdk).ok_or_else(|| {
                        other(
                            "java.lang.NumberFormatException",
                            format!("For input string: \"{sdk}\"").as_str(),
                        )
                    })?;
                    AndroidVersion::new(api, &code.to_owned())
                }
                _ => AndroidVersion::default(),
            };
            s.model = s
                .props
                .get("ro.product.model")
                .cloned()
                .unwrap_or_else(|| "Unknown".into());
            s.manufacturer = s
                .props
                .get("ro.product.manufacturer")
                .cloned()
                .unwrap_or_else(|| "Unknown".into());
        }
        let rotation = self
            .safe_execute_shell_command("dumpsys input", "", context.clone())
            .await?
            .map(|r| parse_rotation(&r.output))
            .unwrap_or(Rotation::Rotation0);
        lock(&self.inner.state).rotation = rotation;
        let external = self
            .safe_execute_shell_command("echo $EXTERNAL_STORAGE", "", context.clone())
            .await?
            .ok_or_else(|| {
                setup(
                    format!(
                        "Unable to configure device {}: externalStorageMount",
                        self.serial_number().to_owned()
                    )
                    .as_str(),
                )
            })?;
        lock(&self.inner.state).external = external.output.to_owned().trim_matches(['\r', '\n']).into();
        let mut features = Vec::new();
        if self.version().is_greater_or_equal_than(19)
            && self.has_binary("/system/bin/screenrecord", context.clone()).await?
        {
            features.push(DeviceFeature::Video)
        }
        if self.version().is_greater_or_equal_than(16) {
            features.push(DeviceFeature::Screenshot)
        }
        let serial = detect_serial(
            self.runtime().configuration.serial_strategy,
            self.adb_serial(),
            &lock(&self.inner.state).props,
        );
        {
            let mut s = lock(&self.inner.state);
            s.features = features;
            s.serial = self.prefix(&serial);
        }
        for binary in ["md5", "md5sum"] {
            if self
                .has_binary(&format!("/system/bin/{binary}"), context.clone())
                .await?
            {
                lock(&self.inner.state).md5 = binary.into();
                break;
            }
        }
        self.fetch_props(context.cancellation.clone()).await?;
        let features = self
            .request(
                FetchDeviceFeaturesRequest::new(self.adb_serial()),
                None,
                context.cancellation.clone(),
            )
            .await?;
        lock(&self.inner.state).adb_features = features;
        self.start_logcat();
        if self.runtime().configuration.test_access_configuration.adb && !self.is_local_emulator() {
            context
                .clock
                .with_timeout(
                    context.cancellation.clone(),
                    millis(self.timeouts().port_forward)?,
                    Box::new(|signal| {
                        Box::pin(async move {
                            let port = self.client().port();
                            let local = RemotePortSpec::Tcp(port as i32);
                            self.bounded_request(
                                ReversePortForwardRequest {
                                    local: local.clone(),
                                    remote: LocalPortSpec::Tcp(port as i32),
                                    mode: PortForwardingMode::Default,
                                },
                                signal,
                            )
                            .await?;
                            lock(&self.inner.reverse).push(local);
                            Ok(())
                        })
                    }),
                )
                .await?;
        }
        Ok(())
    }
    async fn has_binary(&self, path: &str, context: VendorContext) -> VendorResult<bool> {
        Ok(self
            .safe_execute_shell_command(&format!("ls {path}"), "", context)
            .await?
            .is_some_and(|r| r.exit_code == 0))
    }
    pub async fn execute_shell_command(
        &self,
        command: &str,
        error_message: &str,
        context: VendorContext,
    ) -> VendorResult<Option<ShellCommandResult>> {
        // No timeout of its own: its callers are a 10 s health check and perfetto, which runs silent until
        // its listener kills it (D21).
        let result = self
            .bounded_request(ShellCommandRequest::new(command), context.cancellation)
            .await
            .map(Into::into);
        self.shell_catch(result.map(Some), error_message)
    }
    pub async fn safe_execute_shell_command(
        &self,
        command: &str,
        error_message: &str,
        context: VendorContext,
    ) -> VendorResult<Option<ShellCommandResult>> {
        let result = timed(&context, millis(self.timeouts().shell)?, |s| {
            Box::pin(async move {
                self.bounded_request(ShellCommandRequest::new(command), s)
                    .await
                    .map(Into::into)
            })
        })
        .await;
        self.shell_catch(result, error_message)
    }
    fn shell_catch(
        &self,
        result: VendorResult<Option<ShellCommandResult>>,
        message: &str,
    ) -> VendorResult<Option<ShellCommandResult>> {
        match result {
            Err(e) if e.failure().class != "io.vertx.core.impl.NoStackTraceThrowable" => {
                self.log(Level::Error, message, Some(e.failure()));
                Ok(None)
            }
            other => other,
        }
    }
    pub async fn critical_execute_shell_command(
        &self,
        command: &str,
        error_message: &str,
        context: VendorContext,
    ) -> VendorResult<ShellCommandResult> {
        timed(&context, millis(self.timeouts().shell)?, |s| {
            Box::pin(async move {
                self.bounded_request(ShellCommandRequest::new(command), s)
                    .await
                    .map(Into::into)
            })
        })
        .await?
        .ok_or_else(|| {
            other(
                "com.malinskiy.marathon.android.exception.CommandRejectedException",
                error_message,
            )
        })
    }
    pub async fn clear_package(&self, package: &str, context: VendorContext) -> VendorResult<ShellCommandResult> {
        self.critical_execute_shell_command(
            &format!("pm clear {package}"),
            &format!(
                "Could not clear package {package} on device: {}",
                self.serial_number().to_owned()
            ),
            context,
        )
        .await
    }
    pub fn execute_test_request<R: AsyncChannelRequest>(&self, request: R) -> VendorResult<ChannelStream<R>> {
        self.client()
            .execute_channel(request, Some(self.adb_serial()))
            .map_err(adb_error)
    }
    fn start_logcat(&self) {
        self.inner.logcat_ended.send_replace(false);
        let device = self.clone();
        let task = self
            .runtime()
            .dependencies
            .io
            .spawn_cancellable(Box::new(move |signal| {
                Box::pin(async move {
                    let result = async {
                        let mut stream = device
                            .client()
                            .execute_channel(logcat_request(&["long"]), Some(device.adb_serial()))
                            .map_err(adb_error)?;
                        let mut parser = crate::logcat::LogCatMessageParser::new();
                        let mut lines = LogcatLines::default();
                        loop {
                            let chunk = cancellable(signal.clone(), async {
                                stream.next().await.transpose().map_err(adb_error)
                            })
                            .await?;
                            let Some(chunk) = chunk else { break };
                            for message in parser.process_log_lines(&lines.push(&chunk)) {
                                device.on_logcat(&message).await?;
                            }
                        }
                        for message in parser.process_log_lines(&lines.finish()) {
                            device.on_logcat(&message).await?;
                        }
                        Ok(())
                    }
                    .await;
                    device.inner.logcat_ended.send_replace(true);
                    if let Err(e) = &result
                        && !matches!(e, VendorError::Cancelled(_))
                    {
                        device.log_named(
                            "com.malinskiy.marathon.android.adam.LogcatManager",
                            Level::Error,
                            "CoroutineContext closing due to unrecoverable error",
                            Some(e.failure()),
                        );
                    }
                    result
                })
            }));
        *lock(&self.inner.logcat) = Some(task);
    }
    pub async fn on_logcat(&self, message: &crate::logcat::LogCatMessage) -> VendorResult<()> {
        let line = format!(
            "{} {}-{}/{} {}/{}: {}",
            message.timestamp().to_owned(),
            message.pid(),
            message.tid(),
            message.app_name().to_owned(),
            char::from_u32(message.log_level().priority_letter() as u32).unwrap_or('?'),
            message.tag().to_owned(),
            message.message.to_owned()
        );
        self.on_line(line.as_str()).await?;
        let listeners = lock(&self.inner.logcat_listeners).clone();
        for listener in listeners {
            listener.on_message(message)
        }
        Ok(())
    }
    pub async fn on_line(&self, line: &str) -> VendorResult<()> {
        let listeners = lock(&self.inner.line_listeners).clone();
        for listener in listeners {
            listener.on_line(line).await?;
        }
        Ok(())
    }
    /// D22: this device is still listed after its adb server came back, and keeps its actor. What lived on the
    /// lost connections is set up again: the logcat stream, and the reverse port forwards, which a restarted server
    /// has forgotten. Failures are logged; they do not make the device unusable.
    pub(crate) async fn resume_after_server_return(&self, context: VendorContext) -> VendorResult<()> {
        if *self.inner.logcat_ended.borrow() {
            self.stop_logcat().await;
            self.start_logcat();
        }
        let rules = lock(&self.inner.reverse).clone();
        for local in rules {
            let RemotePortSpec::Tcp(port) = local else { continue };
            let result = timed(&context, millis(self.timeouts().port_forward)?, |signal| {
                Box::pin(self.bounded_request(
                    ReversePortForwardRequest {
                        local: RemotePortSpec::Tcp(port),
                        remote: LocalPortSpec::Tcp(port),
                        mode: PortForwardingMode::Default,
                    },
                    signal,
                ))
            })
            .await;
            match result {
                Ok(Some(_)) => {}
                Err(e @ VendorError::Cancelled(_)) => return Err(e),
                Ok(None) => self.log(Level::Warn, "Timed out restoring reverse port forward", None),
                Err(e) => self.log(Level::Warn, "Unable to restore reverse port forward", Some(e.failure())),
            }
        }
        Ok(())
    }
    /// D41: this device came back within its grace period. If it restarted meanwhile it is still booting and has lost
    /// what setup did, so it has to go through setup again: false, as it is if the device does not answer.
    pub(crate) async fn finished_booting(&self, context: &VendorContext) -> VendorResult<bool> {
        let props = timed(context, millis(self.timeouts().shell)?, |signal| {
            Box::pin(self.bounded_request(GetPropRequest, signal))
        })
        .await;
        match props {
            Err(e @ VendorError::Cancelled(_)) => Err(e),
            Err(e) => {
                self.log(
                    Level::Warn,
                    "Unable to read a returned device's properties",
                    Some(e.failure()),
                );
                Ok(false)
            }
            Ok(None) => Ok(false),
            Ok(Some(props)) => Ok(props.contains_key("sys.boot_completed")),
        }
    }
    /// D41: this device came back, and keeps its actor. What adb closed with the transport comes back as in D22.
    pub(crate) async fn resume_after_blip(&self, context: VendorContext) -> VendorResult<()> {
        let mut ended = self.inner.logcat_ended.subscribe();
        let wait = context
            .clock
            .delay(LOGCAT_END_WAIT_MILLIS, context.cancellation.clone());
        tokio::select! {
            _ = ended.wait_for(|ended| *ended) => {}
            elapsed = wait => elapsed?,
        }
        self.resume_after_server_return(context).await
    }
    /// D22, D41: why a failed request does not count against this device: its adb server is away, or the transport is
    /// passing through a brief `offline`. The tracker hears of either at about the same moment as the caller, possibly
    /// just after, so this also asks the server, which takes one `host:devices`, only after a failure. A device on the
    /// list as `device`, or no longer on it, gives `None`.
    async fn passing_loss(&self, signal: Arc<dyn CancellationSignal>) -> Option<&'static str> {
        match self.loss_cause(signal).await {
            LossCause::Passing(why) => Some(why),
            LossCause::Listed | LossCause::Unlisted => None,
        }
    }
    async fn loss_cause(&self, signal: Arc<dyn CancellationSignal>) -> LossCause {
        const SERVER: &str = "the adb server is away";
        const DEVICE: &str = "the device is briefly offline";
        if !self.adb_server_available() {
            return LossCause::Passing(SERVER);
        }
        if self.is_held() {
            return LossCause::Passing(DEVICE);
        }
        let devices = match self.request(ListDevicesRequest, None, signal).await {
            Err(_) => return LossCause::Passing(SERVER),
            Ok(devices) => devices,
        };
        match devices.iter().find(|d| d.serial == self.adb_serial()).map(|d| d.state) {
            Some(state) if is_passing(state) => LossCause::Passing(DEVICE),
            Some(DeviceState::Device) => LossCause::Listed,
            _ => LossCause::Unlisted,
        }
    }
    /// D49: a request was rejected or closed before its reply, and the server still lists this device as `device`. If the
    /// device also answers, the connection was at fault and the device is kept, a limited number of times. A dead device
    /// that stays listed does not answer, and goes at its first loss. So does one that restarted meanwhile.
    async fn survives_request_loss(&self, context: &VendorContext) -> VendorResult<bool> {
        let serial = self.adb_serial();
        if self.inner.tracker.request_losses(serial) >= REQUEST_LOSSES_PER_DEVICE {
            self.log(
                Level::Warn,
                &format!("Device {serial} lost {REQUEST_LOSSES_PER_DEVICE} requests already; not keeping it"),
                None,
            );
            return Ok(false);
        }
        if !self.finished_booting(context).await? {
            return Ok(false);
        }
        self.inner.tracker.count_request_loss(serial);
        Ok(true)
    }
    fn is_gate_closed(&self) -> bool {
        !self.adb_server_available() || self.is_held()
    }
    /// D22, D41: after a failed request whose caller would retry at once, whether that failure was an outage of the adb
    /// server or a device's grace period, in which case this has waited it out and the caller has not used up a retry.
    /// It waits as batches do, until the way is clear or the device has been given up; a device given up says false, so
    /// the caller's retries run out and the run ends. The probe covers the moment until the tracker has heard, for no
    /// longer than a server would get.
    pub(crate) async fn wait_out_passing_loss(&self, context: &VendorContext) -> VendorResult<bool> {
        let limit = self.runtime().configuration.wait_for_devices_timeout_millis;
        let mut waited = 0;
        while !self.is_gate_closed() {
            if !self.is_live()
                || self.client().socket_factory().is_closed()
                || self.passing_loss(context.cancellation.clone()).await.is_none()
                || waited >= limit
            {
                return Ok(false);
            }
            let poll = context.clock.delay(OUTAGE_POLL_MILLIS, context.cancellation.clone());
            tokio::select! {
                elapsed = poll => elapsed?,
                _ = self.inner.tracker.unavailable(self.adb_serial()) => {}
            }
            waited += OUTAGE_POLL_MILLIS;
        }
        cancellable(context.cancellation.clone(), async {
            self.inner.tracker.available(self.adb_serial()).await;
            Ok(())
        })
        .await?;
        Ok(self.is_live())
    }
    pub(crate) async fn stop_logcat(&self) {
        let task = lock(&self.inner.logcat).take();
        if let Some(task) = task {
            task.cancel(requested("logcat closed"));
            let _ = task.join().await;
        }
    }
    async fn dispose_inner(&self) -> VendorResult<()> {
        let mut disposed = self.inner.disposal.lock().await;
        if *disposed {
            return Ok(());
        }
        let task = lock(&self.inner.logcat).take();
        if let Some(task) = task {
            task.cancel(requested("device disposed"));
            let _ = task.join().await;
        }
        let rules = std::mem::take(&mut *lock(&self.inner.reverse));
        for local in rules {
            let context = self.cleanup_context();
            if let Err(e) = self
                .request(
                    RemoveReversePortForwardRequest { local },
                    Some(self.adb_serial()),
                    context.cancellation,
                )
                .await
            {
                self.log(Level::Warn, "Unable to remove reverse port forward", Some(e.failure()));
            }
        }
        *disposed = true;
        Ok(())
    }
    pub(crate) fn cleanup_context(&self) -> VendorContext {
        VendorContext {
            cancellation: Signal::new(),
            clock: self.runtime().dependencies.clock.clone(),
            hooks: self.runtime().dependencies.hooks.clone(),
        }
    }
}
impl Device for AndroidDevice {
    fn info(&self) -> DeviceInfo {
        let s = lock(&self.inner.state);
        DeviceInfo {
            operating_system: OperatingSystem {
                version: s.version.api_string_without_extension(),
            },
            serial_number: if s.booted {
                s.serial.clone()
            } else {
                self.prefix(self.adb_serial())
            },
            model: s.model.clone(),
            manufacturer: s.manufacturer.clone(),
            // D24: Kotlin freezes CONNECTED at construction (N29). D41: a device in its grace period is not connected
            // now, whatever becomes of it, and an attempt ending then says so.
            network_state: if self.is_live() && !self.is_held() {
                NetworkState::Connected
            } else {
                NetworkState::Disconnected
            },
            device_features: s.features.clone(),
            healthy: self.is_live() && !self.is_held(),
        }
    }
    fn abi(&self) -> String {
        lock(&self.inner.state).abi.clone()
    }
    fn worker_id(&self) -> Option<String> {
        None
    }
    fn logger(&self) -> &Logger {
        &self.inner.logger
    }
    fn prepare<'a>(
        &'a self,
        configuration: Arc<marathon_config::Configuration>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        let device = self.clone();
        Box::pin(call_vendor(
            self.runtime().dependencies.io.as_ref(),
            context.cancellation.clone(),
            move |signal| {
                Box::pin(async move {
                    let context = VendorContext {
                        cancellation: signal,
                        ..context
                    };
                    let start = context
                        .hooks
                        .instant("core.Track.trackDevicePreparing")
                        .map_err(|e| other("ReplayError", e.message))?;
                    let mut dependencies = device.runtime().dependencies.clone();
                    dependencies.configuration = configuration;
                    let installation = AndroidRuntime::new(dependencies, device.runtime().executor.clone()).await?;
                    crate::installer::AndroidAppInstaller::new(installation)
                        .prepare_installation(&device, context.clone())
                        .await?;
                    // D27: Kotlin runs `rm -r $EXTERNAL_STORAGE` and `mkdir` here, wiping the user's data (N4).
                    device
                        .safe_execute_shell_command(
                            "logcat -c",
                            &format!(
                                "Could not clear logcat on device: {}",
                                device.serial_number().to_owned()
                            ),
                            context.clone(),
                        )
                        .await?;
                    let end = context
                        .hooks
                        .instant("core.Track.trackDevicePreparing")
                        .map_err(|e| other("ReplayError", e.message))?;
                    device
                        .runtime()
                        .dependencies
                        .tracker
                        .device_preparing(&device.serial_number(), start, end)
                        .await
                })
            },
        ))
    }
    fn execute<'a>(
        &'a self,
        configuration: Arc<marathon_config::Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<dyn Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        let device = self.clone();
        Box::pin(call_vendor(
            self.runtime().dependencies.io.as_ref(),
            context.cancellation.clone(),
            move |signal| {
                Box::pin(async move {
                    // D22, D41: during an outage or a grace period a batch waits, whatever comes of it.
                    cancellable(signal.clone(), async {
                        device.inner.tracker.available(device.adb_serial()).await;
                        Ok(())
                    })
                    .await?;
                    let blips = device.inner.tracker.blips(device.adb_serial());
                    let context = VendorContext {
                        cancellation: signal.clone(),
                        ..context
                    };
                    let outcome = device
                        .runtime()
                        .executor
                        .execute(&device, configuration, pool, batch, result, context.clone())
                        .await;
                    // D41: the actor asks for the next batch as this returns, and a batch given to a device that does
                    // not come back goes back to the queue charged to its tests, which never ran. So it waits here. An
                    // actor terminated meanwhile accounts for this batch as usual.
                    if device.is_held() {
                        cancellable(signal.clone(), async {
                            device.inner.tracker.available(device.adb_serial()).await;
                            Ok(())
                        })
                        .await?;
                    }
                    match outcome {
                        // D22, D41: the device outlives a lost adb server or a brief `offline`, so only its batch
                        // fails: retiring the last device of a pool would end the run before it returns. A grace period
                        // that began during the batch counts, as the server's answer no longer shows it.
                        Err(VendorError::DeviceLost(failure)) => {
                            let passing = if device.inner.tracker.blips(device.adb_serial()) != blips {
                                Some("the device was briefly offline")
                            } else {
                                match device.loss_cause(signal).await {
                                    LossCause::Passing(why) => Some(why),
                                    // D49: the tracker sees no change here, so a device retired now would stay out of
                                    // the run however healthy it is.
                                    LossCause::Listed if device.survives_request_loss(&context).await? => {
                                        Some("a request was lost, while the device is listed and answers")
                                    }
                                    LossCause::Listed | LossCause::Unlisted => None,
                                }
                            };
                            match passing {
                                Some(why) => {
                                    device.log(
                                        Level::Warn,
                                        &format!("Batch failed because {why}; keeping the device"),
                                        Some(&failure),
                                    );
                                    Err(VendorError::BatchExecution(failure))
                                }
                                None => Err(VendorError::DeviceLost(failure)),
                            }
                        }
                        outcome => outcome,
                    }
                })
            },
        ))
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        let device = self.clone();
        Box::pin(call_vendor(
            self.runtime().dependencies.io.as_ref(),
            Signal::new(),
            move |_| Box::pin(async move { device.dispose_inner().await }),
        ))
    }
    fn return_wait_used_millis(&self) -> i64 {
        self.inner.return_wait_used.load(Ordering::Acquire)
    }
}
impl LogProducer for AndroidDevice {
    fn add_line_listener(&self, listener: Arc<dyn LineListener>) -> VendorResult<()> {
        lock(&self.inner.line_listeners).push(listener);
        Ok(())
    }
    fn remove_line_listener(&self, listener: &Arc<dyn LineListener>) -> VendorResult<()> {
        let mut listeners = lock(&self.inner.line_listeners);
        if let Some(i) = listeners.iter().position(|l| Arc::ptr_eq(l, listener)) {
            listeners.remove(i);
        }
        Ok(())
    }
}
impl crate::logcat_listener::LogcatProducer for AndroidDevice {
    fn add_logcat_listener(&self, listener: Arc<dyn crate::logcat_listener::LogcatListener>) {
        lock(&self.inner.logcat_listeners).push(listener)
    }
    fn remove_logcat_listener(&self, listener: &Arc<dyn crate::logcat_listener::LogcatListener>) {
        let mut listeners = lock(&self.inner.logcat_listeners);
        if let Some(i) = listeners.iter().position(|l| Arc::ptr_eq(l, listener)) {
            listeners.remove(i);
        }
    }
}
pub fn host_address(ip: std::net::IpAddr) -> String {
    // This spelling is part of device IDs and result data, beyond D57's diagnostics.
    match ip {
        std::net::IpAddr::V4(ip) => ip.to_string(),
        std::net::IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or_else(
            || {
                ip.segments()
                    .iter()
                    .map(|part| format!("{part:x}"))
                    .collect::<Vec<_>>()
                    .join(":")
            },
            |ip| ip.to_string(),
        ),
    }
}
pub fn detect_serial(
    strategy: marathon_config::vendor::SerialStrategy,
    adb: &str,
    props: &HashMap<String, String>,
) -> String {
    use marathon_config::vendor::SerialStrategy::*;
    let prop = |name| props.get(name).cloned().unwrap_or_default();
    match strategy {
        Automatic => [
            prop("marathon.serialno"),
            adb.to_owned(),
            prop("ro.boot.serialno"),
            prop("net.hostname"),
        ]
        .into_iter()
        .find(|v| !v.is_empty())
        .unwrap_or_default(),
        MarathonProperty => prop("marathon.serialno"),
        BootProperty => prop("ro.boot.serialno"),
        Hostname => prop("net.hostname"),
        Ddms => adb.to_owned(),
    }
}
pub fn parse_rotation(output: &str) -> Rotation {
    let text = output.to_owned();
    let parse = || {
        let start = text.find("SurfaceOrientation")?;
        let line = text[start..].split_once('\n')?.0;
        let parts: Vec<_> = line.split(':').collect();
        if parts.len() != 2 {
            return None;
        }
        Rotation::of(jcompat::jnum::parse_int(&trim(parts[1]))?)
    };
    parse().unwrap_or(Rotation::Rotation0)
}
