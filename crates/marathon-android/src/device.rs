//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AdamAndroidDevice.kt,
//! BaseAndroidDevice.kt, AndroidDevice.kt and adam/LogcatManager.kt.
use crate::{
    cleanup::{self, ChildSignal, DeviceWork, Work},
    models::*,
    remote_file_manager::RemoteFileManager,
    requests::*,
    services::AndroidRuntime,
    support::*,
    tracker::{DeviceStateTracker, is_passing},
    version::AndroidVersion,
};
use marathon_adb::client::{
    AndroidDebugBridgeClient, AsyncChannelRequest, ChannelStream, ComplexRequest, Feature,
    device::{DeviceState, FetchDeviceFeaturesRequest, ListDevicesRequest},
    reverse::*,
    shell::{
        boot_marker::{BOOT_ID_COMMAND, MarkerCommand},
        v1::ShellCommandRequest,
    },
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
use marathon_error::{AdbKind, AndroidKind, FailureKind};
use marathon_logging::{Component, Level};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
};
use uuid::Uuid;

/// D22: how often `wait_out_passing_loss` looks again until the tracker has heard of a lost server.
const OUTAGE_POLL_MILLIS: i64 = 100;
/// D41: the logcat stream's end and the list reporting the blip come over different sockets, so after a very short blip
/// the stream may not have ended yet. It has in every real case seen, so this only limits the wait.
const LOGCAT_END_WAIT_MILLIS: i64 = 1000;
/// D49: lost requests per serial per adb server per run that a device listed as `device` and answering survives. Each one
/// charges its batch an attempt, so it is a count with no forgiveness, as D41's is. One after this retires the device.
const REQUEST_LOSSES_PER_DEVICE: u32 = 3;
/// D70: where the batch this device took last stands, so that it is given back uncharged only if none of it started. A
/// device runs one batch at a time.
struct BatchRecord {
    batch: BatchRef,
    /// Weak: a completed result holds the device, which holds this record. While the batch is parked, `execute` holds
    /// the result, so it can still be completed from here.
    result: Weak<Deferred<TestBatchResults>>,
    /// U197: the listener that gives the batch's results, to end a started batch with what ran when the device leaves.
    /// Weak for the same reason: the listener holds the device and the result.
    results: Option<Weak<dyn crate::listeners::AndroidTestRunListener>>,
    stage: Stage,
}
enum Stage {
    /// No instrumentation of it has been submitted.
    Unstarted,
    /// An instrumentation has been submitted, or the batch was accounted for without one. The core taking it back now
    /// ends it with what the listener has so far (U197).
    Started,
    /// Waiting at D22's or D41's gate for its next instrumentation, with what the earlier ones gave.
    Parked(TestBatchResults),
    /// The core took it back; nothing of it may start.
    Withdrawn,
    /// `execute` has returned.
    Done,
}
/// D22, D41, D49: why a request failed the way that means a lost device.
enum LossCause {
    /// The server is away, or the transport is passing through a brief `offline`.
    Passing(&'static str),
    /// The server lists it as `device`, so the tracker sees no change and would never connect it again (N15).
    Listed,
    /// No longer on the list (`None`), or listed in another state that is not passing, such as `unauthorized`.
    Unlisted(Option<DeviceState>),
}
pub(crate) enum ReturnedBoot {
    Ready,
    Unready,
    SetupLost,
}

#[derive(Clone, Copy)]
struct PreparationMarker {
    token: Uuid,
    confirmed: bool,
}

fn parse_identity(value: &str) -> Option<Uuid> {
    value.parse::<Uuid>().ok().filter(|id| !id.is_nil())
}

fn setup_was_lost(reply: &str, previous: Option<Uuid>, token: Uuid) -> bool {
    let mut lines = reply.lines();
    let (Some(boot), Some(marker), None) = (lines.next(), lines.next(), lines.next()) else {
        return false;
    };
    let (Some(boot), Some(marker)) = (boot.strip_prefix("boot "), marker.strip_prefix("marker ")) else {
        return false;
    };
    matches!((previous, parse_identity(boot)), (Some(previous), Some(boot)) if previous != boot)
        || marker == "missing"
        || marker
            .strip_prefix("value=")
            .and_then(parse_identity)
            .is_some_and(|current| current != token)
}

/// D41's passing states, as adb names them, for log lines.
fn passing_text(state: Option<DeviceState>) -> &'static str {
    match state {
        Some(DeviceState::Offline) => "the device is briefly offline",
        Some(DeviceState::Connecting) => "the device is briefly connecting",
        Some(DeviceState::Authorizing) => "the device is briefly authorizing",
        _ => "the device is briefly not ready",
    }
}
/// adb's name for a state, for log lines.
fn state_name(state: DeviceState) -> &'static str {
    match state {
        DeviceState::Offline => "offline",
        DeviceState::Bootloader => "bootloader",
        DeviceState::Device => "device",
        DeviceState::Host => "host",
        DeviceState::Recovery => "recovery",
        DeviceState::Rescue => "rescue",
        DeviceState::Sideload => "sideload",
        DeviceState::Unauthorized => "unauthorized",
        DeviceState::Authorizing => "authorizing",
        DeviceState::Connecting => "connecting",
        DeviceState::Unknown => "unknown",
    }
}
/// x77: the grace counter moved while a request ran, for a grace of any passing state.
const LOST_BRIEFLY: &str = "the device briefly lost its connection";
pub trait AndroidBatchExecutor: Send + Sync {
    fn execute<'a>(
        &'a self,
        device: &'a AndroidDevice,
        configuration: Arc<marathon_config::Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<Deferred<TestBatchResults>>,
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
        _result: Arc<Deferred<TestBatchResults>>,
        _context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async {
            Err(other(
                FailureKind::UnsupportedOperation,
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
    cleanup_client: AndroidDebugBridgeClient,
    work: DeviceWork,
    screenrecord_active: AtomicBool,
    perfetto_active: AtomicBool,
    adb_serial: String,
    runtime: Arc<AndroidRuntime>,
    tracker: Arc<DeviceStateTracker>,
    logger: Component,
    state: Mutex<State>,
    line_listeners: Mutex<Vec<Arc<dyn LineListener>>>,
    logcat_listeners: Mutex<Vec<Arc<dyn crate::logcat_listener::LogcatListener>>>,
    logcat: Mutex<Option<Arc<dyn VendorTask>>>,
    /// D22: the logcat stream has ended, as it does when the adb server goes away.
    logcat_ended: tokio::sync::watch::Sender<bool>,
    reverse: Mutex<Vec<RemotePortSpec>>,
    recovery: tokio::sync::Mutex<()>,
    /// D24: this device object was disconnected. A reconnect makes a new object on the same tracker, whose
    /// state then says `device` again, so the tracker alone cannot tell.
    lost: AtomicBool,
    /// D48: what an outage or a grace period had waited for it when it was disconnected.
    return_wait_used: AtomicI64,
    /// D85: when the pool actor exhausted preparation of this instance.
    prepare_failed_at: OnceLock<i64>,
    /// D70.
    batch: Mutex<Option<BatchRecord>>,
    /// D87: when this device's preparation must end, on the run's vendor clock; `i64::MAX` for none.
    preparation_deadline: AtomicI64,
    /// D87: the deadline above was taken over from the instance this one replaces (D85), and the actor keeps it.
    preparation_continued: AtomicBool,
}
struct State {
    booted: bool,
    boot_identity: Option<Uuid>,
    preparation_marker: Option<PreparationMarker>,
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
        let logger = runtime.component("marathon_android::device");
        let cleanup_client = client.scoped().without_idle_limit();
        let client = client.scoped();
        Self {
            inner: Arc::new(Inner {
                reference: Mutex::default(),
                bounded_client: client.without_idle_limit(),
                work: DeviceWork::new(client.socket_factory().clone()),
                client,
                cleanup_client,
                screenrecord_active: AtomicBool::new(false),
                perfetto_active: AtomicBool::new(false),
                adb_serial,
                runtime,
                tracker,
                logger,
                state: Mutex::new(State {
                    booted: false,
                    boot_identity: None,
                    preparation_marker: None,
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
                recovery: tokio::sync::Mutex::new(()),
                lost: AtomicBool::new(false),
                return_wait_used: AtomicI64::new(0),
                prepare_failed_at: OnceLock::new(),
                batch: Mutex::default(),
                preparation_deadline: AtomicI64::new(i64::MAX),
                preparation_continued: AtomicBool::new(false),
            }),
        }
    }
    /// D24: from now on this object reports disconnected, whatever happens to its serial later.
    pub(crate) fn mark_disconnected(&self) {
        self.inner.lost.store(true, Ordering::Release);
    }
    /// The same device object, not another one with the same serial.
    pub(crate) fn is_same(&self, other: &AndroidDevice) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    pub(crate) fn set_return_wait_used(&self, millis: i64) {
        self.inner.return_wait_used.store(millis, Ordering::Release);
    }
    /// D87: the deadline of this instance's preparation, if one was opened.
    pub(crate) fn preparation_deadline(&self) -> Option<i64> {
        let deadline = self.inner.preparation_deadline.load(Ordering::Acquire);
        (deadline != i64::MAX).then_some(deadline)
    }
    /// D87: this instance, set up again by D85 after its predecessor's preparation was exhausted, continues that
    /// preparation's deadline instead of opening a new one.
    pub(crate) fn continue_preparation(&self, deadline: i64) {
        self.inner.preparation_deadline.store(deadline, Ordering::Release);
        self.inner.preparation_continued.store(true, Ordering::Release);
    }
    pub(crate) fn prepare_failed_at(&self) -> Option<i64> {
        self.inner.prepare_failed_at.get().copied()
    }
    /// D24: the provider has dropped this device object.
    fn is_disconnected(&self) -> bool {
        self.inner.lost.load(Ordering::Acquire)
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
        self.runtime()
            .emit(&self.runtime().component(name), level, message, error)
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
    pub(crate) fn stopping(&self) -> bool {
        self.inner.work.is_sealed()
    }
    pub(crate) fn ensure_running(&self) -> VendorResult<()> {
        self.inner.work.ensure_running()
    }
    pub(crate) fn admit(&self) -> VendorResult<Work> {
        self.inner.work.admit()
    }
    pub(crate) fn spawn_owned(&self, body: VendorTaskBody) -> VendorResult<Arc<dyn VendorTask>> {
        self.inner.work.spawn(self.runtime().dependencies.io.as_ref(), body)
    }
    pub(crate) fn spawn_admitted(&self, work: Work, body: VendorTaskBody) -> Arc<dyn VendorTask> {
        self.inner
            .work
            .spawn_admitted(self.runtime().dependencies.io.as_ref(), work, body)
    }
    pub(crate) fn force_with(&self, signal: Arc<dyn CancellationSignal>) -> Arc<dyn CancellationSignal> {
        Arc::new(ChildSignal(signal, self.inner.work.force.clone()))
    }
    pub(crate) fn screenrecord_active(&self, active: bool) {
        self.inner.screenrecord_active.store(active, Ordering::Release);
    }
    pub(crate) fn perfetto_active(&self, active: bool) {
        self.inner.perfetto_active.store(active, Ordering::Release);
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
        let _work = self.admit()?;
        let context = VendorContext {
            cancellation: self.force_with(context.cancellation),
            ..context
        };
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
                        Level::DEBUG,
                        &format!("Device {} is still booting...", self.serial_number().to_owned()),
                        None,
                    );
                    boot_clock.delay(1000, signal.clone()).await?;
                }
                self.log(
                    Level::DEBUG,
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
                            FailureKind::NumberFormat,
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
        // An adbd restart keeps the kernel's boot ID. Boot-completed and static boot properties cannot distinguish that
        // from a full reboot, and SELinux restricts the framework's first-boot timestamp on newer Android versions.
        let boot_identity = timed(&context, millis(self.timeouts().shell)?, |signal| {
            Box::pin(self.read_boot_id(signal))
        })
        .await;
        lock(&self.inner.state).boot_identity = self
            .optional_probe(boot_identity, "Unable to read device boot ID")?
            .flatten();
        let features = self
            .request(
                FetchDeviceFeaturesRequest::new(self.adb_serial()),
                None,
                context.cancellation.clone(),
            )
            .await?;
        lock(&self.inner.state).adb_features = features;
        self.start_logcat()?;
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
        self.timed_shell(self.bounded_client(), command, error_message, context)
            .await
    }
    pub(crate) async fn cleanup_shell(
        &self,
        command: &str,
        context: VendorContext,
    ) -> VendorResult<Option<ShellCommandResult>> {
        self.timed_shell(
            &self.inner.cleanup_client,
            command,
            "Android cleanup command failed",
            context,
        )
        .await
    }
    async fn timed_shell(
        &self,
        client: &AndroidDebugBridgeClient,
        command: &str,
        error_message: &str,
        context: VendorContext,
    ) -> VendorResult<Option<ShellCommandResult>> {
        let result = timed(&context, millis(self.timeouts().shell)?, |s| {
            Box::pin(cancellable(s, async move {
                client
                    .execute(ShellCommandRequest::new(command), Some(self.adb_serial()))
                    .await
                    .map(Into::into)
                    .map_err(adb_error)
            }))
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
            Err(e) if e.failure().kind != FailureKind::Adb(AdbKind::TransportClosed) => {
                self.log(Level::ERROR, message, Some(e.failure()));
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
        .ok_or_else(|| other(FailureKind::Android(AndroidKind::CommandRejected), error_message))
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
    fn start_logcat(&self) -> VendorResult<()> {
        let mut logcat = lock(&self.inner.logcat);
        self.inner.logcat_ended.send_replace(false);
        let device = self.clone();
        let task = self.spawn_owned(Box::new(move |signal| {
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
                            cancellable(signal.clone(), device.on_logcat(&message)).await?;
                        }
                    }
                    for message in parser.process_log_lines(&lines.finish()) {
                        cancellable(signal.clone(), device.on_logcat(&message)).await?;
                    }
                    Ok(())
                }
                .await;
                device.inner.logcat_ended.send_replace(true);
                if let Err(e) = &result
                    && !matches!(e, VendorError::Cancelled(_))
                {
                    device.log_named(
                        "marathon_android::device",
                        Level::ERROR,
                        "Task stopped due to unrecoverable error",
                        Some(e.failure()),
                    );
                }
                result
            })
        }))?;
        *logcat = Some(task);
        Ok(())
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
    ///
    /// U170: this runs outside the tracking loop, so the device may have been disconnected meanwhile. It holds the
    /// recovery lock against marker writes. Disposal seals admission and joins a recovery before undoing its work,
    /// or closes its request sockets if it has not joined by the grace deadline.
    pub(crate) async fn resume_after_server_return(&self, context: VendorContext) -> VendorResult<()> {
        let _work = self.admit()?;
        let context = VendorContext {
            cancellation: self.force_with(context.cancellation),
            ..context
        };
        let _recovery = cancellable(context.cancellation.clone(), async {
            Ok(self.inner.recovery.lock().await)
        })
        .await?;
        if let Some(reason) = context.cancellation.reason() {
            return Err(VendorError::Cancelled(reason));
        }
        if self.stopping() || self.inner.lost.load(Ordering::Acquire) {
            return Ok(());
        }
        if *self.inner.logcat_ended.borrow() {
            self.stop_logcat().await;
            self.start_logcat()?;
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
                Ok(None) => self.log(Level::WARN, "Timed out restoring reverse port forward", None),
                Err(e) => self.log(Level::WARN, "Unable to restore reverse port forward", Some(e.failure())),
            }
        }
        Ok(())
    }
    async fn read_boot_id(&self, signal: Arc<dyn CancellationSignal>) -> VendorResult<Option<Uuid>> {
        let response = self
            .bounded_request(ShellCommandRequest::new(BOOT_ID_COMMAND), signal)
            .await?;
        if response.exit_code != 0 {
            return Ok(None);
        }
        Ok(parse_identity(response.output().trim()))
    }
    async fn record_preparation(&self, context: &VendorContext) -> VendorResult<()> {
        let _recovery = cancellable(context.cancellation.clone(), async {
            Ok(self.inner.recovery.lock().await)
        })
        .await?;
        if self.stopping() || self.is_disconnected() {
            return Ok(());
        }
        // A snapshot may include the provider setup but precede the APK installation. Publish the
        // marker only once the pool's preparation has finished.
        let token = {
            let mut state = lock(&self.inner.state);
            if self.stopping() {
                return Ok(());
            }
            let marker = state.preparation_marker.get_or_insert_with(|| PreparationMarker {
                token: Uuid::new_v4(),
                confirmed: false,
            });
            marker.confirmed = false;
            marker.token
        };
        let probe = timed(context, millis(self.timeouts().shell)?, |signal| {
            Box::pin(self.bounded_request(ShellCommandRequest::new(MarkerCommand::Write.command(token)), signal))
        })
        .await;
        let confirmed = self
            .optional_probe(probe, "Unable to write Android preparation marker")?
            .is_some_and(|reply| reply.exit_code == 0 && parse_identity(reply.output().trim()) == Some(token));
        if let Some(marker) = &mut lock(&self.inner.state).preparation_marker {
            marker.confirmed = confirmed;
        }
        Ok(())
    }
    async fn clear_preparation_marker(&self, context: VendorContext) -> VendorResult<()> {
        let marker = lock(&self.inner.state).preparation_marker.take();
        if let Some(marker) = marker
            && self.adb_server_available()
            && self.inner.tracker.get_state(self.adb_serial()) == Some(DeviceState::Device)
        {
            let command = MarkerCommand::Remove.command(marker.token);
            self.cleanup_shell(&command, context).await?;
        }
        Ok(())
    }
    fn optional_probe<T>(&self, probe: VendorResult<Option<T>>, message: &str) -> VendorResult<Option<T>> {
        match probe {
            Err(e @ VendorError::Cancelled(_)) => Err(e),
            Err(e) => {
                self.log(Level::WARN, message, Some(e.failure()));
                Ok(None)
            }
            Ok(answer) => Ok(answer),
        }
    }
    async fn finished_booting(&self, context: &VendorContext) -> VendorResult<bool> {
        let props = timed(context, millis(self.timeouts().shell)?, |signal| {
            Box::pin(self.bounded_request(GetPropRequest, signal))
        })
        .await;
        Ok(self
            .optional_probe(props, "Unable to read a returned device's properties")?
            .is_some_and(|props| props.contains_key("sys.boot_completed")))
    }
    /// D85: both return paths check the setup's boot. D41 also requires an answer with the boot-completed property.
    pub(crate) async fn returned_boot(
        &self,
        context: &VendorContext,
        check_readiness: bool,
    ) -> VendorResult<ReturnedBoot> {
        let (previous, marker) = {
            let state = lock(&self.inner.state);
            (
                state.boot_identity,
                state.preparation_marker.filter(|marker| marker.confirmed),
            )
        };
        if previous.is_none() && marker.is_none() && !check_readiness {
            return Ok(ReturnedBoot::Ready);
        }
        // The existing readiness check and the new boot ID read share D41's shell limit. When only the
        // ID read times out, keep the readiness answer already received.
        let mut ready = !check_readiness;
        let readiness = &mut ready;
        let probe = timed(context, millis(self.timeouts().shell)?, |signal| {
            Box::pin(async move {
                if check_readiness {
                    *readiness = self
                        .bounded_request(GetPropRequest, signal.clone())
                        .await?
                        .contains_key("sys.boot_completed");
                }
                if !*readiness {
                    return Ok(false);
                }
                if let Some(marker) = marker {
                    let reply = self
                        .bounded_request(
                            ShellCommandRequest::new(MarkerCommand::Read.command(marker.token)),
                            signal,
                        )
                        .await?;
                    return Ok(reply.exit_code == 0 && setup_was_lost(&reply.output(), previous, marker.token));
                }
                Ok(match previous {
                    Some(previous) => self
                        .read_boot_id(signal)
                        .await?
                        .is_some_and(|current| current != previous),
                    None => false,
                })
            })
        })
        .await;
        let message = if ready {
            "Unable to read a returned device's setup identity"
        } else {
            "Unable to read a returned device's properties"
        };
        if self.optional_probe(probe, message)? == Some(true) {
            return Ok(ReturnedBoot::SetupLost);
        }
        Ok(if ready {
            ReturnedBoot::Ready
        } else {
            ReturnedBoot::Unready
        })
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
            LossCause::Listed | LossCause::Unlisted(_) => None,
        }
    }
    async fn loss_cause(&self, signal: Arc<dyn CancellationSignal>) -> LossCause {
        const SERVER: &str = "the adb server is away";
        // U170: a device whose restoration after the server's return is still running counts as still in the outage, as
        // it did while the restorations ran inside the tracking loop, before the server's gate opened.
        if !self.adb_server_available() || self.inner.tracker.is_restoring(self.adb_serial()) {
            return LossCause::Passing(SERVER);
        }
        if self.is_held() {
            return LossCause::Passing(passing_text(self.inner.tracker.get_state(self.adb_serial())));
        }
        let devices = match self.request(ListDevicesRequest, None, signal).await {
            // Counted as the server's absence, as D22 has it; the line says only what is known.
            Err(VendorError::Cancelled(_)) => {
                return LossCause::Passing("the device's state could not be checked before the request was cancelled");
            }
            Err(_) => return LossCause::Passing("the adb server did not answer"),
            Ok(devices) => devices,
        };
        match devices.iter().find(|d| d.serial == self.adb_serial()).map(|d| d.state) {
            Some(state) if is_passing(state) => LossCause::Passing(passing_text(Some(state))),
            Some(DeviceState::Device) => LossCause::Listed,
            state => LossCause::Unlisted(state),
        }
    }
    /// D49: a request was rejected or closed before its reply, and the server still lists this device as `device`. If the
    /// device also answers, the connection was at fault and the device is kept, a limited number of times. A dead device
    /// that stays listed does not answer, and goes at its first loss. So does one that restarted meanwhile.
    async fn survives_request_loss(&self, context: &VendorContext) -> VendorResult<bool> {
        let serial = self.adb_serial();
        if self.inner.tracker.request_losses(serial) >= REQUEST_LOSSES_PER_DEVICE {
            self.log(
                Level::WARN,
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
    /// x77: why a request that broke off mid-protocol, as adb's sync does when the transport closes, lost its device, if
    /// it did: the device has been disconnected, a grace began since `blips` was read (the device may be back by now),
    /// or [`Self::loss_cause`] says so. `None` for a device the server still lists as `device`, where the reply itself
    /// was at fault.
    pub async fn lost_connection(&self, signal: Arc<dyn CancellationSignal>, blips: u32) -> Option<String> {
        if self.is_disconnected() {
            return Some("the device has been disconnected".into());
        }
        if self.grace_periods() != blips {
            return Some(LOST_BRIEFLY.into());
        }
        match self.loss_cause(signal).await {
            LossCause::Passing(why) => Some(why.into()),
            LossCause::Unlisted(None) => Some("the device has left the adb server's list".into()),
            LossCause::Unlisted(Some(state)) => {
                Some(format!("the adb server lists the device as {}", state_name(state)))
            }
            LossCause::Listed => None,
        }
    }
    fn is_gate_closed(&self) -> bool {
        !self.adb_server_available() || self.is_held() || self.inner.tracker.is_restoring(self.adb_serial())
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
    /// D70: `execute` has `batch`. False if the core has already taken it back.
    fn take_batch(&self, batch: &BatchRef, result: &Arc<Deferred<TestBatchResults>>) -> bool {
        let mut record = lock(&self.inner.batch);
        if let Some(r) = record.as_ref()
            && Arc::ptr_eq(&r.batch, batch)
            && matches!(r.stage, Stage::Withdrawn)
        {
            return false;
        }
        *record = Some(BatchRecord {
            batch: batch.clone(),
            result: Arc::downgrade(result),
            results: None,
            stage: Stage::Unstarted,
        });
        true
    }
    fn end_batch(&self, batch: &BatchRef) {
        if let Some(r) = lock(&self.inner.batch).as_mut()
            && Arc::ptr_eq(&r.batch, batch)
        {
            r.results = None;
            if !matches!(r.stage, Stage::Withdrawn) {
                r.stage = Stage::Done;
            }
        }
    }
    /// U197: `listener` gives `batch`'s results, so a device that leaves once the batch has started can end it with what
    /// ran, as one waiting at the gate does. Called once, before the batch's first submission, by whoever owns the
    /// listener; the device keeps only a weak handle to it. A batch other than the one `execute` has is ignored.
    pub fn record_results_source(
        &self,
        batch: &BatchRef,
        listener: Weak<dyn crate::listeners::AndroidTestRunListener>,
    ) {
        if let Some(r) = lock(&self.inner.batch).as_mut()
            && Arc::ptr_eq(&r.batch, batch)
        {
            r.results = Some(listener);
        }
    }
    /// D70: an instrumentation of `batch` is about to be submitted, or the batch to be accounted for without one. From
    /// now on the core cannot take it back uncharged. Refused, as a cancellation, once it has taken it back. A batch run
    /// other than through [`Device::execute`], as by a test, is not tracked and passes.
    pub fn record_submission(&self, batch: &BatchRef) -> VendorResult<()> {
        let mut current = lock(&self.inner.batch);
        self.ensure_running()?;
        match current.as_mut() {
            Some(r) if Arc::ptr_eq(&r.batch, batch) => match r.stage {
                Stage::Withdrawn => Err(VendorError::Cancelled(requested("batch given back before it started"))),
                _ => {
                    r.stage = Stage::Started;
                    Ok(())
                }
            },
            _ => Ok(()),
        }
    }
    /// D22, D41 (U166): the next instrumentation of `batch` waits, as the batch's first did in [`Device::execute`], while the
    /// adb server is away or the device is in its grace period, and a device given up waits for the core to stop it.
    /// Returns at once when the way is clear, so a healthy run is unchanged. While it waits after an earlier
    /// instrumentation ran, the core can end the batch with what those gave, from `so_far`, if the device goes.
    pub async fn wait_for_next_instrumentation(
        &self,
        batch: &BatchRef,
        so_far: impl FnOnce() -> VendorResult<Option<TestBatchResults>>,
        context: &VendorContext,
    ) -> VendorResult<()> {
        if !self.is_gate_closed() && !self.inner.lost.load(Ordering::Acquire) {
            // U197: a batch the core ended while it ran sends nothing more, not even the next one's device commands.
            return self.ensure_not_withdrawn(batch);
        }
        let started = lock(&self.inner.batch)
            .as_ref()
            .is_some_and(|r| Arc::ptr_eq(&r.batch, batch) && matches!(r.stage, Stage::Started));
        if started && let Some(results) = so_far()? {
            match lock(&self.inner.batch).as_mut() {
                Some(r) if Arc::ptr_eq(&r.batch, batch) && matches!(r.stage, Stage::Started) => {
                    r.stage = Stage::Parked(results)
                }
                _ => {}
            }
        }
        self.log(
            Level::DEBUG,
            &format!(
                "Batch {} waits for its adb server or device before its next instrumentation",
                batch.id
            ),
            None,
        );
        let waited = self.pass_gate(context.cancellation.clone()).await;
        if let Some(r) = lock(&self.inner.batch).as_mut()
            && Arc::ptr_eq(&r.batch, batch)
            && matches!(r.stage, Stage::Parked(_))
        {
            r.stage = Stage::Started;
        }
        waited?;
        self.ensure_not_withdrawn(batch)
    }
    /// D22, D41: waits while the adb server is away or the device is in its grace period. D70: a device given up waits on
    /// until the core stops it and takes its batch back. The gate opens once its disconnect has been handled, and what
    /// started then would only fail on the lost device, charged to tests that never ran.
    async fn pass_gate(&self, signal: Arc<dyn CancellationSignal>) -> VendorResult<()> {
        cancellable(signal.clone(), async {
            self.inner.tracker.available(self.adb_serial()).await;
            Ok(())
        })
        .await?;
        if self.inner.lost.load(Ordering::Acquire) {
            return Err(VendorError::Cancelled(signal.cancelled().await));
        }
        Ok(())
    }
    /// Whether the core has taken `batch` back meanwhile, which ends it as a cancellation.
    /// Whether `batch` waits at the gate for its next instrumentation with what the earlier ones gave, so the core taking
    /// it back now ends it with those results. For tests, which can see a runner reach the gate before this is recorded.
    #[doc(hidden)]
    pub fn is_parked(&self, batch: &BatchRef) -> bool {
        lock(&self.inner.batch)
            .as_ref()
            .is_some_and(|r| Arc::ptr_eq(&r.batch, batch) && matches!(r.stage, Stage::Parked(_)))
    }
    fn ensure_not_withdrawn(&self, batch: &BatchRef) -> VendorResult<()> {
        match lock(&self.inner.batch).as_ref() {
            Some(r) if Arc::ptr_eq(&r.batch, batch) && matches!(r.stage, Stage::Withdrawn) => {
                Err(VendorError::Cancelled(requested("batch given back before it started")))
            }
            _ => Ok(()),
        }
    }
    async fn stop_logcat(&self) {
        let task = lock(&self.inner.logcat).take();
        if let Some(task) = task {
            task.cancel(requested("logcat closed"));
            let _ = task.join().await;
        }
    }
    pub(crate) fn seal(&self) {
        let now = self.runtime().dependencies.clock.now_millis();
        self.inner
            .work
            .seal(now.saturating_add(cleanup::allowance(self.timeouts())));
    }
    pub(crate) async fn dispose_inner(&self, remove_reverse: bool) -> VendorResult<()> {
        self.start_cleanup(remove_reverse).join().await
    }
    pub(crate) fn start_cleanup(&self, remove_reverse: bool) -> Arc<dyn VendorTask> {
        self.seal();
        lock(&self.inner.work.cleanup)
            .get_or_insert_with(|| {
                let device = self.clone();
                self.runtime()
                    .dependencies
                    .io
                    .spawn(Box::pin(async move { device.finish_cleanup(remove_reverse).await }))
            })
            .clone()
    }
    async fn finish_cleanup(&self, remove_reverse: bool) -> VendorResult<()> {
        let context = self.cleanup_context();
        let deadline = self.inner.work.deadline().unwrap_or_else(|| context.clock.now_millis());
        if let Some(task) = lock(&self.inner.logcat).take() {
            task.cancel(requested("device disposed"));
        }
        let grace = deadline
            .saturating_sub(cleanup::FORCE_RESERVE_MILLIS)
            .saturating_sub(context.clock.now_millis());
        let forced = if grace > 0 {
            tokio::select! {
                biased;
                _ = self.inner.work.idle() => false,
                _ = context.clock.delay(grace, context.cancellation.clone()) => true,
            }
        } else {
            true
        };
        if forced {
            self.inner
                .work
                .force
                .cancel(requested("Android device cleanup grace ended"));
        }
        self.client().close();
        lock(&self.inner.line_listeners).clear();
        lock(&self.inner.logcat_listeners).clear();

        let left = deadline.saturating_sub(context.clock.now_millis());
        let joined = async {
            tokio::join!(
                self.inner.work.idle(),
                self.cleanup_remote(remove_reverse, context.clone())
            );
        };
        let expired = if left > 0 {
            tokio::select! {
                biased;
                _ = context.clock.delay(left, context.cancellation.clone()) => true,
                _ = joined => false,
            }
        } else {
            true
        };
        self.inner.cleanup_client.close();
        if expired {
            let error = other(
                FailureKind::Timeout,
                format!("Android cleanup could not be confirmed for {}", self.serial_number()),
            );
            self.log(Level::WARN, "Android cleanup deadline reached", Some(error.failure()));
            return Err(error);
        }
        Ok(())
    }
    async fn cleanup_remote(&self, remove_reverse: bool, context: VendorContext) {
        let reverse = async {
            if remove_reverse {
                self.remove_reverse_forwards().await;
            }
        };
        let marker = self.clear_preparation_marker(context.clone());
        let recording = async {
            if self.inner.screenrecord_active.load(Ordering::Acquire) {
                let _ = crate::artifact_listeners::video::stop_screen_record_for_cleanup(self, context.clone()).await;
            }
        };
        let profiling = async {
            if self.inner.perfetto_active.load(Ordering::Acquire) {
                let _ = self.cleanup_shell("killall perfetto", context.clone()).await;
            }
        };
        let _ = tokio::join!(reverse, marker, recording, profiling);
    }
    async fn remove_reverse_forwards(&self) {
        let rules = std::mem::take(&mut *lock(&self.inner.reverse));
        for local in rules {
            if let Err(e) = self
                .inner
                .cleanup_client
                .execute(RemoveReversePortForwardRequest { local }, Some(self.adb_serial()))
                .await
            {
                self.log(
                    Level::WARN,
                    "Unable to remove reverse port forward",
                    Some(adb_error(e).failure()),
                );
            }
        }
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
    fn component(&self) -> &Component {
        &self.inner.logger
    }
    fn preparation_time_left_millis(&self, configuration: &marathon_config::Configuration) -> Option<i64> {
        let now = self.runtime().dependencies.clock.now_millis();
        if self.inner.preparation_continued.swap(false, Ordering::AcqRel) {
            let deadline = self.inner.preparation_deadline.load(Ordering::Acquire);
            return Some(deadline.saturating_sub(now).max(0));
        }
        let budget = preparation_budget_millis(
            configuration,
            crate::installer::preparation_envelope_millis(self.runtime()),
        );
        self.inner
            .preparation_deadline
            .store(now.saturating_add(budget).min(i64::MAX - 1), Ordering::Release);
        Some(budget)
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
                    let _work = device.admit()?;
                    let context = VendorContext {
                        cancellation: device.force_with(signal),
                        ..context
                    };
                    let start = context
                        .hooks
                        .instant("core.Track.trackDevicePreparing")
                        .map_err(|e| other(FailureKind::ReplayClock, e.message))?;
                    let mut dependencies = device.runtime().dependencies.clone();
                    dependencies.configuration = configuration;
                    let installation = AndroidRuntime::new(dependencies, device.runtime().executor.clone()).await?;
                    let deadline = device.inner.preparation_deadline.load(Ordering::Acquire);
                    crate::installer::AndroidAppInstaller::new(installation)
                        .with_deadline((deadline != i64::MAX).then_some(deadline))
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
                        .map_err(|e| other(FailureKind::ReplayClock, e.message))?;
                    device
                        .runtime()
                        .dependencies
                        .tracker
                        .device_preparing(&device.serial_number(), start, end)
                        .await?;
                    device.record_preparation(&context).await
                })
            },
        ))
    }
    fn prepare_exhausted(&self) {
        let clock = &self.runtime().dependencies.clock;
        if self.inner.prepare_failed_at.set(clock.now_millis()).is_ok() {
            self.inner.tracker.prepare_failed.notify_one();
        }
    }
    fn execute<'a>(
        &'a self,
        configuration: Arc<marathon_config::Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        let device = self.clone();
        Box::pin(call_vendor(
            self.runtime().dependencies.io.as_ref(),
            context.cancellation.clone(),
            move |signal| {
                Box::pin(async move {
                    let _work = device.admit()?;
                    let signal = device.force_with(signal);
                    if !device.take_batch(&batch, &result) {
                        return Err(VendorError::Cancelled(requested("batch given back before it started")));
                    }
                    // D22, D41: during an outage or a grace period a batch waits, whatever comes of it.
                    device.pass_gate(signal.clone()).await?;
                    device.ensure_not_withdrawn(&batch)?;
                    let blips = device.inner.tracker.blips(device.adb_serial());
                    let context = VendorContext {
                        cancellation: signal.clone(),
                        ..context
                    };
                    let outcome = device
                        .runtime()
                        .executor
                        .execute(&device, configuration, pool, batch.clone(), result, context.clone())
                        .await;
                    device.end_batch(&batch);
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
                                Some(LOST_BRIEFLY)
                            } else {
                                match device.loss_cause(signal).await {
                                    LossCause::Passing(why) => Some(why),
                                    // D49: the tracker sees no change here, so a device retired now would stay out of
                                    // the run however healthy it is.
                                    LossCause::Listed if device.survives_request_loss(&context).await? => {
                                        Some("a request was lost, while the device is listed and answers")
                                    }
                                    LossCause::Listed | LossCause::Unlisted(_) => None,
                                }
                            };
                            match passing {
                                // x77: a grace that ended in a disconnect wakes this batch only after the device has
                                // been dropped, so the device is not kept, whatever the reason was.
                                Some(why) if device.is_disconnected() => {
                                    let cause = if why == LOST_BRIEFLY {
                                        "the device lost its connection and did not return in time"
                                    } else {
                                        "the device was lost"
                                    };
                                    device.log(
                                        Level::WARN,
                                        &format!(
                                            "Batch on {} failed because {cause}; it has been disconnected",
                                            device.serial_number()
                                        ),
                                        Some(&failure),
                                    );
                                    Err(VendorError::BatchExecution(failure))
                                }
                                Some(why) => {
                                    device.log(
                                        Level::WARN,
                                        &format!(
                                            "Batch on {} failed because {why}; keeping the device",
                                            device.serial_number()
                                        ),
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
            move |_| Box::pin(async move { device.dispose_inner(true).await }),
        ))
    }
    fn begin_stop(&self) -> Option<VendorFuture<'static, ()>> {
        self.seal();
        let device = self.clone();
        Some(Box::pin(async move { device.dispose_inner(true).await }))
    }
    fn cancellation_cleanup_millis(&self) -> Option<i64> {
        Some(cleanup::allowance(self.timeouts()))
    }
    fn join_cancelled_work(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if self.stopping() {
                self.dispose_inner(true).await?;
            }
            Ok(())
        })
    }
    fn return_wait_used_millis(&self) -> i64 {
        self.inner.return_wait_used.load(Ordering::Acquire)
    }
    /// D70: a batch is given back uncharged only if no instrumentation of it was submitted, which is recorded as it is
    /// done: the absence of test callbacks would not prove that none started. One parked at the gate after earlier
    /// instrumentations ran ends with what they gave, its remaining tests uncompleted, as when the device is lost there:
    /// giving back the whole batch would run those again.
    ///
    /// U197: one leaving while an instrumentation runs, or between instrumentations away from the gate, ends the same way,
    /// with what its listener has so far: the test in progress and those not yet run are uncompleted. That is what the
    /// runner reports when it sees the loss first; returning the whole batch would run its finished tests again. Without
    /// a listener, or if it has no results, the batch is accounted for as before.
    fn withdraw_unstarted(&self, batch: &BatchRef) -> bool {
        let source = {
            let mut record = lock(&self.inner.batch);
            match record.as_mut() {
                Some(r) if Arc::ptr_eq(&r.batch, batch) => match std::mem::replace(&mut r.stage, Stage::Withdrawn) {
                    Stage::Unstarted | Stage::Withdrawn => return true,
                    Stage::Parked(results) => {
                        if let Some(result) = r.result.upgrade() {
                            result.complete(Ok(results));
                        }
                        return false;
                    }
                    Stage::Started => {
                        r.stage = Stage::Started;
                        r.results.clone()
                    }
                    Stage::Done => {
                        r.stage = Stage::Done;
                        return false;
                    }
                },
                // `execute` has not got this batch yet; it is the one the core runs, so the device has no later one.
                _ => {
                    *record = Some(BatchRecord {
                        batch: batch.clone(),
                        result: Weak::new(),
                        results: None,
                        stage: Stage::Withdrawn,
                    });
                    return true;
                }
            }
        };
        // Outside the record's lock: the listener takes its own. Synchronous and without I/O, as on the core thread.
        let snapshot = match source
            .and_then(|listener| listener.upgrade())
            .map(|l| l.results_so_far())
        {
            Some(Ok(Some(results))) => results,
            Some(Err(error)) => {
                self.log(
                    Level::DEBUG,
                    &format!("Batch {} has no results so far; accounted for as before", batch.id),
                    Some(error.failure()),
                );
                return false;
            }
            _ => return false,
        };
        let mut record = lock(&self.inner.batch);
        if let Some(r) = record.as_mut()
            && Arc::ptr_eq(&r.batch, batch)
            && matches!(r.stage, Stage::Started | Stage::Parked(_))
        {
            // From here nothing more of the batch is sent; the runner's own result, when it comes, loses to this one.
            r.stage = Stage::Withdrawn;
            if let Some(result) = r.result.upgrade() {
                result.complete(Ok(snapshot));
            }
        }
        false
    }
}
impl LogProducer for AndroidDevice {
    fn add_line_listener(&self, listener: Arc<dyn LineListener>) -> VendorResult<()> {
        let mut listeners = lock(&self.inner.line_listeners);
        self.ensure_running()?;
        listeners.push(listener);
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
        let mut listeners = lock(&self.inner.logcat_listeners);
        if !self.stopping() {
            listeners.push(listener);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_positive_boot_or_marker_evidence_replaces_a_prepared_device_d85() {
        let boot = uuid::uuid!("00000000-0000-4000-8000-000000000001");
        let token = uuid::uuid!("00000000-0000-4000-8000-000000000002");
        let changed = uuid::uuid!("00000000-0000-4000-8000-000000000003");
        for (reply, lost) in [
            (format!("boot {boot}\nmarker value={token}\n"), false),
            (format!("boot {boot}\nmarker missing\n"), true),
            (format!("boot {boot}\nmarker value={changed}\n"), true),
            (format!("boot {changed}\nmarker unknown\n"), true),
            (format!("boot {boot}\nmarker unknown\n"), false),
            (format!("boot {boot}\nmarker value=missing\n"), false),
            (format!("boot {boot}\nmarker value=\n"), false),
            (format!("boot {changed}\n"), false),
            ("boot garbage\nmarker garbage\n".into(), false),
            ("boot unknown\nmarker missing\n".into(), true),
            ("boot unknown\nmarker missing\nextra\n".into(), false),
        ] {
            assert_eq!(setup_was_lost(&reply, Some(boot), token), lost, "{reply:?}");
        }
    }
}
