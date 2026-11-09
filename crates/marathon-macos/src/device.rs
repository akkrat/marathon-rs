//! Port of vendor/vendor-apple/macos/src/main/kotlin/com/malinskiy/marathon/apple/macos/MacosDevice.kt

use crate::installer::MacosApplicationInstaller;
use jcompat::{
    file_manager::FileManager,
    jtime::Duration,
    logging::{Level, Logger},
};
use marathon_apple::{
    apple_application_installer::AppleApplicationInstaller,
    apple_device::{AppleDevice, storage_path_for},
    apple_device_test_runner::{AppleDeviceTestRunner, RunError},
    bin::AppleBinaryEnvironment,
    cmd::{
        CommandResult, failure, remote::ssh::sshj::config::SshLogging, retained_tasks::RetainedTasks,
        shared_transfers::SharedTransfers,
    },
    device::connection_factory::{Bridge, Executor},
    execute_test_request::{LineListeners, TestOutput, execute_test_request},
    listener::{
        AppleTestRunListener, CompositeTestRunListener, DebugTestRunListener, DiagnosticLogsPathFinder,
        ResultBundleRunListener, SessionResultsPathFinder, TestResultsListener, TestResultsListenerConfig,
        TestRunListenerAdapter,
    },
    logparser::parser::{DeviceFailureReason, debug_log_printer::DebugLogPrinter},
    model::{AppleTestBundle, Sdk, XcodeVersion},
    remote_file_manager::RemoteFileManager,
    test::TestRequest,
    worker_commands::{ExecutorWorkerCommands, WorkerCommands},
    xcodebuild_session::{EventReceiver, XcodebuildSession},
};
use marathon_config::{
    Configuration, VendorConfiguration,
    devices::Transport,
    vendor::{MacosConfiguration, MacosPermission},
};
use marathon_core::{
    artifacts::{FileLogWriter, LogListener},
    error::{Failure, VendorError, VendorResult},
    executor::shared::{SharedDeferred, Signal},
    listener::{LineListener, LogProducer, LogTarget},
    model::{BatchRef, DeviceInfo, DevicePoolId, NetworkState, OperatingSystem, TestBatchResults},
    runtime::{CancellationSignal, Deferred, VendorFuture},
    screenshot::{RgbaFrame, Rotation, Screenshottable},
    transfer::TransferMeasurement,
    vendor::{Device, DeviceRef, VendorContext},
    wiring::VendorDependencies,
};
use std::path::PathBuf;
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex, Weak},
};

pub const CANCELLED_BATCH_GRACE: Duration = Duration::of_seconds(10);
pub const CANCELLED_BATCH_TIMEOUT: Duration = Duration::of_seconds(70);

