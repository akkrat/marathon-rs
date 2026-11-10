//! Port of vendor/vendor-apple/macos/src/test/kotlin/com/malinskiy/marathon/apple/macos/MacosTestRootTest.kt (bundle builders and recording bridge).
#![allow(dead_code)]
use marathon_logging::{LogSettings, Vendor};

use marathon_error::{FailureKind, InjectedKind};
pub mod runtime;
use jcompat::{file_manager::FileManager, jtime::instant_of};
use marathon_apple::{
    apple_device_registry::AppleDeviceRegistry,
    bin::AppleBinaryEnvironment,
    cmd::shared_transfers::SharedTransfers,
    cmd::{
        BaseCommand, CommandExecutor, CommandHost, CommandRequest, CommandResult, CommandSession, FileBridge,
        SessionControl,
    },
    device::connection_factory::{Bridge, Executor},
};
use marathon_config::{
    VendorConfiguration,
    binding::{BindingContext, Profile},
    devices::Transport,
    loader::MarathonConfigurationLoader,
};
use marathon_core::{
    analytics::Track,
    artifacts::FileManagerOutput,
    error::{VendorError, VendorResult},
    executor::{
        hooks::RuntimeHooks,
        shared::{SharedDeferred, Signal},
    },
    logging::LogQueue,
    model::{DevicePoolId, TestBatch, TestBatchResults},
    runtime::{VendorFuture, VendorRuntime},
    time::SystemTimer,
    vendor::{Device, VendorContext},
    wiring::VendorDependencies,
};
use marathon_macos::{
    AppleMacosProvider, MacosDevice, MacosDeviceOptions, device::CANCELLED_BATCH_TIMEOUT,
    installer::MacosApplicationInstaller, provider::MacosConnections,
};
use marathon_test_support::apple::{FakeSimulatorWorker, fake_simulator_worker::ok};
use std::path::PathBuf;
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::Notify;
pub type R<T = ()> = Result<T, Box<dyn std::error::Error>>;
pub fn v<T, E: std::fmt::Debug>(result: Result<T, E>) -> R<T> {
    result.map_err(|e| std::io::Error::other(format!("{e:?}")).into())
}

