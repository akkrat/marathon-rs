//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/StartupPhasesTest.kt
#![allow(dead_code)]
use crate::support::*;
use jcompat::{
    file_manager::{FileManager, JavaPath},
    jtime::instant_of,
};
use marathon_apple::{
    apple_test_bundle_identifier::AppleTestBundleIdentifier,
    artifact_processor::ArtifactProcessor,
    bin::{AppleBinaryEnvironment, Tools},
    cmd::{CommandResult, shared_transfers::SharedTransfers},
    device::connection_factory::Executor,
};
use marathon_config::{
    VendorConfiguration,
    bind::bind_configuration,
    binder::Binder,
    binding::{BindingContext, Profile},
    devices::Transport,
    vendor::*,
};
use marathon_core::{analytics::Track, time::SystemTimer, vendor::Device};
use marathon_ios::{
    apple_simulator_device::*,
    device::{simulator_factory::*, worker_provisioning_context::*},
};
use marathon_test_support::apple::{FakeSimulatorWorker, fake_simulator_worker::ok};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
};

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub struct Boot {
    pub state: AtomicI32,
    pub boots: AtomicUsize,
    pub shutdowns: AtomicUsize,
    pub reads: AtomicUsize,
    pub polls: AtomicUsize,
    pub finish_after: AtomicUsize,
    pub exit: AtomicI32,
    pub fail_first: AtomicBool,
    pub shutdown_hangs: AtomicBool,
    pub polled: tokio::sync::Notify,
}
impl Default for Boot {
    fn default() -> Self {
        Self {
            state: 1.into(),
            boots: 0.into(),
            shutdowns: 0.into(),
            reads: 0.into(),
            polls: 0.into(),
            finish_after: 1.into(),
            exit: 0.into(),
            fail_first: false.into(),
            shutdown_hangs: false.into(),
            polled: tokio::sync::Notify::new(),
        }
    }
}
impl Boot {
    pub fn answer(&self, command: &[String]) -> Option<CommandResult> {
        let cmd: Vec<_> = command.iter().map(String::as_str).collect();
        match cmd.as_slice() {
            ["cat", _] => {
                self.reads.fetch_add(1, Ordering::SeqCst);
                let state = self.state.load(Ordering::SeqCst);
                Some(if state < 0 {
                    CommandResult {
                        stdout: vec![],
                        stderr: vec![],
                        exit_code: Some(1),
                    }
                } else {
                    ok([format!(
                        "<plist><dict><key>state</key><integer>{state}</integer><key>deviceType</key><string>fallback-type</string></dict></plist>"
                    )])
                })
            }
            ["xcrun", "simctl", "boot", _] => {
                self.boots.fetch_add(1, Ordering::SeqCst);
                if self.exit.load(Ordering::SeqCst) == 0 {
                    self.state.store(3, Ordering::SeqCst);
                }
                Some(CommandResult {
                    stdout: vec![],
                    stderr: vec![],
                    exit_code: Some(self.exit.load(Ordering::SeqCst)),
                })
            }
            ["xcrun", "simctl", "shutdown", _] => {
                self.shutdowns.fetch_add(1, Ordering::SeqCst);
                if self.state.load(Ordering::SeqCst) >= 0 {
                    self.state.store(1, Ordering::SeqCst);
                }
                self.polls.store(0, Ordering::SeqCst);
                Some(ok(Vec::<String>::new()))
            }
            ["xcrun", "simctl", "spawn", ..] => {
                let count = self.polls.fetch_add(1, Ordering::SeqCst) + 1;
                self.polled.notify_one();
                let finish = self.finish_after.load(Ordering::SeqCst);
                Some(ok([
                    if count >= finish
                        && !(self.fail_first.load(Ordering::SeqCst) && self.boots.load(Ordering::SeqCst) == 1)
                    {
                        "A com.apple.springboard.services"
                    } else {
                        "D com.apple.springboard.services"
                    },
                ]))
            }
            ["env"]
            | ["uname", _]
            | ["/usr/sbin/system_profiler", ..]
            | ["xcrun", "xcodebuild", "-version"]
            | ["xcrun", "simctl", "getenv", ..]
            | ["xcrun", "simctl", "list", ..]
            | ["xcrun", "simctl", "create", ..]
            | ["xcrun", "simctl", "delete", ..] => None,
            ["lipo", ..] => Some(ok(["arm64"])),
            ["xcrun", "xcresulttool", ..] => Some(CommandResult {
                stdout: vec![],
                stderr: vec![],
                exit_code: Some(1),
            }),
            _ => Some(ok(Vec::<String>::new())),
        }
    }
}
pub struct Harness {
    pub dir: tempfile::TempDir,
    pub worker: Arc<FakeSimulatorWorker>,
    pub boot: Arc<Boot>,
    pub clock: Arc<Clock>,
    pub services: SimulatorServices,
    pub logs: Logs,
}
impl Harness {
    pub fn new(edit: impl FnOnce(&mut IosConfiguration)) -> TestResult<Self> {
        let dir = tempfile::tempdir()?;
        for (folder, name) in [("app.app", "App"), ("tests.xctest", "Tests")] {
            let root = dir.path().join(folder);
            std::fs::create_dir_all(&root)?;
            std::fs::write(
                root.join("Info.plist"),
                format!(
                    "<plist><dict><key>CFBundleIdentifier</key><string>com.example.{name}</string><key>CFBundleName</key><string>{name}</string><key>CFBundleExecutable</key><string>{name}</string><key>CFBundleSupportedPlatforms</key><array><string>iPhoneSimulator</string></array></dict></plist>"
                ),
            )?;
            std::fs::write(root.join(name), [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0])?;
        }
        let ctx = BindingContext {
            profile: Profile::Marathonfile,
            marathonfile: JavaPath::from_text("/Marathonfile"),
            marathonfile_dir: JavaPath::from_text("/"),
            process_dir: JavaPath::from_text("/"),
            environment: vec![],
            now: instant_of(0, 0).unwrap(),
            analytics_tracking: false,
            bugsnag_reporting: false,
        };
        let stream =
            marathon_config::node::load("name: test\noutputDir: /tmp/output\nvendorConfiguration:\n  type: iOS\n");
        let mut configuration = Binder::new(&ctx, &stream)
            .first(&stream, |v| {
                bind_configuration(v, marathon_config::bind_vendor::bind_vendor_configuration)
            })
            .map_err(|e| format!("{e:?}"))?
            .flatten()
            .unwrap();
        let VendorConfiguration::Ios(mut vendor) = configuration.vendor_configuration.clone() else {
            panic!("iOS");
        };
        let path = |s: &str| JavaPath::from_text(dir.path().join(s).to_str().unwrap());
        let mut bundle = AppleTestBundleConfiguration::unresolved(
            Some(path("app.app")),
            Some(path("tests.xctest")),
            None,
            None,
            Some(TestType::Xcuitest),
        );
        bundle.app = Some(path("app.app"));
        bundle.xctest = Some(path("tests.xctest"));
        vendor.bundle = Some(bundle);
        vendor.lifecycle_configuration.on_prepare.clear();
        vendor.lifecycle_configuration.on_dispose.clear();
        vendor
            .screen_record_configuration
            .video_configuration
            .transcoding
            .enabled = false;
        vendor.xcresult.pulling_policy = PullingPolicy::Never;
        vendor.timeout_configuration.boot_wait = duration(500);
        edit(&mut vendor);
        configuration.vendor_configuration = VendorConfiguration::Ios(vendor.clone());
        let clock = Arc::new(Clock::default());
        let (logging, logs) = logging();
        let worker = Arc::new(FakeSimulatorWorker::new(
            Arc::new(Runtime),
            &mut logging.registry.lock().unwrap(),
        ));
        worker.add(["a", "b"]);
        let boot = Arc::new(Boot::default());
        let plan = boot.clone();
        *worker.before.lock().unwrap() = Arc::new(move |cmd| {
            let result = plan.answer(&cmd);
            Box::pin(async move { Ok(result) })
        });
        let artifacts = ArtifactProcessor::new(8, 2, 4, Arc::new(Runtime), clock.clone(), logging.clone()).unwrap();
        let services = SimulatorServices {
            configuration: Arc::new(configuration),
            vendor: Arc::new(vendor),
            runtime: Arc::new(Runtime),
            context: marathon_core::vendor::VendorContext {
                cancellation: Arc::new(marathon_apple::cmd::SessionScope::default()),
                clock: clock.clone(),
                hooks: clock.clone(),
            },
            monotonic: clock.clone(),
            wall: clock.clone(),
            timer: Arc::new(SystemTimer::new(clock.clone()).unwrap()),
            output: Arc::new(marathon_core::artifacts::FileManagerOutput::new(
                Arc::new(Runtime),
                FileManager::new(0, 0, dir.path().to_str().unwrap(), "/"),
                JavaPath::from_text(dir.path().to_str().unwrap()),
                logging.logger("com.malinskiy.marathon.io.FileManager"),
                {
                    let log = logging.clone();
                    Arc::new(move |logger, level, message, error| {
                        if log.enabled(logger, level) {
                            (log.sink)(logger, level, message.to_string_lossy(), error.cloned());
                        }
                    })
                },
            )),
            logs: marathon_core::logging::LogQueue::default().sender(),
            result_wall: {
                let clock = clock.clone();
                Arc::new(move || clock.now())
            },
            listeners: Arc::new(MinimalSimulatorListenerFactory),
            files: Arc::new(FileManager::new(0, 0, dir.path().to_str().unwrap(), "/")),
            identifier: Arc::new(AppleTestBundleIdentifier::default()),
            artifacts,
            track: Track::new(Arc::new(Runtime), clock.clone()),
            logging,
            timeouts: SimulatorTimeouts {
                session_close_millis: 100,
                cancelled_batch_millis: 200,
                boot_retry_pause_millis: 10,
                boot_poll_interval_millis: 20,
            },
        };
        Ok(Self {
            dir,
            worker,
            boot,
            clock,
            services,
            logs,
        })
    }
    pub fn binary(&self, executor: Executor) -> Arc<AppleBinaryEnvironment> {
        Arc::new(AppleBinaryEnvironment::with_tools(
            Tools {
                executor,
                timeouts: Arc::new(self.services.vendor.timeout_configuration.clone()),
                runtime: Arc::new(Runtime),
                logging: self.services.logging.clone(),
            },
            self.services.configuration.test_batch_timeout_millis,
            self.services.configuration.test_output_timeout_millis,
        ))
    }
    pub async fn device(&self) -> TestResult<Arc<AppleSimulatorDevice>> {
        self.device_on(
            self.worker.clone(),
            "a",
            Arc::new(WorkerProvisioningContext::new(Transport::Local, self.clock.clone())),
        )
        .await
    }
    pub async fn device_on(
        &self,
        executor: Executor,
        udid: &str,
        cycle: Arc<WorkerProvisioningContext>,
    ) -> TestResult<Arc<AppleSimulatorDevice>> {
        self.device_on_bridge(executor, udid, cycle, Arc::new(BridgeMock)).await
    }
    pub async fn device_on_bridge(
        &self,
        executor: Executor,
        udid: &str,
        cycle: Arc<WorkerProvisioningContext>,
        bridge: marathon_apple::device::connection_factory::Bridge,
    ) -> TestResult<Arc<AppleSimulatorDevice>> {
        let bin = self.binary(executor.clone());
        let record = cycle.simulator(udid, &bin, false, &self.services.context).await?;
        let built = AppleSimulatorConstructor(self.services.clone()).construct(SimulatorConstruction {
            transport: Transport::Local,
            executor,
            bridge,
            bin,
            record: record.clone(),
            sdk: marathon_apple::model::Sdk::IphoneSimulator,
            shared_transfers: Arc::new(SharedTransfers::new(Arc::new(Runtime), self.services.logging.clone())),
        })?;
        assert!(Arc::ptr_eq(&built.device.0, &(built.handle.clone() as Arc<dyn Device>)));
        built
            .setup
            .setup(&record, cycle, self.services.context.clone())
            .await
            .map_err(|e| e.failure().to_java_string().to_string_lossy())?;
        Ok(built.handle)
    }
    pub fn factory(&self) -> Arc<SimulatorFactory<Arc<AppleSimulatorDevice>>> {
        let services = self.services.clone();
        Arc::new(SimulatorFactory::new(
            Arc::new(move |executor| {
                Arc::new(AppleBinaryEnvironment::with_tools(
                    Tools {
                        executor,
                        timeouts: Arc::new(services.vendor.timeout_configuration.clone()),
                        runtime: services.runtime.clone(),
                        logging: services.logging.clone(),
                    },
                    5000,
                    5000,
                ))
            }),
            Arc::new(AppleSimulatorConstructor(self.services.clone())),
            Arc::new(SharedTransfers::new(Arc::new(Runtime), self.services.logging.clone())),
            self.services.track.clone(),
            marathon_ios::device::timing::ProvisioningTiming::new(
                self.services.timer.clone(),
                self.services.logging.clone(),
            ),
        ))
    }
    pub async fn prepare(&self, device: &AppleSimulatorDevice) -> marathon_core::error::VendorResult<()> {
        device
            .prepare(self.services.configuration.clone(), self.services.context.clone())
            .await
    }
    pub async fn finish(&self, devices: &[Arc<AppleSimulatorDevice>]) {
        for device in devices {
            device.dispose().await.unwrap();
            device.join_retained().await.unwrap();
        }
        self.services
            .artifacts
            .close_and_join(1000, 1000, &self.services.context)
            .await
            .unwrap();
    }
}
pub fn failing() -> CommandResult {
    CommandResult {
        stdout: vec![],
        stderr: vec!["failure".into()],
        exit_code: Some(1),
    }
}