#[derive(Clone)]
pub struct MacosDeviceOptions {
    pub dependencies: VendorDependencies,
    pub vendor: Arc<MacosConfiguration>,
    pub logging: SshLogging,
    pub files: Arc<FileManager>,
    pub shared_transfers: Arc<SharedTransfers>,
    pub installer: Arc<MacosApplicationInstaller>,
    pub session_close_timeout: Duration,
    pub cancelled_batch_timeout: Duration,
}
struct Facts {
    info: DeviceInfo,
    abi: String,
    env: HashMap<String, String>,
    xcode: Option<XcodeVersion>,
    bundle: Option<Arc<AppleTestBundle>>,
}
#[derive(Default)]
struct Execution {
    running: Option<Arc<BatchJob>>,
    disposal: Option<Arc<SharedDeferred<()>>>,
}
struct BatchJob {
    result: Arc<SharedDeferred<()>>,
    cancellation: Arc<Signal>,
}
impl BatchJob {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            result: SharedDeferred::new(),
            cancellation: Signal::new(),
        })
    }
}
pub struct MacosDevice {
    identity: Weak<Self>,
    udid: String,
    serial: String,
    transport: Transport,
    storage: String,
    binary: AppleBinaryEnvironment,
    bridge: Bridge,
    commands: Arc<ExecutorWorkerCommands>,
    remote: RemoteFileManager,
    logger: Logger,
    facts: Mutex<Facts>,
    execution: Mutex<Execution>,
    listeners: Arc<LineListeners>,
    sessions: Mutex<Vec<Arc<XcodebuildSession>>>,
    tasks: RetainedTasks,
    pub options: MacosDeviceOptions,
}
impl MacosDevice {
    pub fn new(
        udid: String,
        transport: Transport,
        binary: AppleBinaryEnvironment,
        bridge: Bridge,
        options: MacosDeviceOptions,
    ) -> Arc<Self> {
        let serial: String = format!("{udid}@{}", binary.command_executor.host().id);
        let storage = storage_path_for(&udid);
        let logger = options.logging.logger("com.malinskiy.marathon.apple.macos.MacosDevice");
        let commands = Arc::new(ExecutorWorkerCommands {
            executor: binary.command_executor.clone(),
            timeouts: Arc::new(options.vendor.timeout_configuration.clone()),
        });
        let remote = RemoteFileManager::new(storage.clone(), udid.clone(), commands.clone(), options.logging.clone());
        let device = Arc::new_cyclic(|identity| Self {
            identity: identity.clone(),
            udid,
            transport,
            storage,
            binary,
            bridge,
            commands,
            remote,
            logger,
            facts: Mutex::new(Facts {
                info: DeviceInfo {
                    operating_system: OperatingSystem {
                        version: "Unknown".into(),
                    },
                    serial_number: serial.clone(),
                    model: "Unknown".into(),
                    manufacturer: "Unknown".into(),
                    network_state: NetworkState::Connected,
                    device_features: vec![],
                    healthy: true,
                },
                abi: "Unknown".into(),
                env: HashMap::new(),
                xcode: None,
                bundle: None,
            }),
            serial,
            execution: Mutex::default(),
            listeners: Arc::default(),
            sessions: Mutex::default(),
            tasks: RetainedTasks::default(),
            options,
        });
        device.options.installer.register(&device);
        device
    }
    fn retained(&self) -> VendorResult<Arc<Self>> {
        self.identity.upgrade().ok_or_else(|| {
            VendorError::Other(failure(
                "java.lang.IllegalStateException",
                "macOS device identity expired",
            ))
        })
    }
    pub fn executor(&self) -> Executor {
        self.binary.command_executor.clone()
    }
    fn log(&self, level: Level, message: String, cause: Option<Failure>) {
        if self.options.logging.enabled(&self.logger, level) {
            (self.options.logging.sink)(&self.logger, level, message, cause);
        }
    }
    fn context(&self) -> VendorContext {
        VendorContext {
            cancellation: Signal::new(),
            clock: self.options.dependencies.clock.clone(),
            hooks: self.options.dependencies.hooks.clone(),
        }
    }
    async fn prepare_inner(&self, context: &VendorContext) -> VendorResult<()> {
        self.remote.remove_remote_directory(&[], context).await?;
        self.remote.create_remote_directory(None, context).await?;
        self.remote.create_remote_shared_directory(context).await?;
        let vendor = VendorConfiguration::Macos((*self.options.vendor).clone());
        AppleApplicationInstaller::new(
            &vendor,
            self.options.dependencies.io.clone(),
            self.options.logging.clone(),
        )
        .with_code_coverage(self.options.dependencies.configuration.is_code_coverage_enabled)
        .with_after_install(&*self.options.installer)
        .prepare_installation(self, false, context)
        .await
        .map_err(|e| e.into_vendor())?;
        let bundle = self.options.vendor.bundle.as_ref().ok_or_else(|| {
            VendorError::Other(failure("java.lang.IllegalArgumentException", "No test bundle provided"))
        })?;
        let xctest = bundle.xctest.as_ref().ok_or_else(|| {
            VendorError::Other(failure("java.lang.IllegalArgumentException", "No test bundle provided"))
        })?;
        let bundle = AppleTestBundle::new(
            bundle.app.clone(),
            bundle.test_app.clone(),
            xctest.clone(),
            Sdk::Macos,
            self.options.dependencies.io.clone(),
            self.options.logging.clone(),
        )
        .await?;
        self.facts.lock().unwrap_or_else(|e| e.into_inner()).bundle = Some(Arc::new(bundle));
        Ok(())
    }
    async fn run(
        &self,
        configuration: Arc<Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<dyn Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorResult<()> {
        let (listener, lines) = self.execution_listeners(pool, batch.clone(), result, context.clone())?;
        for line in &lines {
            self.listeners.add(line.clone());
        }
        let outcome = async {
            let bundle = self
                .facts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .bundle
                .clone()
                .ok_or_else(|| {
                    VendorError::Other(failure(
                        "kotlin.UninitializedPropertyAccessException",
                        "lateinit property testBundle has not been initialized",
                    ))
                })?;
            let mut runner = AppleDeviceTestRunner::new(
                self.retained()?,
                self.options.dependencies.io.clone(),
                self.options.logging.clone(),
            );
            runner.session_close_timeout = self.options.session_close_timeout;
            runner
                .execute(&configuration, &bundle, &batch, &listener, &context)
                .await
                .map_err(classify_run)
        }
        .await;
        for line in &lines {
            self.listeners.remove(line);
        }
        outcome
    }
    fn execution_listeners(
        &self,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<dyn Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorResult<(CompositeTestRunListener, Vec<Arc<dyn LineListener>>)> {
        let device = self.retained()?;
        let deps = &self.options.dependencies;
        let log = LogListener::new(
            deps.io.clone(),
            device.clone(),
            Arc::new(FileLogWriter::new(deps.io.clone(), deps.files.clone())),
            LogTarget {
                pool: pool.clone(),
                batch_id: batch.id.clone(),
                device: self.info(),
                test: None,
            },
            Some("xcodebuild-log".into()),
        );
        let diagnostic = Arc::new(DiagnosticLogsPathFinder::new(self.options.logging.clone()));
        let session = Arc::new(SessionResultsPathFinder::new(self.options.logging.clone()));
        let debug = Arc::new(DebugLogPrinter::new(
            None,
            self.options.vendor.hide_runner_output,
            self.options.logging.clone(),
        ));
        let results = TestResultsListener::new(TestResultsListenerConfig {
            batch: batch.clone(),
            device: DeviceRef(device.clone()),
            deferred: result,
            timer: deps.timer.clone(),
            wall: deps.wall.clone(),
            logs: deps.logs.clone(),
            remote_xcresult: self.remote.remote_xcresult_file(&batch).to_owned(),
            xcresulttool: Arc::new(marathon_apple::bin::xcrun::xcresulttool::Xcresulttool::new(
                marathon_apple::bin::Tools {
                    executor: self.executor(),
                    timeouts: Arc::new(self.options.vendor.timeout_configuration.clone()),
                    runtime: deps.io.clone(),
                    logging: self.options.logging.clone(),
                },
            )),
            xcode_version: self.facts.lock().unwrap_or_else(|e| e.into_inner()).xcode,
            context: context.clone(),
            logging: self.options.logging.clone(),
            attachment_providers: vec![log.clone()],
        })?;
        struct Recorder;
        impl AppleTestRunListener for Recorder {}
        let listeners: Vec<Arc<dyn AppleTestRunListener>> = vec![
            Arc::new(results),
            Arc::new(TestRunListenerAdapter { listener: log }),
            Arc::new(DebugTestRunListener::new(
                DeviceRef(device.clone()),
                self.options.logging.clone(),
                context.hooks.clone(),
            )),
            diagnostic.clone(),
            session.clone(),
            Arc::new(Recorder),
            Arc::new(ResultBundleRunListener::new(
                device,
                self.options.vendor.xcresult.clone(),
                pool,
                batch,
                self.options.files.clone(),
                None,
                context,
                self.options.logging.clone(),
            )),
        ];
        Ok((CompositeTestRunListener { listeners }, vec![diagnostic, session, debug]))
    }
    async fn join_cancelled(&self, batch: &BatchJob) -> VendorResult<()> {
        batch
            .cancellation
            .cancel(marathon_core::error::Cancellation::Requested(failure(
                "kotlinx.coroutines.JobCancellationException",
                "LazyDeferredCoroutine was cancelled",
            )));
        let context = self.context();
        let finished = context
            .clock
            .with_timeout_or_null(
                context.cancellation.clone(),
                self.options
                    .cancelled_batch_timeout
                    .to_millis()
                    .ok_or_else(|| VendorError::Other(failure("java.lang.ArithmeticException", "long overflow")))?,
                Box::new(|signal| {
                    Box::pin(async move {
                        tokio::select! { biased;
                            reason = signal.cancelled() => Err(VendorError::Cancelled(reason)),
                            _ = batch.result.wait() => Ok(()),
                        }
                    })
                }),
            )
            .await?
            .is_some();
        if !finished {
            self.log(
                Level::Warn,
                format!(
                    "Cancelled batch on {} has not finished within {}, not waiting for it any longer",
                    self.serial.to_owned(),
                    self.options.cancelled_batch_timeout
                ),
                None,
            );
        }
        Ok(())
    }
    async fn dispose_inner(&self) -> VendorResult<()> {
        let (cell, running, first) = {
            let mut state = self.execution.lock().unwrap_or_else(|e| e.into_inner());
            match &state.disposal {
                Some(cell) => (cell.clone(), None, false),
                None => {
                    let cell = SharedDeferred::new();
                    state.disposal = Some(cell.clone());
                    (cell, state.running.clone(), true)
                }
            }
        };
        if first {
            let device = self.retained()?;
            let outcome_cell = SharedDeferred::new();
            let first_complete = outcome_cell.clone();
            let complete = cell.clone();
            self.tasks.spawn(&*self.options.dependencies.io, async move {
                let outcome = if let Some(batch) = running {
                    device.join_cancelled(&batch).await
                } else {
                    Ok(())
                };
                // Kotlin completes the shared disposal cell with Unit in finally.
                complete.complete(Ok(()));
                first_complete.complete(outcome);
                Ok(())
            });
            return outcome_cell.wait().await.map(|_| ());
        }
        cell.wait().await.map(|_| ())
    }
    /// Explicit owner drain for tests/final runtime shutdown, never part of bounded disposal.
    pub async fn join_pending(&self) -> VendorResult<()> {
        let mut outcome = Ok(());
        for task in self.tasks.take() {
            let joined = task.join().await;
            outcome = outcome.and(joined);
        }
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for session in sessions {
            let joined = session.join_producer().await;
            outcome = outcome.and(joined);
        }
        outcome
    }
    pub async fn grant(
        &self,
        client: &str,
        permissions: &[MacosPermission],
        context: &VendorContext,
    ) -> VendorResult<()> {
        let (version, user) = {
            let facts = self.facts.lock().unwrap_or_else(|e| e.into_inner());
            (
                facts.info.operating_system.version.clone(),
                facts.env.get("USER").cloned().unwrap_or_else(|| "null".into()),
            )
        };
        let major = version.split('.').next().and_then(jcompat::jnum::parse_int);
        if !major.is_some_and(|n| n > 10) {
            return Err(VendorError::IncompatibleDevice(failure(
                "com.malinskiy.marathon.exceptions.IncompatibleDeviceException",
                "Modifying permissions is supported to macOS 10+",
            )));
        }
        // P4: malformed client identifiers are rejected before constructing SQL.
        if client.chars().any(|c| c == '"' || c.is_control()) {
            return Err(VendorError::Other(failure(
                "java.lang.IllegalArgumentException",
                "Permission client contains a double quote or control character",
            )));
        }
        let epoch = context
            .hooks
            .millis("macos.MacosDevice.grant")
            .map_err(crate::clock_error)?
            / 1000;
        for permission in permissions {
            let query = format!(
                "replace into access (service,client,client_type,auth_value,auth_reason,auth_version,indirect_object_identifier,flags,last_modified) values (\"{}\",\"{client}\",0,2,1,1,\"UNUSED\",0,{epoch});",
                permission.value()
            );
            self.binary
                .sqlite3
                .query(
                    "/Library/Application Support/com.apple.TCC/TCC.db",
                    &query,
                    true,
                    context,
                )
                .await
                .map_err(classify_setup)?;
            self.binary
                .sqlite3
                .query(
                    &format!("/Users/{user}/Library/Application Support/com.apple.TCC/TCC.db"),
                    &query,
                    true,
                    context,
                )
                .await
                .map_err(classify_setup)?;
        }
        Ok(())
    }
    /// D54: a send to this Mac ends on its caller's cancellation or when the Mac's own connection is lost.
    fn send_stop(&self, context: &VendorContext) -> marathon_apple::cmd::TransferStop {
        marathon_apple::cmd::send_stop(self.binary.command_executor.as_ref(), context)
    }
    /// D54: a receive from it ends only when that connection is lost.
    fn receive_stop(&self) -> marathon_apple::cmd::TransferStop {
        marathon_apple::cmd::receive_stop(self.binary.command_executor.as_ref())
    }
    fn measurement(&self) -> TransferMeasurement {
        let logging = self.options.logging.clone();
        TransferMeasurement {
            io: self.options.dependencies.io.clone(),
            clock: self.options.dependencies.hooks.clone(),
            logger: self.logger.clone(),
            serial: self.serial.clone(),
            log: Arc::new(move |logger, level, message, cause| {
                if logging.enabled(logger, level) {
                    (logging.sink)(logger, level, message.to_owned(), cause.cloned());
                }
            }),
        }
    }
}
fn lost(cause: Failure) -> VendorError {
    let rendered = if cause.class == marathon_apple::cmd::remote::ssh::sshj::error::OPEN_FAIL_EXCEPTION {
        marathon_apple::cmd::remote::ssh::sshj::SshFailure::new(
            marathon_apple::cmd::remote::ssh::sshj::error::OPEN_FAIL_EXCEPTION,
            cause.message.as_ref().map_or_else(|| "null".into(), String::clone),
        )
        .java_to_string()
    } else if cause.class == "kotlinx.coroutines.JobCancellationException"
        && cause.message == Some("LazyDeferredCoroutine was cancelled".into())
    {
        let object = jcompat::writers::KotlinValue::object("kotlinx.coroutines.LazyDeferredCoroutine")
            .render()
            .to_owned();
        let identity = object.rsplit_once('@').map_or("", |(_, hash)| hash);
        format!(
            "{}; job=LazyDeferredCoroutine{{Cancelled}}@{identity}",
            cause.to_java_string().to_owned()
        )
    } else {
        cause.to_java_string().to_owned()
    };
    let mut detail = failure("com.malinskiy.marathon.exceptions.DeviceLostException", rendered);
    detail.cause = Some(Box::new(cause));
    VendorError::DeviceLost(detail)
}
fn classify_setup(error: VendorError) -> VendorError {
    match error {
        VendorError::Other(f) if f.class == "com.malinskiy.marathon.exceptions.DeviceSetupException" => {
            VendorError::DeviceSetup(f)
        }
        other => other,
    }
}
fn classify_run(run: RunError) -> VendorError {
    let reason = run.reason;
    let error = run.into_vendor();
    match reason {
        Some(DeviceFailureReason::IncompatibleDevice) => {
            let mut f = failure(
                "com.malinskiy.marathon.exceptions.IncompatibleDeviceException",
                error.failure().to_java_string(),
            );
            f.cause = Some(Box::new(error.failure().clone()));
            VendorError::IncompatibleDevice(f)
        }
        Some(_) => lost(error.failure().clone()),
        None => classify_execute(error),
    }
}
fn classify_execute(error: VendorError) -> VendorError {
    if matches!(error, VendorError::Cancelled(_)) {
        return error;
    }
    use marathon_apple::cmd::remote::ssh::sshj::error::{
        CONNECTION_EXCEPTION, OPEN_FAIL_EXCEPTION, TRANSPORT_EXCEPTION, is_instance,
    };
    let class = error.failure().class.to_owned();
    if class == "java.lang.IllegalStateException"
        || [CONNECTION_EXCEPTION, TRANSPORT_EXCEPTION, OPEN_FAIL_EXCEPTION]
            .iter()
            .any(|base| is_instance(&class, base))
    {
        lost(error.failure().clone())
    } else {
        error
    }
}
fn unsupported<T>() -> VendorResult<T> {
    Err(VendorError::Other(failure(
        "kotlin.NotImplementedError",
        "An operation is not implemented: Not yet implemented",
    )))
}
impl Device for MacosDevice {
    fn info(&self) -> DeviceInfo {
        self.facts.lock().unwrap_or_else(|e| e.into_inner()).info.clone()
    }
    fn abi(&self) -> String {
        self.facts.lock().unwrap_or_else(|e| e.into_inner()).abi.clone()
    }
    fn worker_id(&self) -> Option<String> {
        Some(self.transport.id())
    }
    fn logger(&self) -> &Logger {
        &self.logger
    }
    fn prepare<'a>(&'a self, _: Arc<Configuration>, context: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let device = self.retained()?;
            let own_context = self.context();
            let task = self.tasks.spawn(&*self.options.dependencies.io, async move {
                let started = own_context
                    .hooks
                    .instant("core.Track.trackDevicePreparing")
                    .map_err(crate::clock_error)?;
                device.prepare_inner(&own_context).await?;
                let finish = own_context
                    .hooks
                    .instant("core.Track.trackDevicePreparing")
                    .map_err(crate::clock_error)?;
                device
                    .options
                    .dependencies
                    .tracker
                    .device_preparing(&device.serial, started, finish)
                    .await
            });
            tokio::select! { biased;
                reason = context.cancellation.cancelled() => Err(VendorError::Cancelled(reason)),
                result = task.join() => result,
            }
        })
    }
    fn execute<'a>(
        &'a self,
        configuration: Arc<Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<dyn Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let device = self.retained()?;
            let job = BatchJob::new();
            {
                let mut execution = self.execution.lock().unwrap_or_else(|e| e.into_inner());
                if execution.disposal.is_some() {
                    return Err(lost(failure(
                        "java.lang.IllegalStateException",
                        format!("Device {} has been disposed", self.serial.to_owned()),
                    )));
                }
                execution.running = Some(job.clone());
            }
            let running = job.clone();
            let mut own_context = context.clone();
            own_context.cancellation = job.cancellation.clone();
            self.tasks.spawn(&*self.options.dependencies.io, async move {
                let outcome = if let Some(reason) = running.cancellation.reason() {
                    Err(VendorError::Cancelled(reason))
                } else {
                    device.run(configuration, pool, batch, result, own_context).await
                };
                {
                    let mut execution = device.execution.lock().unwrap_or_else(|e| e.into_inner());
                    if execution.running.as_ref().is_some_and(|j| Arc::ptr_eq(j, &running)) {
                        execution.running = None;
                    }
                }
                running.result.complete(outcome);
                Ok(())
            });
            match job.result.wait_in(context.cancellation.clone()).await {
                Ok(_) => Ok(()),
                Err(VendorError::Cancelled(reason)) => {
                    self.join_cancelled(&job).await?;
                    if context.cancellation.reason().is_none() {
                        Err(lost(reason.failure().clone()))
                    } else {
                        Err(VendorError::Cancelled(reason))
                    }
                }
                Err(error) => Err(classify_execute(error)),
            }
        })
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if let Err(error) = self.dispose_inner().await {
                if jcompat::throwable::is_error_class(&error.failure().class.to_owned()) {
                    return Err(error);
                }
                self.log(
                    Level::Debug,
                    format!("Error disposing {}", self.serial.to_owned()),
                    Some(error.failure().clone()),
                );
            }
            Ok(())
        })
    }
    /// D48: the provider connects each worker's Mac once, at the start, so nothing can bring a lost one back.
    fn return_wait_used_millis(&self) -> i64 {
        i64::MAX
    }
}
impl AppleDevice for MacosDevice {
    fn udid(&self) -> &str {
        &self.udid
    }
    fn remote_file_manager(&self) -> &RemoteFileManager {
        &self.remote
    }
    fn storage_path(&self) -> &str {
        &self.storage
    }
    fn transport(&self) -> &Transport {
        &self.transport
    }
    fn sdk(&self) -> Sdk {
        Sdk::Macos
    }
    fn binary_environment(&self) -> &AppleBinaryEnvironment {
        &self.binary
    }
    fn setup<'a>(&'a self, context: &'a VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let result = self.execute_worker_command(vec!["env".into()], context).await?;
            let result = result.filter(CommandResult::successful).ok_or_else(|| {
                VendorError::DeviceSetup(failure(
                    "com.malinskiy.marathon.exceptions.DeviceSetupException",
                    format!("macOS {}: unable to detect environment variables", self.udid),
                ))
            })?;
            let env: HashMap<_, _> = result
                .stdout
                .iter()
                .map(|s| marathon_apple::bin::kt::trim(s))
                .filter(|s| !s.is_empty())
                .map(|s| {
                    let (k, v) = s.split_once('=').unwrap_or((s, s));
                    (
                        marathon_apple::bin::kt::trim(k).to_owned(),
                        marathon_apple::bin::kt::trim(v).to_owned(),
                    )
                })
                .collect();
            self.facts.lock().unwrap_or_else(|e| e.into_inner()).env = env.clone();
            let xcode = self
                .binary
                .xcrun
                .xcodebuild
                .get_version(context)
                .await
                .map_err(classify_setup)?;
            self.facts.lock().unwrap_or_else(|e| e.into_inner()).xcode = Some(xcode);
            let home = env.get("HOME").map_or("", String::as_str);
            if marathon_apple::bin::kt::trim(home).is_empty() {
                return Err(VendorError::DeviceSetup(failure(
                    "com.malinskiy.marathon.exceptions.DeviceSetupException",
                    format!(
                        "macOS {}: invalid value {home} for environment variable HOME",
                        self.udid
                    ),
                )));
            }
            let model = self.binary.ioreg.get_model(context).await.map_err(classify_setup)?;
            self.facts.lock().unwrap_or_else(|e| e.into_inner()).info.model = model;
            let manufacturer = self
                .binary
                .ioreg
                .get_manufacturer(context)
                .await
                .map_err(classify_setup)?;
            self.facts.lock().unwrap_or_else(|e| e.into_inner()).info.manufacturer = manufacturer;
            let version = self.binary.swvers.get_version(context).await.map_err(classify_setup)?;
            self.facts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .info
                .operating_system
                .version = version;
            let abi = self
                .execute_worker_command(vec!["uname".into(), "-m".into()], context)
                .await?
                .filter(CommandResult::successful)
                .map(|r| marathon_apple::bin::kt::trim(&r.combined_stdout()).to_owned())
                .unwrap_or_else(|| "Unknown".into());
            // D36: on a lost connection this probe answers null, which reads as "use the default". A device
            // described that way must not register (A17).
            if !self.binary.command_executor.connected() {
                return Err(VendorError::Other(failure(
                    marathon_apple::cmd::remote::ssh::sshj::error::CONNECTION_EXCEPTION,
                    "Not connected",
                )));
            }
            self.facts.lock().unwrap_or_else(|e| e.into_inner()).abi = abi;
            Ok(())
        })
    }
    fn execute_test_request<'a>(
        &'a self,
        request: TestRequest,
        session: Arc<XcodebuildSession>,
        context: &'a VendorContext,
    ) -> VendorFuture<'a, EventReceiver> {
        Box::pin(async move {
            {
                let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(previous) = sessions.last() {
                    previous.check_closed(&self.serial.to_owned())?;
                }
                sessions.retain(|s| !s.closed());
                sessions.push(session.clone());
            }
            Ok(execute_test_request(
                self.retained()?,
                request,
                session,
                context.clone(),
                TestOutput {
                    label: format!("macOS {}", self.udid),
                    args: self
                        .options
                        .vendor
                        .xcodebuild_test_args
                        .iter()
                        .map(|(k, v)| (k.clone(), v.to_owned()))
                        .collect(),
                    timer: self.options.dependencies.timer.clone(),
                    listeners: self.listeners.clone(),
                    logging: self.options.logging.clone(),
                },
            ))
        })
    }
    fn execute_worker_command<'a>(
        &'a self,
        command: Vec<String>,
        context: &'a VendorContext,
    ) -> VendorFuture<'a, Option<CommandResult>> {
        self.commands.execute_worker_command(command, context)
    }
    fn push_file<'a>(&'a self, src: &'a Path, dst: &'a str, context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.measurement()
                .measure_file_transfer(
                    PathBuf::from(src.to_string_lossy().into_owned()),
                    self.bridge.send_with(src, dst, self.send_stop(context)),
                )
                .await
        })
    }
    fn pull_file<'a>(&'a self, src: &'a str, dst: &'a Path, _context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.measurement()
                .measure_file_transfer(
                    PathBuf::from(dst.to_string_lossy().into_owned()),
                    self.bridge.receive_with(src, dst, self.receive_stop()),
                )
                .await
        })
    }
    fn push_folder<'a>(&'a self, src: &'a Path, dst: &'a str, context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.measurement()
                .measure_folder_transfer(
                    PathBuf::from(src.to_string_lossy().into_owned()),
                    self.bridge.send_with(src, dst, self.send_stop(context)),
                )
                .await
        })
    }
    fn pull_folder<'a>(&'a self, src: &'a str, dst: &'a Path, _context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.measurement()
                .measure_folder_transfer(
                    PathBuf::from(dst.to_string_lossy().into_owned()),
                    self.bridge.receive_with(src, dst, self.receive_stop()),
                )
                .await
        })
    }
    fn push_shared<'a>(&'a self, src: &'a Path, dst: &'a str, context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.options
                .shared_transfers
                .send(
                    &self.transport.id().to_owned(),
                    src,
                    dst,
                    self.send_stop(context),
                    || self.remote.exists(dst, context),
                    || async {
                        if tokio::fs::metadata(src).await.is_ok_and(|m| m.is_dir()) {
                            self.push_folder(src, dst, context).await
                        } else {
                            self.push_file(src, dst, context).await
                        }
                    },
                )
                .await
        })
    }
    fn install<'a>(&'a self, _: &'a str, _: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    fn get_screenshot<'a>(&'a self, _: Duration, _: &'a Path, _: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async { unsupported() })
    }
    fn start_video_recording<'a>(
        &'a self,
        _: &'a str,
        _: &'a VendorContext,
    ) -> VendorFuture<'a, Option<CommandResult>> {
        Box::pin(async { unsupported() })
    }
    fn stop_video_recording<'a>(&'a self, _: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async { unsupported() })
    }
    fn shutdown<'a>(&'a self, _: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async { unsupported() })
    }
    fn erase<'a>(&'a self, _: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async { unsupported() })
    }
}
impl Screenshottable for MacosDevice {
    fn orientation(&self) -> VendorResult<Rotation> {
        Ok(Rotation::Rotation0)
    }
    fn screenshot<'a>(&'a self, _: Duration, _: Arc<dyn CancellationSignal>) -> VendorFuture<'a, Option<RgbaFrame>> {
        Box::pin(async { unsupported() })
    }
}
impl LogProducer for MacosDevice {
    fn add_line_listener(&self, listener: Arc<dyn LineListener>) -> VendorResult<()> {
        self.listeners.add(listener);
        Ok(())
    }
    fn remove_line_listener(&self, listener: &Arc<dyn LineListener>) -> VendorResult<()> {
        self.listeners.remove(listener);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    #[test]
    fn execute_error_classification_matches_jvm_chains() {
        for line in include_str!("../../../fixtures/oracle/macos/probes.jsonl").lines() {
            let row: Value = serde_json::from_str(line).unwrap();
            let Some(case) = row["case"].as_str().filter(|s| s.starts_with("execute-")) else {
                continue;
            };
            let expected = row["error"].as_array().unwrap();
            let cause = expected.last().unwrap();
            let reason = match case {
                "execute-DeviceFailureException-IncompatibleDevice" => Some(DeviceFailureReason::IncompatibleDevice),
                "execute-DeviceFailureException-FailedRunner" => Some(DeviceFailureReason::FailedRunner),
                _ => None,
            };
            let error = classify_run(RunError {
                error: VendorError::Other(failure(
                    cause["class"].as_str().unwrap(),
                    cause["message"].as_str().unwrap(),
                )),
                reason,
                suppressed: Box::default(),
            });
            let mut chain = vec![];
            let mut current = Some(error.failure());
            while let Some(f) = current {
                chain.push(json!({"class":f.class.to_owned(),"message":f.message.clone()}));
                current = f.cause.as_deref();
            }
            assert_eq!(json!(chain), row["error"], "{case}");
        }
    }
}