pub struct Worker {
    pub fake: FakeSimulatorWorker,
    pub requests: Mutex<Vec<CommandRequest>>,
}
impl CommandExecutor for Worker {
    fn host(&self) -> &CommandHost {
        self.fake.host()
    }
    fn component(&self) -> &marathon_logging::Component {
        self.fake.component()
    }
    fn connected(&self) -> bool {
        self.fake.connected()
    }
    fn log_error(&self, message: String) {
        self.fake.log_error(message);
    }
    fn execute(&self, request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandSession> {
        self.requests.lock().unwrap().push(request.clone());
        self.fake.execute(request, context)
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        self.fake.close()
    }
    fn connection_lost(&self) -> std::pin::Pin<Box<dyn Future<Output = marathon_core::error::Failure> + Send>> {
        self.fake.connection_lost()
    }
}
#[derive(Default)]
pub struct RecordingBridge {
    pub sent: Mutex<Vec<(String, Vec<u8>)>>,
    pub pulls: Mutex<Vec<String>>,
    pub pull_entered: Notify,
    pub pull_release: Notify,
    pub block_pull: std::sync::atomic::AtomicBool,
    /// Every send ends as a stalled one does, counted.
    pub stalled_sends: std::sync::atomic::AtomicUsize,
    pub stalled: std::sync::atomic::AtomicBool,
    /// D54: every transfer runs until it is stopped, and ends as the rsync bridge ends it.
    pub hang: std::sync::atomic::AtomicBool,
    pub hanging: Notify,
}
impl RecordingBridge {
    async fn until_stopped(&self, mut stop: marathon_apple::cmd::TransferStop, receive: bool) -> VendorResult<bool> {
        use marathon_apple::cmd::TransferStopped;
        self.hanging.notify_one();
        match stop.stopped().await {
            TransferStopped::Cancelled(reason) => Err(VendorError::Cancelled(reason)),
            TransferStopped::Lost(_) if receive => Ok(false),
            TransferStopped::Lost(failure) => Err(VendorError::Other(failure)),
        }
    }
}
impl FileBridge for RecordingBridge {
    fn send_with<'a>(
        &'a self,
        src: &'a Path,
        dst: &'a str,
        stop: marathon_apple::cmd::TransferStop,
    ) -> VendorFuture<'a, bool> {
        if self.hang.load(Ordering::SeqCst) {
            Box::pin(self.until_stopped(stop, false))
        } else {
            self.send(src, dst)
        }
    }
    fn receive_with<'a>(
        &'a self,
        src: &'a str,
        dst: &'a Path,
        stop: marathon_apple::cmd::TransferStop,
    ) -> VendorFuture<'a, bool> {
        if self.hang.load(Ordering::SeqCst) {
            Box::pin(self.until_stopped(stop, true))
        } else {
            self.receive(src, dst)
        }
    }
    fn send<'a>(&'a self, src: &'a Path, dst: &'a str) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            if self.stalled.load(Ordering::SeqCst) {
                self.stalled_sends.fetch_add(1, Ordering::SeqCst);
                return Err(VendorError::LeavePool {
                    reason: marathon_core::error::LeavePoolReason::TransferStalled,
                    detail: marathon_apple::cmd::failure(
                        FailureKind::Injected(InjectedKind::TransferStalled),
                        "no progress",
                    ),
                });
            }
            let bytes = if src.is_file() {
                std::fs::read(src)
                    .map_err(|e| VendorError::Other(marathon_apple::cmd::failure(FailureKind::Io, e.to_string())))?
            } else {
                vec![]
            };
            self.sent.lock().unwrap().push((dst.into(), bytes));
            Ok(true)
        })
    }
    fn receive<'a>(&'a self, src: &'a str, dst: &'a Path) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.pulls.lock().unwrap().push(src.into());
            self.pull_entered.notify_one();
            if self.block_pull.load(Ordering::SeqCst) {
                self.pull_release.notified().await;
            }
            std::fs::create_dir_all(dst.join(src.rsplit('/').next().unwrap_or(src)))
                .map_err(|e| VendorError::Other(marathon_apple::cmd::failure(FailureKind::Io, e.to_string())))?;
            Ok(true)
        })
    }
}
pub struct Connections {
    pub worker: Arc<Worker>,
    pub bridge: Arc<RecordingBridge>,
    pub creates: AtomicUsize,
    pub releases: AtomicUsize,
    pub null: std::sync::atomic::AtomicBool,
}
impl MacosConnections for Connections {
    fn create(&self, _: Transport, _: VendorContext) -> VendorFuture<'_, (Option<Executor>, Bridge)> {
        Box::pin(async move {
            self.creates.fetch_add(1, Ordering::SeqCst);
            Ok((
                (!self.null.load(Ordering::SeqCst)).then(|| self.worker.clone() as Executor),
                self.bridge.clone() as Bridge,
            ))
        })
    }
    fn release(&self, _: Executor) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.releases.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