#[derive(Clone, Default)]
pub struct OutputPlan {
    pub lines: Vec<String>,
    pub failure: Option<marathon_core::error::Failure>,
    pub close_gate: Option<Arc<marathon_core::executor::shared::SharedDeferred<()>>>,
    pub hang: bool,
}
pub struct BatchScript {
    pub plan: Mutex<OutputPlan>,
    pub opened: Arc<marathon_core::executor::shared::SharedDeferred<()>>,
    pub closing: Arc<marathon_core::executor::shared::SharedDeferred<()>>,
    pub records: Arc<Mutex<Vec<String>>>,
    pub count: AtomicUsize,
}
impl Default for BatchScript {
    fn default() -> Self {
        Self {
            plan: Mutex::default(),
            opened: marathon_core::executor::shared::SharedDeferred::new(),
            closing: marathon_core::executor::shared::SharedDeferred::new(),
            records: Arc::default(),
            count: 0.into(),
        }
    }
}
impl BatchScript {
    pub fn attach(self: &Arc<Self>, worker: &FakeSimulatorWorker) {
        use marathon_apple::cmd::{BaseCommand, CommandSession, SessionControl};
        use marathon_core::runtime::{CancellationSignal, Deferred, VendorRuntime};
        let script = self.clone();
        *worker.session.lock().unwrap() = Arc::new(move |cmd| {
            if cmd.get(2).is_none_or(|s| s != "test-without-building") {
                return Ok(None);
            }
            let n = script.count.fetch_add(1, Ordering::SeqCst) + 1;
            script.records.lock().unwrap().push(format!("xcodebuild {n} started"));
            script.opened.complete(Ok(()));
            let plan = script.plan.lock().unwrap().clone();
            let base = BaseCommand::new(128, Arc::new(Runtime));
            let stdout = base.stdout.clone();
            let stderr = base.stderr.clone();
            let exit = base.exit_code.clone();
            let scope = base.scope.clone();
            let lines = plan.lines;
            let failure = plan.failure;
            let hang = plan.hang;
            let pump = Runtime.spawn(Box::pin(async move {
                for line in lines {
                    stdout.send(line).await?;
                }
                if hang {
                    scope.cancelled().await;
                }
                stdout.close(failure);
                stderr.close(None);
                exit.complete(Ok(Some(65)));
                Ok(())
            }));
            base.scope.track(pump);
            let gate = plan.close_gate;
            let closing = script.closing.clone();
            let records = script.records.clone();
            Ok(Some(CommandSession::new(
                base,
                SessionControl {
                    is_alive: Box::new(move || hang),
                    terminate: Box::new(|| Ok(())),
                    kill: Box::new(|| Ok(())),
                    close: Box::new(move || {
                        let gate = gate.clone();
                        let closing = closing.clone();
                        let records = records.clone();
                        Box::pin(async move {
                            closing.complete(Ok(()));
                            if let Some(gate) = gate {
                                gate.wait().await?;
                            }
                            records.lock().unwrap().push(format!("xcodebuild {n} closed"));
                            Ok(())
                        })
                    }),
                },
            )))
        });
    }
    pub fn output(&self, lines: Vec<String>) {
        *self.plan.lock().unwrap() = OutputPlan {
            lines,
            ..Default::default()
        };
    }
}
pub fn tests() -> Vec<marathon_core::model::Test> {
    (1..=2)
        .map(|n| marathon_core::model::Test {
            pkg: "Tests".into(),
            clazz: "Suite".into(),
            method: format!("test{n}").into(),
            meta_properties: vec![],
        })
        .collect()
}
pub fn passed(n: usize) -> Vec<String> {
    vec![
        format!("Test Case '-[Suite test{n}]' started."),
        format!("Test Case '-[Suite test{n}]' passed (0.100 seconds)."),
    ]
}
pub fn failed(n: usize) -> Vec<String> {
    vec![
        format!("Test Case '-[Suite test{n}]' started."),
        format!("Test Case '-[Suite test{n}]' failed (0.100 seconds)."),
    ]
}
static BATCH_ID: AtomicUsize = AtomicUsize::new(1);
pub async fn run(
    h: &Harness,
    d: &AppleSimulatorDevice,
) -> (
    marathon_core::error::VendorResult<()>,
    Option<Arc<marathon_core::model::TestBatchResults>>,
) {
    run_with(h, d, h.services.context.clone()).await
}
pub async fn run_with(
    h: &Harness,
    d: &AppleSimulatorDevice,
    context: marathon_core::vendor::VendorContext,
) -> (
    marathon_core::error::VendorResult<()>,
    Option<Arc<marathon_core::model::TestBatchResults>>,
) {
    use marathon_core::runtime::Deferred;
    let result = marathon_core::executor::shared::SharedDeferred::new();
    let batch = Arc::new(marathon_core::model::TestBatch {
        tests: tests(),
        id: format!("batch-{}", BATCH_ID.fetch_add(1, Ordering::SeqCst)).into(),
    });
    let outcome = d
        .execute(
            h.services.configuration.clone(),
            marathon_core::model::DevicePoolId { name: "pool".into() },
            batch,
            result.clone(),
            context,
        )
        .await;
    (outcome, result.completed().and_then(Result::ok))
}
pub async fn prepared(h: &Harness) -> TestResult<(Arc<AppleSimulatorDevice>, Arc<BatchScript>)> {
    let d = h.device().await?;
    h.prepare(&d)
        .await
        .map_err(|e| e.failure().to_java_string().to_string_lossy())?;
    let script = Arc::new(BatchScript::default());
    script.attach(&h.worker);
    Ok((d, script))
}

pub fn spawn_batch(
    h: &Harness,
    d: Arc<AppleSimulatorDevice>,
    context: marathon_core::vendor::VendorContext,
) -> tokio::task::JoinHandle<(
    marathon_core::error::VendorResult<()>,
    Option<Arc<marathon_core::model::TestBatchResults>>,
)> {
    use marathon_core::runtime::Deferred;
    let configuration = h.services.configuration.clone();
    tokio::spawn(async move {
        let result = marathon_core::executor::shared::SharedDeferred::new();
        let batch = Arc::new(marathon_core::model::TestBatch {
            tests: tests(),
            id: format!("batch-{}", BATCH_ID.fetch_add(1, Ordering::SeqCst)).into(),
        });
        let outcome = d
            .execute(
                configuration,
                marathon_core::model::DevicePoolId { name: "pool".into() },
                batch,
                result.clone(),
                context,
            )
            .await;
        (outcome, result.completed().and_then(Result::ok))
    })
}
pub mod artifacts;
