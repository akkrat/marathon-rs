//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/FakeSimulatorWorker.kt
use jcompat::logging::{Logger, MarathonLogging};
use marathon_apple::cmd::{CommandExecutor, CommandHost, CommandRequest, CommandResult, CommandSession, failure};
use marathon_core::{
    error::{Failure, VendorError, VendorResult},
    runtime::{VendorFuture, VendorRuntime},
    vendor::VendorContext,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub const IPHONE: &str = "com.apple.CoreSimulator.SimDeviceType.iPhone-15-Pro";
pub const IOS: &str = "com.apple.CoreSimulator.SimRuntime.iOS-18-6";
pub const SYSTEM_PROFILER: &str = "/usr/sbin/system_profiler";
pub const LIST: &[&str] = &["xcrun", "simctl", "list", "--json"];
pub const ENV: &[&str] = &["env"];
pub const XCODEBUILD_VERSION: &[&str] = &["xcrun", "xcodebuild", "-version"];
pub const UNAME: &[&str] = &["uname", "-m"];
pub const WORKER_FACTS: &[&[&str]] = &[ENV, XCODEBUILD_VERSION, UNAME];
pub fn ok(stdout: impl IntoIterator<Item = impl Into<String>>) -> CommandResult {
    CommandResult {
        stdout: stdout.into_iter().map(Into::into).collect(),
        stderr: vec![],
        exit_code: Some(0),
    }
}
pub fn invalid(udid: &str) -> CommandResult {
    CommandResult {
        stdout: vec![],
        stderr: vec![format!("Invalid device: {udid}")],
        exit_code: Some(148),
    }
}
pub fn connection_exception(message: &str) -> Failure {
    failure("net.schmizz.sshj.connection.ConnectionException", message)
}
pub fn transport_exception(message: &str) -> Failure {
    failure("net.schmizz.sshj.transport.TransportException", message)
}
pub fn io_exception(message: &str) -> Failure {
    failure("java.io.IOException", message)
}
pub fn cancellation_exception(message: &str) -> Failure {
    failure("java.util.concurrent.CancellationException", message)
}

pub struct Simulator {
    pub udid: String,
    pub device_type: String,
    pub runtime: String,
    pub available: AtomicBool,
    pub model: String,
}
impl Simulator {
    pub fn new(udid: &str, device_type: &str, runtime: &str) -> Self {
        Self {
            udid: udid.into(),
            device_type: device_type.into(),
            runtime: runtime.into(),
            available: AtomicBool::new(true),
            model: format!("model-{udid}"),
        }
    }
}
pub type Simulators = Arc<Mutex<BTreeMap<String, Arc<Simulator>>>>;
pub type BeforeHook = Arc<dyn Fn(Vec<String>) -> VendorFuture<'static, Option<CommandResult>> + Send + Sync>;
pub type SessionHook = Arc<dyn Fn(&[String]) -> VendorResult<Option<CommandSession>> + Send + Sync>;
pub type ConnectedHook = Arc<dyn Fn() + Send + Sync>;

pub struct FakeSimulatorWorker {
    runtime: Arc<dyn VendorRuntime>,
    host: CommandHost,
    logger: Logger,
    pub arch: Mutex<String>,
    pub home: String,
    pub simulators: Simulators,
    pub connected: AtomicBool,
    /// Test-only: a blocking hook must be called on a vendor worker thread.
    pub on_connected_check: Mutex<ConnectedHook>,
    pub commands: Mutex<Vec<Vec<String>>>,
    pub xcodebuild_version: Mutex<Vec<String>>,
    pub before: Mutex<BeforeHook>,
    pub session: Mutex<SessionHook>,
    /// D54: why the connection was lost, once `lose` has been called.
    lost: tokio::sync::watch::Sender<Option<Failure>>,
}
impl FakeSimulatorWorker {
    /// D54: the connection is lost, as keep-alive loses it: not connected, and `connection_lost` is ready with `cause`.
    pub fn lose(&self, cause: Failure) {
        self.connected.store(false, Ordering::SeqCst);
        self.lost.send_replace(Some(cause));
    }
    pub fn new(runtime: Arc<dyn VendorRuntime>, logging: &mut MarathonLogging) -> Self {
        Self {
            runtime,
            host: CommandHost {
                id: "local".into(),
                charset: "UTF-8".into(),
            },
            logger: logging.logger("com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker", None),
            arch: Mutex::new("arm64".into()),
            home: "/Users/worker".into(),
            simulators: Arc::default(),
            connected: AtomicBool::new(true),
            on_connected_check: Mutex::new(Arc::new(|| {})),
            commands: Mutex::default(),
            xcodebuild_version: Mutex::new(vec!["Xcode 16.4".into(), "Build version 16F6".into()]),
            before: Mutex::new(Arc::new(|_| Box::pin(async { Ok(None) }))),
            session: Mutex::new(Arc::new(|_| Ok(None))),
            lost: tokio::sync::watch::channel(None).0,
        }
    }
    pub fn with_host_id(mut self, host_id: &str) -> Self {
        self.host.id = host_id.into();
        self
    }
    pub fn with_simulators(mut self, simulators: Simulators) -> Self {
        self.simulators = simulators;
        self
    }
    pub fn add(&self, udids: impl IntoIterator<Item = impl AsRef<str>>) {
        self.add_with(udids, IPHONE, IOS);
    }
    pub fn add_with(&self, udids: impl IntoIterator<Item = impl AsRef<str>>, device_type: &str, runtime: &str) {
        let mut simulators = self.simulators.lock().unwrap_or_else(|e| e.into_inner());
        for id in udids {
            let id = id.as_ref();
            simulators.insert(id.into(), Arc::new(Simulator::new(id, device_type, runtime)));
        }
    }
    pub fn count(&self, argv: &[impl AsRef<str>]) -> usize {
        self.commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|a| a.iter().map(String::as_str).eq(argv.iter().map(AsRef::as_ref)))
            .count()
    }
    pub fn lists(&self) -> usize {
        self.count(LIST)
    }
    pub fn worker_fact_commands(&self) -> usize {
        self.commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|a| WORKER_FACTS.iter().any(|f| a == f) || a.first().is_some_and(|s| s == SYSTEM_PROFILER))
            .count()
    }
    pub fn getenv(&self, udid: &str, key: &str) -> usize {
        self.count(&["xcrun", "simctl", "getenv", udid, key])
    }
    fn inventory(&self) -> String {
        let mut devices: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
        for d in self.simulators.lock().unwrap_or_else(|e| e.into_inner()).values() {
            devices.entry(d.runtime.clone()).or_default().push(json!({"udid":d.udid,"name":d.udid,"state":"Shutdown","isAvailable":d.available.load(Ordering::SeqCst),"logPath":format!("{}/Library/Logs/CoreSimulator/{}",self.home,d.udid),"deviceTypeIdentifier":d.device_type}));
        }
        json!({"devicetypes":[{"name":"iPhone 15 Pro","identifier":IPHONE}],"runtimes":[{"identifier":IOS,"name":"iOS 18.6","version":"18.6"}],"devices":devices}).to_string()
    }
    fn answer(&self, command: &[String]) -> VendorResult<CommandResult> {
        let a = command.iter().map(String::as_str).collect::<Vec<_>>();
        Ok(match a.as_slice() {
            a if a == LIST => ok([self.inventory()]),
            a if a == ENV => ok([format!("HOME={}", self.home), "USER=worker".into()]),
            a if a == XCODEBUILD_VERSION => ok(self
                .xcodebuild_version
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()),
            a if a == UNAME => ok([self.arch.lock().unwrap_or_else(|e| e.into_inner()).clone()]),
            [SYSTEM_PROFILER, ..] => ok(["<string>spdisplays_metalfamily</string>"]),
            ["xcrun", "simctl", "getenv", id, key, ..] => {
                let simulators = self.simulators.lock().unwrap_or_else(|e| e.into_inner());
                let Some(simulator) = simulators.get(*id) else {
                    return Ok(invalid("device"));
                };
                match *key {
                    "HOME" => ok([format!(
                        "{}/Library/Developer/CoreSimulator/Devices/{}/data",
                        self.home, simulator.udid
                    )]),
                    "SIMULATOR_MODEL_IDENTIFIER" => ok([simulator.model.clone()]),
                    "SIMULATOR_RUNTIME_VERSION" => ok(["18.6"]),
                    _ => CommandResult {
                        stdout: vec![],
                        stderr: vec![],
                        exit_code: Some(1),
                    },
                }
            }
            ["xcrun", "simctl", "create", _, device_type, rest @ ..] => {
                let id = uuid::Uuid::new_v4().to_string().to_uppercase();
                self.add_with([&id], device_type, rest.first().copied().unwrap_or(IOS));
                ok([id])
            }
            ["xcrun", "simctl", "delete", id, ..] => {
                if self
                    .simulators
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(*id)
                    .is_some()
                {
                    ok(Vec::<String>::new())
                } else {
                    invalid(id)
                }
            }
            ["xcrun", "simctl", "getenv" | "create" | "delete", ..] => {
                return Err(VendorError::Other(failure(
                    "java.lang.IndexOutOfBoundsException",
                    "malformed fake command",
                )));
            }
            _ => CommandResult {
                stdout: vec![],
                stderr: vec!["unsupported command".into()],
                exit_code: Some(1),
            },
        })
    }
}
impl CommandExecutor for FakeSimulatorWorker {
    fn host(&self) -> &CommandHost {
        &self.host
    }
    fn logger(&self) -> &Logger {
        &self.logger
    }
    fn connected(&self) -> bool {
        let hook = self
            .on_connected_check
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        hook();
        self.connected.load(Ordering::SeqCst)
    }
    fn log_error(&self, _: String) {}
    fn execute(&self, request: CommandRequest, _: VendorContext) -> VendorFuture<'_, CommandSession> {
        Box::pin(async move {
            self.commands
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(request.command.clone());
            let session = self.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(session) = session(&request.command)? {
                return Ok(session);
            }
            let before = self.before.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let result = match before(request.command.clone()).await? {
                Some(result) => result,
                None => self.answer(&request.command)?,
            };
            Ok(CommandSession::finished(self.runtime.clone(), result))
        })
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn connection_lost(&self) -> std::pin::Pin<Box<dyn Future<Output = Failure> + Send>> {
        let mut lost = self.lost.subscribe();
        Box::pin(async move {
            let cause = match lost.wait_for(Option::is_some).await {
                Ok(cause) => cause.clone(),
                Err(_) => None,
            };
            match cause {
                Some(cause) => cause,
                None => std::future::pending().await,
            }
        })
    }
}