pub struct Harness {
    pub root: tempfile::TempDir,
    pub options: MacosDeviceOptions,
    pub context: VendorContext,
    pub worker: Arc<Worker>,
    pub bridge: Arc<RecordingBridge>,
    pub logs: LogQueue,
}
impl Harness {
    pub fn new(extra: &str) -> R<Self> {
        let root = tempfile::tempdir()?;
        for (dir, name) in [("MacApp.app", "MacApp"), ("UITests.xctest", "UITests")] {
            let contents = root.path().join(dir).join("Contents");
            std::fs::create_dir_all(contents.join("MacOS"))?;
            std::fs::write(
                contents.join("Info.plist"),
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>com.example.{name}</string><key>CFBundleName</key><string>{name}</string><key>CFBundleSupportedPlatforms</key><array><string>MacOSX</string></array><key>LSMinimumSystemVersion</key><string>14.0</string></dict></plist>"
                ),
            )?;
            std::fs::write(contents.join("MacOS").join(name), [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0])?;
        }
        let path = PathBuf::from(root.path().to_string_lossy().into_owned());
        let bind = BindingContext {
            profile: Profile::Marathonfile,
            marathonfile: PathBuf::from(root.path().join("Marathonfile").to_string_lossy().into_owned()),
            marathonfile_dir: path.clone(),
            process_dir: path.clone(),
            environment: vec![],
            now: instant_of(0, 0).unwrap(),
            analytics_tracking: false,
            bugsnag_reporting: false,
        };
        let yaml = format!(
            "name: test\noutputDir: output\nvendorConfiguration:\n  type: macOS\n  bundle:\n    application: MacApp.app\n    testApplication: UITests.xctest\n    testType: xcuitest\n  xcresult:\n    pullingPolicy: NEVER\n    remoteClean: false\n{extra}"
        );
        let config = Arc::new(v(MarathonConfigurationLoader.parse_sync(yaml.as_bytes(), &bind))?);
        let VendorConfiguration::Macos(vendor) = &config.vendor_configuration else {
            unreachable!()
        };
        let vendor = Arc::new(vendor.clone());
        let io: Arc<dyn VendorRuntime> = Arc::new(runtime::Runtime);
        let clock = Arc::new(runtime::Clock::default());
        let hooks = RuntimeHooks::live();
        let logs = LogQueue::default();
        let registry = Arc::new(Mutex::new(LogSettings::default()));
        {
            let mut r = registry.lock().unwrap();
            r.debug = true;
            r.configure(true, Vendor::Apple { compact_output: false });
        }
        let timer = Arc::new(v(SystemTimer::new(hooks.clone()))?);
        let files = FileManager::new(
            0,
            0,
            &root.path().join("output").to_string_lossy(),
            &root.path().to_string_lossy(),
        );
        let output = Arc::new(FileManagerOutput::new(
            io.clone(),
            FileManager::new(
                0,
                0,
                &root.path().join("output").to_string_lossy(),
                &root.path().to_string_lossy(),
            ),
            config.output_dir.clone(),
            registry
                .lock()
                .unwrap()
                .component("com.malinskiy.marathon.io.FileManager"),
            Arc::new(|_, _, _, _| {}),
        ));
        let deps = VendorDependencies {
            logs: logs.sender(),
            wall: Arc::new(|| 1234),
            log: {
                let logs = logs.sender();
                Arc::new(move |logger, level, message, cause| {
                    logs.push(marathon_core::logging::LogRecord {
                        target: logger.target().to_owned(),
                        logger: logger.name().into(),
                        at_millis: 0,
                        wall_millis: 1234,
                        level,
                        message: message.to_string(),
                        cause: cause.cloned(),
                    });
                })
            },
            configuration: config,
            io: io.clone(),
            clock: clock.clone(),
            logging: registry.clone(),
            hooks: hooks.clone(),
            timer,
            tracker: Arc::new(Track::new(io.clone(), hooks.clone())),
            files: output,
            barriers: hooks.barriers(),
        };
        let logging = marathon_macos::vendor::logging(&deps);
        let fake = FakeSimulatorWorker::new(io.clone(), &mut registry.lock().unwrap());
        *fake.before.lock().unwrap() = Arc::new(|cmd| {
            Box::pin(async move {
                Ok(Some(match cmd.first().map(String::as_str) {
                    Some("env") => ok(["HOME=/Users/worker", "USER=tester", " A = first ", " A = last ", "bare"]),
                    Some("sh") if cmd.iter().any(|s| s.contains("system_profiler")) => ok(["Provisioning UDID: udid"]),
                    Some("sh") if cmd.iter().any(|s| s.contains("IOPlatformUUID")) => ok(["fallback"]),
                    Some("sh") if cmd.iter().any(|s| s.contains("manufacturer")) => ok(["Apple"]),
                    Some("sh") => ok(["Mac14,6"]),
                    Some("sw_vers") => ok(["15.0"]),
                    Some("uname") => ok(["arm64"]),
                    Some("csrutil") => ok(["disabled"]),
                    Some("lipo") => ok(["arm64"]),
                    Some("test") => CommandResult {
                        stdout: vec![],
                        stderr: vec![],
                        exit_code: Some(1),
                    },
                    Some("xcrun") if cmd.get(1).is_some_and(|s| s == "--sdk") => {
                        ok(["/Xcode/Platforms/MacOSX.platform"])
                    }
                    Some("xcrun") if cmd.get(2).is_some_and(|s| s == "-version") => {
                        ok(["Xcode 16.4", "Build version 16F6"])
                    }
                    Some("xcrun") if cmd.get(1).is_some_and(|s| s == "xcresulttool") => CommandResult {
                        stdout: vec![],
                        stderr: vec![],
                        exit_code: Some(1),
                    },
                    _ => ok(Vec::<String>::new()),
                }))
            })
        });
        let worker = Arc::new(Worker {
            fake,
            requests: Mutex::default(),
        });
        let bridge = Arc::new(RecordingBridge::default());
        let context = VendorContext {
            cancellation: Signal::new(),
            clock,
            hooks,
        };
        let options = MacosDeviceOptions {
            dependencies: deps,
            vendor,
            logging: logging.clone(),
            files: Arc::new(files),
            shared_transfers: Arc::new(SharedTransfers::new(io, logging)),
            installer: Arc::new(MacosApplicationInstaller::default()),
            session_close_timeout: marathon_apple::xcodebuild_session::CLOSE_TIMEOUT,
            cancelled_batch_timeout: CANCELLED_BATCH_TIMEOUT,
        };
        Ok(Self {
            root,
            options,
            context,
            worker,
            bridge,
            logs,
        })
    }
    pub fn device(&self) -> Arc<MacosDevice> {
        let binary = AppleBinaryEnvironment::new(
            self.worker.clone(),
            &self.options.dependencies.configuration,
            self.options.vendor.timeout_configuration.clone(),
            self.options.dependencies.io.clone(),
            self.options.logging.clone(),
        );
        MacosDevice::new(
            "udid".into(),
            Transport::Local,
            binary,
            self.bridge.clone(),
            self.options.clone(),
        )
    }
    pub fn provider(&self) -> (Arc<AppleMacosProvider>, Arc<Connections>) {
        let connections = Arc::new(Connections {
            worker: self.worker.clone(),
            bridge: self.bridge.clone(),
            creates: AtomicUsize::new(0),
            releases: AtomicUsize::new(0),
            null: std::sync::atomic::AtomicBool::new(false),
        });
        let provider = AppleMacosProvider::new(
            self.options.clone(),
            Arc::new(AppleDeviceRegistry::default()),
            connections.clone(),
            PathBuf::from(self.root.path().to_string_lossy().into_owned()),
            vec![],
        );
        (provider, connections)
    }
}
pub async fn batch(device: Arc<MacosDevice>, context: VendorContext) -> VendorResult<()> {
    execute(device, context, SharedDeferred::new()).await
}
pub async fn execute(
    device: Arc<MacosDevice>,
    context: VendorContext,
    result: Arc<SharedDeferred<TestBatchResults>>,
) -> VendorResult<()> {
    device
        .execute(
            device.options.dependencies.configuration.clone(),
            DevicePoolId { name: "pool".into() },
            Arc::new(TestBatch {
                id: "batch".into(),
                tests: vec![],
            }),
            result,
            context,
        )
        .await
}
pub struct Running {
    pub opened: Arc<Notify>,
    pub closing: Arc<Notify>,
    pub release: Arc<Notify>,
    pub events: Arc<Mutex<Vec<String>>>,
    pub started: Arc<AtomicUsize>,
}
impl Running {
    pub fn install(h: &Harness, lines: Vec<String>) -> Self {
        let opened = Arc::new(Notify::new());
        let closing = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let events = Arc::new(Mutex::new(vec![]));
        let started = Arc::new(AtomicUsize::new(0));
        let (op, cl, re, ev, st) = (
            opened.clone(),
            closing.clone(),
            release.clone(),
            events.clone(),
            started.clone(),
        );
        let runtime = h.options.dependencies.io.clone();
        *h.worker.fake.session.lock().unwrap() = Arc::new(move |cmd| {
            if cmd.get(2).is_none_or(|s| s != "test-without-building") {
                return Ok(None);
            }
            let n = st.fetch_add(1, Ordering::SeqCst) + 1;
            ev.lock().unwrap().push(format!("xcodebuild {n} started"));
            op.notify_one();
            let base = BaseCommand::new(100, runtime.clone());
            let output = base.stdout.clone();
            let lines = lines.clone();
            base.scope.track(runtime.spawn(Box::pin(async move {
                for line in lines {
                    output.send(line).await?;
                }
                Ok(())
            })));
            let (cl, re, ev) = (cl.clone(), re.clone(), ev.clone());
            Ok(Some(CommandSession::new(
                base,
                SessionControl {
                    is_alive: Box::new(|| true),
                    terminate: Box::new(|| Ok(())),
                    kill: Box::new(|| Ok(())),
                    close: Box::new(move || {
                        let (cl, re, ev) = (cl.clone(), re.clone(), ev.clone());
                        Box::pin(async move {
                            cl.notify_one();
                            re.notified().await;
                            ev.lock().unwrap().push(format!("xcodebuild {n} closed"));
                            Ok(())
                        })
                    }),
                },
            )))
        });
        Self {
            opened,
            closing,
            release,
            events,
            started,
        }
    }
}
