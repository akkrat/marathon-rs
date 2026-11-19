//! Port of vendor/vendor-apple/macos/src/main/kotlin/com/malinskiy/marathon/apple/macos/MacosDevice.kt

use marathon_logging::{Component, Level};

use crate::installer::MacosApplicationInstaller;
use jcompat::{file_manager::FileManager, jtime::Duration};
use marathon_apple::{
    apple_application_installer::AppleApplicationInstaller,
    apple_device::{AppleDevice, storage_path_for},
    apple_device_test_runner::{AppleDeviceTestRunner, RunError},
    bin::AppleBinaryEnvironment,
    cmd::{
        CommandResult, SealCause, ShutdownWindow, Termination, failure,
        remote::ssh::sshj::config::SshLogging,
        retained_tasks::{RetainedTasks, panic_as_error},
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
    analytics::{DeviceTimingLog, track_device_operation},
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
use marathon_error::{FailureKind, FatalKind};
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
    /// D84 (R3-2): completed when the latest preparation's retained work has ended, whatever its outcome. A cancelled
    /// `prepare` returns to its caller at once and leaves this work running.
    preparation: Option<Arc<SharedDeferred<()>>>,
    /// D93: stops the latest preparation; only a Mac that is stopping uses it.
    preparation_stop: Option<Arc<Signal>>,
    /// D91, D93: sends to this Mac that may still be running, whoever waits for them, registered under this lock so that
    /// none starts once the Mac is sealed.
    transfers: Vec<Arc<marathon_apple::cmd::TransferReceipt>>,
    /// D93 (round 9): pulls from this Mac, registered as its sends are: one still running at its disposal keeps its lease
    /// from being released beside it.
    pulls: marathon_apple::cmd::DevicePulls,
    /// D93: sealed by the run's stop or by the first disposal: the Mac's one window, its verdict and its receipt.
    shutdown: Option<Arc<Shutdown>>,
    /// D93: its disposal could not confirm that its work had ended: its lease is quarantined, not released.
    quarantined: bool,
}
impl Execution {
    /// U210, D93: why no new batch, preparation or send may start, if none may.
    /// D93 (rounds 7, 8): the refusal as an error, built by the seal's cause alone (`SealCause::refusal`).
    fn refusal_error(&self, serial: &str) -> Option<VendorError> {
        let why = failure(FailureKind::IllegalState, self.refusal(serial)?);
        let cause = self.shutdown.as_ref().map_or(SealCause::Disposal, |s| s.cause);
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
/// D93: a sealed Mac's one window, with its verdict, and the receipt it is decided on: what the Mac ran when it was
/// sealed. Sealed, it starts nothing new.
struct Shutdown {
    window: Arc<ShutdownWindow>,
    /// Sealed by the run's stop, whose published bound the lease's release shares.
    by_run_stop: bool,
    /// D93 (round 7): why it was sealed, which alone classifies a refusal after it.
    cause: SealCause,
    batches: Vec<Arc<BatchJob>>,
    preparation: Option<Arc<SharedDeferred<()>>>,
    preparation_stop: Option<Arc<Signal>>,
    transfers: Vec<Arc<marathon_apple::cmd::TransferReceipt>>,
    /// The xcodebuild sessions registered and not confirmed to have ended when it was sealed: one still running (a
    /// batch's, a parse's) and one whose exec or close could not be confirmed. No session registers after the seal.
    sessions: Vec<Arc<XcodebuildSession>>,
}
struct BatchJob {
    result: Arc<SharedDeferred<()>>,
    cancellation: Arc<Signal>,
    /// D93: the xcodebuild sessions this batch opened, which the Mac's stop can stop while the batch is still in its
    /// cleanup (an xcresult pull) before its runner stops them. A parse's sessions are not here: they are its own.
    sessions: Mutex<Vec<Arc<XcodebuildSession>>>,
}
impl BatchJob {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            result: SharedDeferred::new(),
            cancellation: Signal::new(),
            sessions: Mutex::default(),
        })
    }
}
tokio::task_local! {
    /// D93: the batch whose task this is, so that a session it opens is known as the batch's.
    static BATCH: Arc<BatchJob>;
}
pub struct MacosDevice {
    identity: Weak<Self>,
    udid: String,
    serial: String,
    transport: Transport,
    storage: String,
    binary: AppleBinaryEnvironment,
    /// D93: the Mac's own handle on its worker's connection, which `binary` is built on and which its disposal can
    /// revoke; its lease is the connection inside.
    device_commands: Arc<marathon_apple::cmd::DeviceCommands>,
    bridge: Bridge,
    commands: Arc<ExecutorWorkerCommands>,
    remote: RemoteFileManager,
    logger: Component,
    pub(crate) timing_log: DeviceTimingLog,
    facts: Mutex<Facts>,
    execution: Mutex<Execution>,
    signal_finish: marathon_core::signal_finish::SignalDeadline,
    listeners: Arc<LineListeners>,
    sessions: Mutex<Vec<Arc<XcodebuildSession>>>,
    tasks: RetainedTasks,
    /// D87: when this Mac's preparation must end, on the run's vendor clock; `i64::MAX` for none.
    preparation_deadline: std::sync::atomic::AtomicI64,
    pub options: MacosDeviceOptions,
}
impl MacosDevice {
    /// `binary` is built on `device_commands`.
    pub fn new(
        udid: String,
        transport: Transport,
        binary: AppleBinaryEnvironment,
        device_commands: Arc<marathon_apple::cmd::DeviceCommands>,
        bridge: Bridge,
        options: MacosDeviceOptions,
    ) -> Arc<Self> {
        let serial: String = format!("{udid}@{}", binary.command_executor.host().id);
        let storage = storage_path_for(&udid);
        let logger = options.logging.component("marathon_macos::device");
        let timing_log = DeviceTimingLog::new(options.dependencies.log.clone(), &options.dependencies.logging);
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
            device_commands,
            bridge,
            commands,
            remote,
            logger,
            timing_log,
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
            signal_finish: Default::default(),
            execution: Mutex::default(),
            listeners: Arc::default(),
            sessions: Mutex::default(),
            tasks: RetainedTasks::default(),
            preparation_deadline: std::sync::atomic::AtomicI64::new(i64::MAX),
            options,
        });
        device.options.installer.register(&device);
        device
    }
    /// D87: how many commands with a limit (each `shell`) one preparation can send at most, with every retry inside it
    /// made. `tests/preparation_commands.rs` checks it against what a preparation sends, variant by variant:
    /// - 3 directory commands, then 2 in each of the installer's 3 attempts at the test bundle;
    /// - the test type's probe (`nm`), unless the test type is configured;
    /// - the test root's directory;
    /// - hosted XCTest: the app's `chmod` and its `PlugIns` directory (2). XCUITest, or a test type left to the probe, up
    ///   to 14: the SDK platform path; the runner, each of its two overwriting copies preceded by a removal (4), three
    ///   architecture commands on arm64e (`lipo` twice and a removal), three plist edits, its `PlugIns` directory and the
    ///   test bundle's copy into it; the app's `chmod`. (With a runner of its own: `chmod`, a removal and a link, and the
    ///   architecture commands, fewer.)
    /// - with permissions to grant: the SIP check, then 2 per permission (the system's TCC database and the user's);
    /// - for each shared destination sent to, one `test -e` when an earlier transfer of the same file there is on record
    ///   (a parse or another device sent it) and it may have gone: the test bundle, the app, hosted XCTest's copy into the
    ///   app's `PlugIns`, each extra application.
    ///
    /// Not counted: code coverage's static-library directories, one per library, which only the built bundle names.
    pub fn preparation_commands(&self) -> i64 {
        let vendor = &self.options.vendor;
        let test_type = vendor.bundle.as_ref().and_then(|b| b.test_type);
        let probe = i64::from(test_type.is_none());
        let test_root = match test_type {
            Some(marathon_config::vendor::TestType::Xctest) => 2,
            _ => 14,
        };
        let grants = &vendor.permissions.grant;
        let permissions = match grants.is_empty() {
            true => 0,
            false => 1 + 2 * grants.iter().map(|(_, p)| p.len() as i64).sum::<i64>(),
        };
        let extras = vendor
            .bundle
            .as_ref()
            .and_then(|b| b.extra_applications.as_ref())
            .map_or(0, |e| e.len() as i64);
        let hosted = matches!(test_type, Some(marathon_config::vendor::TestType::Xctest));
        let cached = 2 + i64::from(hosted) + extras;
        3 + 6 + probe + 1 + test_root + permissions + cached
    }
    /// D87: whether a retry of this Mac's preparation may start now, before its deadline.
    fn may_start_attempt(&self) -> bool {
        self.options.dependencies.clock.now_millis()
            < self.preparation_deadline.load(std::sync::atomic::Ordering::Acquire)
    }
    fn retained(&self) -> VendorResult<Arc<Self>> {
        self.identity
            .upgrade()
            .ok_or_else(|| VendorError::Other(failure(FailureKind::IllegalState, "macOS device identity expired")))
    }
    /// The Mac's lease on its worker's connection; its commands go through its own handle on it.
    pub fn executor(&self) -> Executor {
        self.device_commands.lease()
    }
    /// D93: its disposal could not confirm that its work had ended, so its lease is to be quarantined, not released.
    pub fn quarantined(&self) -> bool {
        self.execution.lock().unwrap_or_else(|e| e.into_inner()).quarantined
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
        .with_retry_gate(&|| self.may_start_attempt())
        .prepare_installation(self, false, context)
        .await
        .map_err(|e| e.into_vendor())?;
        let bundle = self
            .options
            .vendor
            .bundle
            .as_ref()
            .ok_or_else(|| VendorError::Other(failure(FailureKind::IllegalArgument, "No test bundle provided")))?;
        let xctest = bundle
            .xctest
            .as_ref()
            .ok_or_else(|| VendorError::Other(failure(FailureKind::IllegalArgument, "No test bundle provided")))?;
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
        result: Arc<Deferred<TestBatchResults>>,
        context: VendorContext,
        caller_cancellation: Arc<dyn CancellationSignal>,
    ) -> VendorResult<()> {
        let (listener, lines) = self.execution_listeners(pool, batch.clone(), result.clone(), context.clone())?;
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
                        FailureKind::UninitializedProperty,
                        "lateinit property testBundle has not been initialized",
                    ))
                })?;
            let mut runner = AppleDeviceTestRunner::new(
                self.retained()?,
                self.options.dependencies.io.clone(),
                self.options.logging.clone(),
            );
            runner.observe_batch_results(result, caller_cancellation);
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
        result: Arc<Deferred<TestBatchResults>>,
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
                    executor: self.binary.command_executor.clone(),
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
    /// D84 (R3-2): waits for the latest preparation's retained work (an installation that its cancelled caller left
    /// running) within `cancelled_batch_timeout`, the bound this device gives a cancelled call. The provider waits for
    /// this before it releases the Mac's lease, so the lease is not released under a running installation within it.
    /// D93: within what is left of the Mac's one window (sealed by the run's stop or its disposal), together with the rest
    /// of its work, rather than a window of its own; what did not end is for its disposal to report.
    pub async fn join_preparation(&self) -> VendorResult<()> {
        self.finish_work().await;
        Ok(())
    }
    fn window_millis(&self) -> VendorResult<i64> {
        self.options
            .cancelled_batch_timeout
            .to_millis()
            .ok_or_else(|| VendorError::Other(failure(FailureKind::Arithmetic, "long overflow")))
    }
    /// D93: seals the Mac, once: it starts no batch, preparation or send from now on, its window for ending what it runs
    /// (`cancelled_batch_timeout`) starts, and what it runs now is the receipt its one verdict is decided on. No I/O, no
    /// blocking: the run's stop calls it on the core thread.
    fn seal(&self) -> Arc<Shutdown> {
        let mut execution = self.execution.lock().unwrap_or_else(|e| e.into_inner());
        self.seal_locked(&mut execution, SealCause::RunStop)
    }
    /// D93 (round 8): the provider's termination at the run's end, just before it disposes of the Mac: a refusal after it
    /// is an orderly cancellation; the disposal keeps its own limits (not a run's stop).
    pub fn seal_for_termination(&self) {
        let mut execution = self.execution.lock().unwrap_or_else(|e| e.into_inner());
        self.seal_locked(&mut execution, SealCause::Termination);
    }
    /// A window too long for millis has no end; deciding then fails as `window_millis` says, which is `Unconfirmed`.
    /// D93 (round 7): records the seal with its cause, closes the Mac's command gate in the same critical section, and stops
    /// at once what it registered before it (`stop_all`): nothing registered before the seal starts after it.
    fn seal_locked(&self, execution: &mut Execution, cause: SealCause) -> Arc<Shutdown> {
        if let Some(shutdown) = &execution.shutdown {
            return shutdown.clone();
        }
        let window = self.options.cancelled_batch_timeout.to_millis().unwrap_or(i64::MAX);
        let deadline = self
            .options
            .dependencies
            .clock
            .now_millis()
            .saturating_add(window.max(0));
        let shutdown = Arc::new(Shutdown {
            window: ShutdownWindow::new(deadline),
            by_run_stop: cause == SealCause::RunStop,
            cause,
            batches: execution.running.clone().into_iter().collect(),
            preparation: execution.preparation.clone().filter(|p| p.completed().is_none()),
            preparation_stop: execution.preparation_stop.clone(),
            // D91: a send whose cleanup was not confirmed earlier in the run counts too (`send_stop` keeps those).
            transfers: execution
                .transfers
                .iter()
                .filter(|t| t.ended() != Some(marathon_apple::cmd::TransferCleanup::Confirmed))
                .cloned()
                .collect(),
            // Under the execution lock, as `execute_test_request` registers a session.
            sessions: self
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|s| s.ended() != Some(true))
                .cloned()
                .collect(),
        });
        execution.shutdown = Some(shutdown.clone());
        let why = execution.refusal(&self.serial).unwrap_or_default();
        self.device_commands
            .seal(cause, failure(FailureKind::IllegalState, why));
        self.stop_all(&shutdown);
        match self.identity.upgrade() {
            Some(device) => {
                let deciding = shutdown.clone();
                self.tasks.spawn(&*self.options.dependencies.io, async move {
                    match panic_as_error(device.await_receipt(&deciding)).await {
                        Ok(verdict) => {
                            deciding.window.decide(verdict);
                        }
                        Err(error) => deciding.window.fail(error),
                    }
                    Ok(())
                });
            }
            None => {
                shutdown.window.decide(Termination::Unconfirmed(vec![
                    "the Mac's allocation was released".into(),
                ]));
            }
        }
        shutdown
    }
    /// D93: the sessions the receipt covers: those registered and not confirmed to have ended at the seal, and those its
    /// batches opened.
    fn receipt_sessions(&self, shutdown: &Shutdown) -> Vec<Arc<XcodebuildSession>> {
        let mut sessions = shutdown.sessions.clone();
        for batch in &shutdown.batches {
            for session in batch.sessions.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                if !sessions.iter().any(|s| Arc::ptr_eq(s, session)) {
                    sessions.push(session.clone());
                }
            }
        }
        sessions
    }
    /// D93: stops everything in the receipt: the batch, the preparation, the sends (by the window's end). Idempotent.
    fn stop_all(&self, shutdown: &Shutdown) {
        let stop = || {
            marathon_core::error::Cancellation::Requested(failure(
                FailureKind::JobCancelled,
                format!("Device {} is being disposed", self.serial),
            ))
        };
        for job in &shutdown.batches {
            job.cancellation
                .cancel(marathon_core::error::Cancellation::Requested(failure(
                    FailureKind::JobCancelled,
                    "LazyDeferredCoroutine was cancelled",
                )));
        }
        if let Some(signal) = &shutdown.preparation_stop {
            signal.cancel(stop());
        }
        for transfer in &shutdown.transfers {
            transfer.stop(
                stop(),
                Some(
                    self.signal_finish
                        .limit(shutdown.window.deadline, |finish| finish.receipts),
                ),
            );
        }
        // A session registered before the seal that has not begun to open never will.
        for session in &shutdown.sessions {
            session.refuse_unopened(shutdown.cause.refusal(stop().failure().clone()));
        }
    }
    /// D93: seals the Mac if it is not yet, stops what it runs and returns its one verdict, decided at most once, within
    /// its window. Every caller (the cancelled call, the run's stop, disposal, the provider) reads the same verdict.
    async fn finish_work(&self) -> Termination {
        let shutdown = self.seal();
        self.stop_all(&shutdown);
        shutdown.window.verdict().await
    }
    /// D93: the one decision: waits for the receipt until the window's end, forcing in its reserve what has not ended (the
    /// batch's sessions, still running while the batch is stuck in its cleanup, are stopped); then what has not ended.
    async fn await_receipt(&self, shutdown: &Shutdown) -> VendorResult<Termination> {
        let context = self.context();
        let clock = context.clock.clone();
        let deadline = shutdown.window.deadline;
        let reserve = marathon_apple::cmd::device_commands::force_reserve_millis(self.window_millis()?);
        let graceful = deadline.saturating_sub(reserve);
        // D93 (review round 2): the verdict is these waits' outcome. Work not seen ended before the deadline is unconfirmed,
        // whatever a later look would find.
        let in_time = self.wait_for(shutdown, graceful, |finish| finish.drain).await?;
        let in_time = in_time || {
            self.stop_all(shutdown);
            let left = self
                .signal_finish
                .limit(deadline, |finish| finish.receipts)
                .saturating_sub(clock.now_millis())
                .max(0);
            let forced = self
                .receipt_sessions(shutdown)
                .into_iter()
                .filter(|s| s.ended().is_none())
                .map(|session| {
                    let context = context.clone();
                    async move {
                        let refusal = shutdown.cause.refusal(failure(
                            FailureKind::IllegalState,
                            format!("Device {} is being disposed", self.serial),
                        ));
                        let _ = session
                            .stop_refusing(marathon_apple::bin::java_duration_of_millis(left), &context, refusal)
                            .await;
                    }
                });
            let all = async {
                let (_, waited) = tokio::join!(
                    futures_util::future::join_all(forced),
                    self.wait_for(shutdown, deadline, |finish| finish.receipts)
                );
                waited
            };
            tokio::select! { biased;
                result = all => result?,
                _ = self.signal_finish.wait_for_signal(clock.as_ref(), |finish| finish.receipts) => false,
            }
        };
        let mut left = Vec::new();
        if shutdown.batches.iter().any(|b| b.result.completed().is_none()) {
            left.push("its batch has not ended".to_owned());
        }
        if shutdown.preparation.as_ref().is_some_and(|p| p.completed().is_none()) {
            left.push("its preparation has not ended".to_owned());
        }
        for transfer in &shutdown.transfers {
            match transfer.ended() {
                None => left.push("a send has not ended".to_owned()),
                Some(marathon_apple::cmd::TransferCleanup::Unconfirmed(why)) => left.push(format!(
                    "a send's cleanup was not confirmed ({})",
                    why.message.as_deref().unwrap_or_default()
                )),
                Some(marathon_apple::cmd::TransferCleanup::Confirmed) => {}
            }
        }
        for session in self.receipt_sessions(shutdown) {
            match session.ended() {
                None => left.push("an xcodebuild session has not ended".to_owned()),
                Some(false) => left.push("an xcodebuild session's command did not close".to_owned()),
                Some(true) => {}
            }
        }
        // D93 (round 10): every command it handed to the worker before the seal (or its batch's after it).
        left.extend(self.device_commands.handed_left());
        if !in_time {
            if left.is_empty() {
                left.push("its work was not seen to end before its window ended".to_owned());
            }
            return Ok(Termination::Unconfirmed(left));
        }
        Ok(if left.is_empty() {
            Termination::Confirmed
        } else {
            Termination::Unconfirmed(left)
        })
    }
    /// Waits up to `millis` for everything in the receipt, then for its sessions' producers (a batch opens its session
    /// itself); whether all of it ended. Nothing left: no wait, only a look.
    async fn wait_for(
        &self,
        shutdown: &Shutdown,
        until: i64,
        cutoff: fn(marathon_core::signal_finish::SignalFinish) -> i64,
    ) -> VendorResult<bool> {
        let context = self.context();
        let clock = &context.clock;
        let deadline = self
            .signal_finish
            .limit(shutdown.window.deadline, |finish| finish.receipts);
        let ended = |device: &Self| {
            shutdown.batches.iter().all(|b| b.result.completed().is_some())
                && shutdown.preparation.as_ref().is_none_or(|p| p.completed().is_some())
                && shutdown.transfers.iter().all(|t| t.ended().is_some())
                && device.receipt_sessions(shutdown).iter().all(|s| s.ended().is_some())
                && device.device_commands.handed_ended()
        };
        let millis = until.saturating_sub(clock.now_millis());
        if ended(self) {
            // A look after the deadline counts for nothing.
            return Ok(clock.now_millis() <= deadline);
        }
        if millis <= 0 {
            return Ok(false);
        }
        let timed = clock.with_timeout_or_null(
            Signal::new(),
            millis,
            Box::new(move |signal| {
                Box::pin(async move {
                    let cells = shutdown
                        .batches
                        .iter()
                        .map(|b| b.result.clone())
                        .chain(shutdown.preparation.clone());
                    for done in cells {
                        tokio::select! { biased;
                            reason = signal.cancelled() => return Err(VendorError::Cancelled(reason)),
                            _ = done.wait() => {}
                        }
                    }
                    for send in &shutdown.transfers {
                        if let Err(VendorError::Cancelled(reason)) = send.wait_in(signal.clone()).await
                            && send.ended().is_none()
                        {
                            return Err(VendorError::Cancelled(reason));
                        }
                    }
                    for session in self.receipt_sessions(shutdown) {
                        tokio::select! { biased;
                            reason = signal.cancelled() => return Err(VendorError::Cancelled(reason)),
                            _ = session.wait_ended() => {}
                        }
                    }
                    self.device_commands.wait_handed(signal).await
                })
            }),
        );
        let finished = tokio::select! { biased;
            result = timed => result?.is_some(),
            _ = self.signal_finish.wait_for_signal(clock.as_ref(), cutoff) => false,
        };
        let received_in_time = self
            .signal_finish
            .current()
            .is_none_or(|finish| clock.now_millis() <= finish.receipts);
        Ok(finished && received_in_time && ended(self))
    }
    /// D93: the disposal could not confirm that the Mac's work had ended: once, one WARN naming it and what was left, no
    /// further command from it, and its lease quarantined in its connection factory now rather than released.
    fn quarantine(&self, left: &[String]) {
        {
            let mut execution = self.execution.lock().unwrap_or_else(|e| e.into_inner());
            if execution.quarantined {
                return;
            }
            execution.quarantined = true;
        }
        self.device_commands.quarantine(failure(
            FailureKind::IllegalState,
            format!(
                "Device {}'s work was not confirmed to have ended: {}",
                self.serial,
                left.join(", ")
            ),
        ));
        let level = if self.signal_finish.current().is_some() {
            Level::ERROR
        } else {
            Level::WARN
        };
        self.log(
            level,
            format!(
                "Work on {} has not finished within {}, not waiting for it any longer ({}); sending it no further command and keeping its connection out of use",
                self.serial.to_owned(),
                marathon_core::error::duration_for_log(self.options.cancelled_batch_timeout),
                left.join(", "),
            ),
            None,
        );
    }
    /// D93 (round 7): why the Mac was sealed, once it has been.
    fn seal_cause(&self) -> Option<SealCause> {
        self.execution
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown
            .as_ref()
            .map(|s| s.cause)
    }
    /// D93: how the provider quarantines this Mac's lease, applied when its disposal decides to.
    pub fn on_lease_quarantine(&self, hook: marathon_apple::cmd::device_commands::LeaseQuarantine) {
        self.device_commands.on_quarantine(hook);
    }
    /// D93: the window's end, on the vendor clock, once the Mac is sealed.
    pub fn shutdown_deadline(&self) -> Option<i64> {
        self.execution
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown
            .as_ref()
            .filter(|s| s.by_run_stop)
            .map(|s| s.window.deadline)
    }
    /// A batch whose call was cancelled. D93: once the Mac is sealed this is part of its one window; otherwise the call has
    /// its own window, as in Kotlin.
    async fn join_cancelled(&self, batch: &Arc<BatchJob>) -> VendorResult<()> {
        let sealed = self
            .execution
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown
            .clone();
        if let Some(shutdown) = sealed {
            self.finish_work().await;
            // An error while deciding is this call's error too, as the window's wait's was before D93.
            return match shutdown.window.failure() {
                Some(error) => Err(error),
                None => Ok(()),
            };
        }
        batch
            .cancellation
            .cancel(marathon_core::error::Cancellation::Requested(failure(
                FailureKind::JobCancelled,
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
                    .ok_or_else(|| VendorError::Other(failure(FailureKind::Arithmetic, "long overflow")))?,
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
                Level::WARN,
                format!(
                    "Cancelled batch on {} has not finished within {}, not waiting for it any longer",
                    self.serial.to_owned(),
                    marathon_core::error::duration_for_log(self.options.cancelled_batch_timeout)
                ),
                None,
            );
        }
        Ok(())
    }
    async fn dispose_inner(&self) -> VendorResult<()> {
        let (cell, first) = {
            let mut state = self.execution.lock().unwrap_or_else(|e| e.into_inner());
            match &state.disposal {
                Some(cell) => (cell.clone(), false),
                None => {
                    let cell = SharedDeferred::new();
                    state.disposal = Some(cell.clone());
                    // D93: the window the run's stop sealed, or one that starts with this first disposal.
                    self.seal_locked(&mut state, SealCause::Disposal);
                    (cell, true)
                }
            }
        };
        if first {
            let device = self.retained()?;
            let outcome_cell = SharedDeferred::new();
            let first_complete = outcome_cell.clone();
            let complete = cell.clone();
            self.tasks.spawn(&*self.options.dependencies.io, async move {
                // D84: a panic is published to the callers too. D93: only work confirmed to have ended lets the lease
                // be released.
                let outcome = panic_as_error(async {
                    // D93 (round 9): a pull still running keeps the lease from being released beside it, as unconfirmed
                    // work does; one left after a run's stop or the termination is stopped (D91's bound).
                    let verdict = device.finish_work().await;
                    // D93 (round 10): the look at its pulls closes them, under the lock they register under: a pull asked
                    // for after it is refused as it starts (an undelivered artifact), whatever the seal's cause.
                    let (pulls, cause) = {
                        let execution = device.execution.lock().unwrap_or_else(|e| e.into_inner());
                        device.device_commands.close_pulls(failure(
                            FailureKind::IllegalState,
                            format!("Device {} is being disposed", device.serial),
                        ));
                        let cause = execution.shutdown.as_ref().map_or(SealCause::Disposal, |s| s.cause);
                        (execution.pulls.snapshot(), cause)
                    };
                    if let Some(finish) = device.signal_finish.current() {
                        marathon_apple::cmd::device_commands::wait_for_pulls(
                            &pulls,
                            device.options.dependencies.clock.as_ref(),
                            finish.receipts,
                        )
                        .await;
                    }
                    let pulls_left =
                        marathon_apple::cmd::device_commands::settle_pulls(&pulls, &device.serial, cause.stops_pulls());
                    match verdict {
                        Termination::Unconfirmed(left) => device.quarantine(&left),
                        Termination::Confirmed if !pulls_left.is_empty() => device.quarantine(&pulls_left),
                        Termination::Confirmed => {}
                    }
                    // An error while deciding is the disposal's error too, after the quarantine.
                    match device.seal().window.failure() {
                        Some(error) => Err(error),
                        None => Ok(()),
                    }
                })
                .await;
                if let Err(error) = &outcome {
                    // D93: a disposal that failed or panicked has confirmed nothing.
                    device.quarantine(&[marathon_apple::cmd::device_commands::failure_text(error.failure())]);
                }
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
                FailureKind::IncompatibleDevice,
                "Modifying permissions is supported to macOS 10+",
            )));
        }
        // P4: malformed client identifiers are rejected before constructing SQL.
        if client.chars().any(|c| c == '"' || c.is_control()) {
            return Err(VendorError::Other(failure(
                FailureKind::IllegalArgument,
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
    /// D54: a send to this Mac ends on its caller's cancellation or when the Mac's own connection is lost. D91, D93: and
    /// when the Mac stops: registered before the send starts, under the lock its seal and disposal take, so that its one
    /// window covers the send, whoever else waits for it, or the send never starts.
    fn send_stop(&self, context: &VendorContext) -> marathon_apple::cmd::TransferStop {
        let receipt = marathon_apple::cmd::TransferReceipt::new();
        {
            let mut execution = self.execution.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(refusal) = execution.refusal(&self.serial) {
                receipt.stop(
                    marathon_core::error::Cancellation::Requested(failure(FailureKind::IllegalState, refusal)),
                    None,
                );
            } else {
                // D93: a send whose cleanup was not confirmed is kept, so that the Mac's stop still counts it.
                execution
                    .transfers
                    .retain(|t| t.ended() != Some(marathon_apple::cmd::TransferCleanup::Confirmed));
                execution.transfers.push(receipt.clone());
            }
        }
        marathon_apple::cmd::send_stop(self.binary.command_executor.as_ref(), context).owned_by(receipt)
    }
    /// D93 (round 7): a send that this Mac's seal stopped or refused is what the seal's cause says, whatever its caller's
    /// state: a device failure after a disposal outside a run's stop (discovery then takes another device, D53), a
    /// cancellation after the run's stop.
    fn leaving_send(&self, error: VendorError) -> VendorError {
        match error {
            VendorError::Cancelled(reason) => match self.seal_cause() {
                Some(cause) => cause.refusal(reason.failure().clone()),
                None => VendorError::Cancelled(reason),
            },
            other => other,
        }
    }
    /// D54: a receive from it ends only when that connection is lost.
    /// D93 (round 8): also the Mac's pull gate, closed by a disposal's seal or a quarantine.
    /// D93 (round 9): registered with the Mac, as a send is, so that its disposal finds it if it still runs.
    fn receive_stop(&self) -> marathon_apple::cmd::TransferStop {
        let receipt = self
            .execution
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pulls
            .register(self.options.dependencies.clock.clone());
        marathon_apple::cmd::receive_stop_gated(
            self.binary.command_executor.as_ref(),
            self.device_commands.pull_signal(),
        )
        .owned_by(receipt)
    }
    /// D93 (round 8): a pull the Mac's pull gate refused or stopped is an undelivered artifact, with a WARN.
    fn gated_pull(&self, received: VendorResult<bool>, src: &str) -> VendorResult<bool> {
        match received {
            Err(VendorError::Cancelled(reason)) if self.device_commands.is_gate_cancellation(&reason) => {
                self.log(
                    Level::WARN,
                    format!(
                        "Not receiving {src} from {}: {}",
                        self.serial,
                        reason.failure().for_log()
                    ),
                    None,
                );
                Ok(false)
            }
            other => other,
        }
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
    let rendered = if cause.kind == marathon_apple::cmd::remote::ssh::sshj::error::OPEN_FAIL_EXCEPTION {
        marathon_apple::cmd::remote::ssh::sshj::SshFailure::new(
            marathon_apple::cmd::remote::ssh::sshj::error::OPEN_FAIL_EXCEPTION,
            cause.message.as_ref().map_or_else(|| "null".into(), String::clone),
        )
        .to_string()
    } else if cause.kind == FailureKind::JobCancelled
        && cause.message == Some("LazyDeferredCoroutine was cancelled".into())
    {
        format!(
            "{}; job=LazyDeferredCoroutine{{Cancelled}}",
            cause.to_string().to_owned()
        )
    } else {
        cause.to_string().to_owned()
    };
    let mut detail = failure(FailureKind::DeviceLost, rendered);
    detail.cause = Some(Box::new(cause));
    VendorError::DeviceLost(detail)
}
fn classify_setup(error: VendorError) -> VendorError {
    match error {
        VendorError::Other(f) if f.kind == FailureKind::DeviceSetup => VendorError::DeviceSetup(f),
        other => other,
    }
}
fn classify_run(run: RunError) -> VendorError {
    let reason = run.reason;
    let error = run.into_vendor();
    match reason {
        Some(DeviceFailureReason::IncompatibleDevice) => {
            let mut f = failure(FailureKind::IncompatibleDevice, error.failure().to_string());
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
    let class = error.failure().kind.to_owned();
    if class == FailureKind::IllegalState
        || [CONNECTION_EXCEPTION, TRANSPORT_EXCEPTION, OPEN_FAIL_EXCEPTION]
            .iter()
            .any(|base| is_instance(&class, base.clone()))
    {
        lost(error.failure().clone())
    } else {
        error
    }
}
fn unsupported<T>() -> VendorResult<T> {
    Err(VendorError::Other(failure(
        FailureKind::Fatal(FatalKind::NotImplemented),
        "An operation is not implemented: Not yet implemented",
    )))
}
impl Device for MacosDevice {
    /// D84: a cancelled batch is joined within `cancelled_batch_timeout`; a cancelled preparation returns at once.
    fn cancellation_cleanup_millis(&self) -> Option<i64> {
        self.options.cancelled_batch_timeout.to_millis()
    }
    /// D84 (R3-2): a cancelled preparation's installation goes on in a retained task. D93: the wait covers all of the
    /// Mac's work, within what is left of the window `seal_for_stop` started.
    fn join_cancelled_work(&self) -> VendorFuture<'_, ()> {
        Box::pin(self.join_preparation())
    }
    fn seal_for_stop(&self) {
        let mut execution = self.execution.lock().unwrap_or_else(|e| e.into_inner());
        let shutdown = self.seal_locked(&mut execution, SealCause::RunStop);
        if self.signal_finish.current().is_none() {
            execution.pulls.limit_to(shutdown.window.deadline);
        }
    }
    fn seal_for_signal(&self, finish: marathon_core::signal_finish::SignalFinish) {
        self.signal_finish.set(finish);
        self.device_commands.limit_commands_for_signal(finish);
        let mut execution = self.execution.lock().unwrap_or_else(|e| e.into_inner());
        execution.pulls.limit_for_signal(finish);
        let shutdown = self.seal_locked(&mut execution, SealCause::RunStop);
        self.stop_all(&shutdown);
    }
    fn info(&self) -> DeviceInfo {
        self.facts.lock().unwrap_or_else(|e| e.into_inner()).info.clone()
    }
    fn abi(&self) -> String {
        self.facts.lock().unwrap_or_else(|e| e.into_inner()).abi.clone()
    }
    fn worker_id(&self) -> Option<String> {
        Some(self.transport.id())
    }
    fn component(&self) -> &Component {
        &self.logger
    }
    /// D87: a Mac is never replaced by a recovery during its preparation, so its episode is the device actor's attempts,
    /// and the installer sends a failed transfer again only before the deadline. The envelope is what one preparation's
    /// commands with known limits can take, each under `shell` ([`MacosDevice::preparation_commands`]), and the
    /// installer's two 1 s pauses. A transfer has no limit while it progresses (D54) and is not counted.
    fn preparation_time_left_millis(&self, configuration: &Configuration) -> Option<i64> {
        let shell = self
            .options
            .vendor
            .timeout_configuration
            .shell
            .to_millis()
            .unwrap_or(i64::MAX);
        let envelope = shell.saturating_mul(self.preparation_commands()).saturating_add(2_000);
        let budget = marathon_core::vendor::preparation_budget_millis(configuration, envelope);
        let now = self.options.dependencies.clock.now_millis();
        self.preparation_deadline.store(
            now.saturating_add(budget).min(i64::MAX - 1),
            std::sync::atomic::Ordering::Release,
        );
        Some(budget)
    }
    fn prepare<'a>(&'a self, _: Arc<Configuration>, context: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let device = self.retained()?;
            let mut own_context = self.context();
            let done = SharedDeferred::new();
            let stop = Signal::new();
            // D93: the preparation goes on after its caller is cancelled (D84) and is stopped only when the Mac stops.
            own_context.cancellation = stop.clone();
            {
                // U210: registered under the lock that disposal installs itself with, so a disposed Mac starts no
                // preparation, and `join_preparation` after a disposal sees every preparation there is. A cancellation,
                // as iOS refuses a disposed simulator's (U179): the device actor does not retry it.
                let mut execution = self.execution.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(refusal) = execution.refusal_error(&self.serial) {
                    return Err(refusal);
                }
                execution.preparation = Some(done.clone());
                execution.preparation_stop = Some(stop);
            }
            let task = self.tasks.spawn(&*self.options.dependencies.io, async move {
                let label = "core.Track.trackDevicePreparing";
                let outcome = panic_as_error(track_device_operation(
                    own_context.cancellation.as_ref(),
                    || async { own_context.hooks.instant(label).map_err(crate::clock_error) },
                    device.prepare_inner(&own_context),
                    |start, finish| {
                        device
                            .options
                            .dependencies
                            .tracker
                            .device_preparing(&device.serial, start, finish)
                    },
                    |error| device.timing_log.warn("preparation", &device.serial, error),
                ))
                .await;
                done.complete(Ok(()));
                outcome
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
        result: Arc<Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let device = self.retained()?;
            let job = BatchJob::new();
            {
                let mut execution = self.execution.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(refusal) = execution.refusal_error(&self.serial) {
                    return Err(refusal);
                }
                execution.running = Some(job.clone());
            }
            let running = job.clone();
            let mut own_context = context.clone();
            own_context.cancellation = job.cancellation.clone();
            let caller_cancellation = context.cancellation.clone();
            self.tasks.spawn(&*self.options.dependencies.io, async move {
                let outcome = if let Some(reason) = running.cancellation.reason() {
                    Err(VendorError::Cancelled(reason))
                } else {
                    // D84: a panic is this batch's error too, so that the caller and disposal hear of its end.
                    BATCH
                        .scope(
                            running.clone(),
                            marathon_apple::cmd::device_commands::as_registered_batch(panic_as_error(device.run(
                                configuration,
                                pool,
                                batch,
                                result,
                                own_context,
                                caller_cancellation,
                            ))),
                        )
                        .await
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
                    // D93 (round 7): on a sealed Mac only the seal's cause says what the cancelled batch is.
                    // A disposal outside a stop keeps the existing rule: its caller's own cancel came first.
                    if self.seal_cause() == Some(SealCause::RunStop) {
                        return Err(VendorError::Cancelled(reason));
                    }
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
                if error.failure().kind.is_fatal() {
                    return Err(error);
                }
                self.log(
                    Level::DEBUG,
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
                    FailureKind::DeviceSetup,
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
                    FailureKind::DeviceSetup,
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
                // D93: registered under the lock the seal takes, as a batch, a preparation and a send are: a sealed Mac
                // opens no session, and one opened before the seal is in the seal's receipt. A session leaves the list
                // only once it has ended confirmed, so one whose exec or close could not be confirmed keeps the Mac's stop
                // unconfirmed for the rest of the run.
                // A refusal is a device failure, as a batch's on a disposed Mac is: discovery takes another device (D53).
                let execution = self.execution.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(refusal) = execution.refusal_error(&self.serial) {
                    return Err(refusal);
                }
                let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(previous) = sessions.last() {
                    previous.check_closed(&self.serial.to_owned())?;
                }
                // Live from here, before the locks are let go: a seal that comes next counts it.
                session.claim();
                sessions.retain(|s| s.ended() != Some(true));
                sessions.push(session.clone());
            }
            // D93: a session a batch opens is that batch's, for the Mac's stop.
            let _ = BATCH.try_with(|batch| {
                batch
                    .sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(session.clone())
            });
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
                .measure_delivery(
                    PathBuf::from(src.to_string_lossy().into_owned()),
                    self.bridge.send_with(src, dst, self.send_stop(context)),
                    false,
                )
                .await
                .map_err(|error| self.leaving_send(error))
        })
    }
    fn pull_file<'a>(&'a self, src: &'a str, dst: &'a Path, _context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            let received = self
                .measurement()
                .measure_delivery(
                    PathBuf::from(dst.to_string_lossy().into_owned()),
                    self.bridge.receive_with(src, dst, self.receive_stop()),
                    false,
                )
                .await;
            self.gated_pull(received, src)
        })
    }
    fn push_folder<'a>(&'a self, src: &'a Path, dst: &'a str, context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.measurement()
                .measure_delivery(
                    PathBuf::from(src.to_string_lossy().into_owned()),
                    self.bridge.send_with(src, dst, self.send_stop(context)),
                    true,
                )
                .await
                .map_err(|error| self.leaving_send(error))
        })
    }
    fn pull_folder<'a>(&'a self, src: &'a str, dst: &'a Path, _context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            let received = self
                .measurement()
                .measure_delivery(
                    PathBuf::from(dst.to_string_lossy().into_owned()),
                    self.bridge.receive_with(src, dst, self.receive_stop()),
                    true,
                )
                .await;
            self.gated_pull(received, src)
        })
    }
    fn pull_bundle<'a>(&'a self, src: &'a str, dst: &'a Path, _context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            let received = self
                .measurement()
                .measure_delivery(
                    PathBuf::from(dst.to_string_lossy().into_owned()),
                    self.bridge.receive_bundle(src, dst, self.receive_stop()),
                    true,
                )
                .await;
            self.gated_pull(received, src)
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
                    marathon_test_support::recorded_errors::failure_kind(
                        cause["class"].as_str().unwrap(),
                        cause["message"].as_str(),
                    )
                    .unwrap_or_else(|e| panic!("{e}")),
                    cause["message"].as_str().unwrap(),
                )),
                reason,
                suppressed: Box::default(),
            });
            let mut chain = vec![];
            let mut current = Some(error.failure());
            while let Some(f) = current {
                chain.push(json!({"class":f.kind.identifier(),"message":f.message.clone()}));
                current = f.cause.as_deref();
            }
            let mut expected = row["error"].clone();
            if reason == Some(DeviceFailureReason::IncompatibleDevice) {
                // classify_run embeds the cause's rendering in this generated wrapper.
                expected[0]["message"] =
                    marathon_test_support::recorded_errors::rendered(expected[0]["message"].as_str().unwrap())
                        .unwrap_or_else(|error| panic!("{error}"))
                        .into();
            }
            marathon_test_support::recorded_errors::error_chain(&mut expected);
            assert_eq!(json!(chain), expected, "{case}");
        }
    }
}
