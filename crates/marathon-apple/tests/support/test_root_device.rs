//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/StartupPhasesTest.kt (generated xctestrun rows).
//! Factory-only oracle witnesses complement the four reduced preparation harnesses approved in 4.5b.
#![allow(dead_code)]
use marathon_logging::{Component, LogSettings};

use crate::support;
use base64::{Engine, engine::general_purpose::STANDARD};
use jcompat::{
    jtime::Duration,
    
};
use marathon_apple::{
    apple_device::AppleDevice,
    bin::{AppleBinaryEnvironment, Tools},
    cmd::{
        CommandExecutor, CommandHost, CommandRequest, CommandResult, CommandSession, cancelled, failure,
        remote::ssh::sshj::config::SshLogging,
    },
    generated_file,
    model::{AppleTestBundle, Arch, Sdk},
    plist::{PlistDict, PropertyList, bundle::BundleInfo},
    remote_file_manager::RemoteFileManager,
    worker_commands::ExecutorWorkerCommands,
};
use marathon_config::{Configuration, devices::Transport, vendor::*};
use marathon_core::{
    error::{VendorError, VendorResult},
    listener::{LineListener, LogProducer},
    model::{BatchRef, DeviceInfo, DevicePoolId, NetworkState, OperatingSystem, TestBatchResults},
    runtime::{CancellationSignal, Deferred, VendorFuture},
    screenshot::{RgbaFrame, Rotation, Screenshottable},
    vendor::{Device, VendorContext},
};
use marathon_error::FailureKind;
use marathon_test_support::apple::fake_simulator_worker::FakeSimulatorWorker;
use serde_json::{Value, json};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

pub(crate) type WrittenFiles = Arc<Mutex<Vec<(String, Vec<u8>)>>>;
pub(crate) type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
pub(crate) fn v<T, E: std::fmt::Debug>(value: Result<T, E>) -> TestResult<T> {
    value.map_err(|e| io::Error::other(format!("{e:?}")).into())
}
pub(crate) fn s<'a>(c: &'a Value, key: &str, default: &'a str) -> &'a str {
    c[key].as_str().unwrap_or(default)
}
pub(crate) fn b(c: &Value, key: &str) -> bool {
    c[key].as_bool().unwrap_or(false)
}
pub(crate) fn sdk(c: &Value) -> Sdk {
    Sdk::ALL
        .into_iter()
        .find(|sdk| sdk.name() == s(c, "sdk", "IPHONESIMULATOR"))
        .unwrap()
}
pub(crate) fn arch(c: &Value) -> Arch {
    Arch::ALL
        .into_iter()
        .find(|arch| arch.value() == s(c, "arch", "arm64"))
        .unwrap()
}
pub(crate) fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}
pub(crate) fn rows() -> impl Iterator<Item = Value> {
    include_str!("../../../../fixtures/oracle/test-root/probes.jsonl")
        .lines()
        .chain(include_str!("../../../../fixtures/oracle/test-root/temp-prefixes.jsonl").lines())
        .map(|line| serde_json::from_str(line).unwrap())
}
pub(crate) fn input() -> Value {
    rows().next().unwrap()["input"].clone()
}
pub(crate) fn io_vendor(error: io::Error) -> VendorError {
    VendorError::Other(failure(FailureKind::Io, error.to_string()))
}

pub(crate) struct Recorder {
    pub(crate) worker: Arc<FakeSimulatorWorker>,
    pub(crate) requests: Mutex<Vec<Value>>,
    pub(crate) events: Arc<Mutex<Vec<Value>>>,
}
impl CommandExecutor for Recorder {
    fn host(&self) -> &CommandHost {
        self.worker.host()
    }
    fn component(&self) -> &Component {
        self.worker.component()
    }
    fn connected(&self) -> bool {
        self.worker.connected()
    }
    fn log_error(&self, _: String) {}
    fn execute(&self, request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandSession> {
        let millis = |d: Duration| d.seconds() * 1000 + (d.nanos() / 1_000_000);
        let event = json!({"argv":request.command,"timeoutMillis":millis(request.timeout),"idleMillis":millis(request.idle_timeout)});
        self.requests.lock().unwrap().push(event.clone());
        self.events.lock().unwrap().push(event);
        self.worker.execute(request, context)
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        self.worker.close()
    }
}
pub(crate) struct Fake {
    pub(crate) input: Value,
    pub(crate) root: PathBuf,
    pub(crate) manager: RemoteFileManager,
    pub(crate) binary: AppleBinaryEnvironment,
    pub(crate) recorder: Arc<Recorder>,
    pub(crate) logging: SshLogging,
    pub(crate) transport: Transport,
    pub(crate) policy: bool,
    pub(crate) debug: bool,
    pub(crate) pushes: Mutex<Vec<Value>>,
    pub(crate) xml: Mutex<Option<String>>,
    pub(crate) staged: Mutex<Vec<PathBuf>>,
    pub(crate) bridge: Mutex<Vec<(String, Vec<u8>)>>,
    pub(crate) written: WrittenFiles,
    pub(crate) logs: Arc<Mutex<Vec<Value>>>,
    shared: marathon_apple::cmd::shared_transfers::SharedTransfers,
    attempts: AtomicUsize,
}
impl Fake {
    pub(crate) fn new(input: Value, root: &Path) -> Self {
        let registry = Arc::new(Mutex::new(LogSettings::default()));
        let logs = Arc::new(Mutex::new(vec![]));
        let captured = logs.clone();
        registry.lock().unwrap().debug = true;
        let root_text = root.to_string_lossy().into_owned();
        let logging = SshLogging::new(
            registry.clone(),
            Arc::new(move |logger, level, text, _| {
                if logger.name() == "marathon_ios::apple_simulator_media_importer" {
                    captured.lock().unwrap().push(json!({"logger":logger.name(),"level":format!("{level:?}").to_uppercase(),"message":text.replace(&root_text,"<bundle>")}));
                }
            }),
        );
        let worker = Arc::new(FakeSimulatorWorker::new(
            Arc::new(support::Runtime),
            &mut registry.lock().unwrap(),
        ));
        worker.add(["a"]);
        let spec = input.clone();
        let lipo = Arc::new(AtomicUsize::new(0));
        let written: WrittenFiles = Arc::default();
        let writes = written.clone();
        *worker.before.lock().unwrap() = Arc::new(move |command| {
            let spec = spec.clone();
            let lipo = lipo.clone();
            let writes = writes.clone();
            Box::pin(async move {
                let mode = s(&spec, "mode", "");
                if command[0] == "test" && mode == "exists-throw" {
                    return Err(VendorError::Other(failure(FailureKind::Io, "exists io")));
                }
                if command[0] == "sh" && mode == "write-cancel" {
                    return Err(VendorError::Cancelled(cancelled("write cancelled")));
                }
                if command[0] == "mkdir" && mode == "mkdir-cancel" {
                    return Err(VendorError::Cancelled(cancelled("mkdir cancelled")));
                }
                let mut stdout = vec![];
                if command[0] == "xcrun" {
                    stdout.push(format!("/Xcode/Platforms/{}.platform", sdk(&spec).platform_name()));
                }
                if command[0] == "lipo" && command.last().is_some_and(|s| s == "-archs") {
                    stdout.push(
                        s(
                            &spec,
                            if lipo.fetch_add(1, Ordering::Relaxed).is_multiple_of(2) {
                                "testArch"
                            } else {
                                "runnerArch"
                            },
                            "arm64",
                        )
                        .into(),
                    );
                }
                if command[0] == "sh" && command[2].contains("nm ") {
                    stdout = match s(&spec, "type", "") {
                        "detect-ui" => vec!["XCUIApplication".into()],
                        "detect-kif" => vec!["KIFTestCase".into()],
                        "detect-both" => vec!["XCUIApplication".into(), "KIFTestCase".into()],
                        _ => vec![],
                    };
                }
                if command[0] == "sh" && command[2].starts_with("umask ") {
                    let script = &command[2];
                    let encoded = script
                        .strip_prefix("umask 077 && echo ")
                        .unwrap()
                        .split(' ')
                        .next()
                        .unwrap();
                    let dst = script.split("mv -f ").nth(1).unwrap().split(' ').nth(1).unwrap();
                    writes
                        .lock()
                        .unwrap()
                        .push((dst.into(), STANDARD.decode(encoded).unwrap()));
                }
                let missing = s(&spec, "missing", "");
                let exit = if command[0] == "mkdir" && mode == "mkdir-fail" {
                    1
                } else if command[0] == "test" && command[2] == missing {
                    if b(&spec, "unknown") { 2 } else { 1 }
                } else if (!missing.is_empty() && command.iter().any(|arg| arg.contains(missing)))
                    || (command[0] == "test"
                        && (mode == "exists-missing"
                            || mode == "exists-test-missing" && command.last().unwrap().contains(".xctest/")))
                    || (command[0].ends_with("PlistBuddy") && mode == "plist-false")
                    || (command[0] == "sh" && mode == "write-false")
                {
                    1
                } else if command[0] == "test"
                    && (mode == "exists-unknown"
                        || mode == "exists-test-unknown" && command.last().unwrap().contains(".xctest/"))
                {
                    2
                } else {
                    0
                };
                Ok(Some(CommandResult {
                    stdout,
                    stderr: vec![],
                    exit_code: Some(exit),
                }))
            })
        });
        let recorder = Arc::new(Recorder {
            worker,
            requests: Mutex::default(),
            events: Arc::default(),
        });
        let timeouts = Arc::new(AppleTimeoutConfiguration {
            shell: support::duration(31_000),
            shell_idle: support::duration(32_000),
            ..apple_timeout_configuration(AppleTimeoutParameters::empty())
        });
        let binary = AppleBinaryEnvironment::with_tools(
            Tools {
                executor: recorder.clone(),
                timeouts: timeouts.clone(),
                runtime: Arc::new(support::Runtime),
                logging: logging.clone(),
            },
            60_000,
            30_000,
        );
        let udid = s(&input, "udid", "a");
        let manager = RemoteFileManager::new(
            format!("/tmp/marathon/{udid}"),
            udid.into(),
            Arc::new(ExecutorWorkerCommands {
                executor: recorder.clone(),
                timeouts,
            }),
            logging.clone(),
        );
        Self {
            input,
            root: root.into(),
            manager,
            binary,
            recorder,
            shared: marathon_apple::cmd::shared_transfers::SharedTransfers::new(
                Arc::new(support::Runtime),
                logging.clone(),
            ),
            logs,
            attempts: AtomicUsize::new(0),
            logging,
            transport: Transport::Local,
            policy: false,
            debug: false,
            pushes: Mutex::default(),
            xml: Mutex::default(),
            staged: Mutex::default(),
            bridge: Mutex::default(),
            written,
        }
    }
}
impl Device for Fake {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            operating_system: OperatingSystem {
                version: s(&self.input, "os", "15.0").into(),
            },
            serial_number: if b(&self.input, "installer") {
                format!("{}@local", self.udid())
            } else {
                self.udid().into()
            },
            model: "model".into(),
            manufacturer: "Apple".into(),
            network_state: NetworkState::Connected,
            device_features: vec![],
            healthy: true,
        }
    }
    fn abi(&self) -> String {
        "arm64".into()
    }
    fn worker_id(&self) -> Option<String> {
        Some(self.transport.id())
    }
    fn component(&self) -> &Component {
        self.recorder.component()
    }
    fn prepare<'a>(&'a self, _: Arc<Configuration>, _: VendorContext) -> VendorFuture<'a, ()> {
        panic!("unused")
    }
    fn execute<'a>(
        &'a self,
        _: Arc<Configuration>,
        _: DevicePoolId,
        _: BatchRef,
        _: Arc<dyn Deferred<TestBatchResults>>,
        _: VendorContext,
    ) -> VendorFuture<'a, ()> {
        panic!("unused")
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        self.recorder.close()
    }
}
impl Screenshottable for Fake {
    fn orientation(&self) -> VendorResult<Rotation> {
        Ok(Rotation::Rotation0)
    }
    fn screenshot<'a>(&'a self, _: Duration, _: Arc<dyn CancellationSignal>) -> VendorFuture<'a, Option<RgbaFrame>> {
        panic!("unused")
    }
}
impl LogProducer for Fake {
    fn add_line_listener(&self, _: Arc<dyn LineListener>) -> VendorResult<()> {
        Ok(())
    }
    fn remove_line_listener(&self, _: &Arc<dyn LineListener>) -> VendorResult<()> {
        Ok(())
    }
}
macro_rules! unused {
    ($name:ident ($($arg:ident:$ty:ty),*) -> $result:ty) => {
        fn $name<'a>(&'a self,$($arg:$ty,)*_context:&'a VendorContext)->VendorFuture<'a,$result> { $(let _=$arg;)* panic!("unused") }
    }
}
impl AppleDevice for Fake {
    fn udid(&self) -> &str {
        s(&self.input, "udid", "a")
    }
    fn remote_file_manager(&self) -> &RemoteFileManager {
        &self.manager
    }
    fn storage_path(&self) -> &str {
        self.manager.remote_directory()
    }
    fn transport(&self) -> &Transport {
        &self.transport
    }
    fn sdk(&self) -> Sdk {
        sdk(&self.input)
    }
    fn arch(&self) -> Arch {
        arch(&self.input)
    }
    fn binary_environment(&self) -> &AppleBinaryEnvironment {
        &self.binary
    }
    fn push_file<'a>(&'a self, src: &'a Path, dst: &'a str, _: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            let bytes = tokio::fs::read(src).await.map_err(io_vendor)?;
            self.bridge.lock().unwrap().push((dst.into(), bytes));
            Ok(true)
        })
    }
    fn push_generated_file<'a>(
        &'a self,
        src: &'a Path,
        dst: &'a str,
        context: &'a VendorContext,
    ) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.staged.lock().unwrap().push(src.into());
            let bytes = tokio::fs::read(src).await.map_err(io_vendor)?;
            *self.xml.lock().unwrap() = Some(STANDARD.encode(&bytes));
            let exists = tokio::fs::metadata(src).await.is_ok();
            self.pushes.lock().unwrap().push(json!({"kind":"generated","dst":dst,"existsDuringSend":exists,"suffix":src.file_name().unwrap().to_str().unwrap().ends_with("xctestrun")}));
            self.recorder
                .events
                .lock()
                .unwrap()
                .push(self.pushes.lock().unwrap().last().unwrap().clone());
            if self.policy {
                return generated_file::push_generated_file(
                    self,
                    src,
                    dst,
                    self.debug,
                    support::duration(31_000),
                    &self.logging,
                    context,
                )
                .await;
            }
            match s(&self.input, "mode", "") {
                "push-false" => Ok(false),
                "push-throw" => Err(VendorError::Other(failure(FailureKind::Io, "push io"))),
                "push-cancel" => Err(VendorError::Cancelled(cancelled("push cancelled"))),
                "delete-fail" => {
                    tokio::fs::remove_file(src).await.map_err(io_vendor)?;
                    tokio::fs::create_dir(src).await.map_err(io_vendor)?;
                    tokio::fs::write(src.join("child"), "keep").await.map_err(io_vendor)?;
                    Ok(false)
                }
                "await-cancel" => {
                    context.cancellation.cancelled().await;
                    assert!(tokio::fs::metadata(src).await.is_ok());
                    Err(VendorError::Cancelled(cancelled("push cancelled")))
                }
                _ => Ok(true),
            }
        })
    }
    fn push_shared<'a>(&'a self, src: &'a Path, dst: &'a str, context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            let transfer = || async {
                let push =
                    json!({"kind":"shared","src":src.strip_prefix(&self.root).unwrap().to_str().unwrap(),"dst":dst});
                self.pushes.lock().unwrap().push(push.clone());
                self.recorder.events.lock().unwrap().push(push);
                let count = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let mode = s(&self.input, "mode", "");
                match mode {
                    "retry-success" => Ok(count > 2),
                    "retry-fail" | "delay-cancel" => Ok(false),
                    "retry-throw" => Err(VendorError::Other(failure(FailureKind::Io, "transfer io"))),
                    "retry-cancel" => Err(VendorError::Cancelled(cancelled("transfer cancelled"))),
                    "extras-false" => Ok(!src.file_name().unwrap().to_str().unwrap().starts_with("first")
                        && !src.file_name().unwrap().to_str().unwrap().starts_with("second")),
                    _ => Ok(!((mode == "push-app-false" && count == 1) || (mode == "push-test-false" && count == 2))),
                }
            };
            if b(&self.input, "installer") {
                self.shared
                    .send(
                        "local",
                        src,
                        dst,
                        marathon_apple::cmd::TransferStop::new(context.cancellation.clone()),
                        || self.manager.exists(dst, context),
                        transfer,
                    )
                    .await
            } else {
                transfer().await
            }
        })
    }
    fn install<'a>(&'a self, path: &'a str, _: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.recorder.events.lock().unwrap().push(json!({"install":path}));
            Ok(s(&self.input, "mode", "") != "extras-false")
        })
    }
    unused!(setup()->());
    unused!(execute_worker_command(command: Vec<String>) -> Option<CommandResult>);
    unused!(pull_file(src: &'a str, dst: &'a Path) -> bool);
    unused!(push_folder(src: &'a Path, dst: &'a str) -> bool);
    unused!(pull_folder(src: &'a str, dst: &'a Path) -> bool);
    unused!(get_screenshot(timeout: Duration, dst: &'a Path) -> bool);
    unused!(start_video_recording(path: &'a str) -> Option<CommandResult>);
    unused!(stop_video_recording()->bool);
    unused!(shutdown()->bool);
    unused!(erase()->bool);
}
pub(crate) fn extract(zip_name: &str, dir: &Path) -> io::Result<()> {
    let zip = std::fs::read(fixtures().join("bundles").join(zip_name))?;
    let u16_at = |off: usize| usize::from(u16::from_le_bytes([zip[off], zip[off + 1]]));
    let u32_at = |off: usize| u32::from_le_bytes(zip[off..off + 4].try_into().unwrap()) as usize;
    let eocd = zip.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
    let mut pos = u32_at(eocd + 16);
    for _ in 0..u16_at(eocd + 10) {
        let (method, size) = (u16_at(pos + 10), u32_at(pos + 20));
        let (nl, el, cl) = (u16_at(pos + 28), u16_at(pos + 30), u16_at(pos + 32));
        let local = u32_at(pos + 42);
        let name = std::str::from_utf8(&zip[pos + 46..pos + 46 + nl]).unwrap();
        let start = local + 30 + u16_at(local + 26) + u16_at(local + 28);
        let mut bytes = vec![];
        if method == 0 {
            bytes.extend_from_slice(&zip[start..start + size]);
        } else {
            flate2::read::DeflateDecoder::new(&zip[start..start + size]).read_to_end(&mut bytes)?;
        }
        if !name.ends_with('/') {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(path, bytes)?;
        }
        pos += 46 + nl + el + cl;
    }
    Ok(())
}
pub(crate) fn prepare(root: &Path, c: &Value) -> TestResult {
    if b(c, "real") {
        extract(if sdk(c) == Sdk::Macos { "macos.zip" } else { "ios.zip" }, root)?;
        return Ok(());
    }
    let mac = sdk(c) == Sdk::Macos;
    for (key, name) in [("app", "CloudApp"), ("runner", "UITests-Runner"), ("test", "UITests")] {
        if b(c, "omitRunner") && key == "runner" {
            continue;
        }
        let name = if key == "test" && s(c, "test", "") == "UnitTests.xctest" {
            "UnitTests"
        } else {
            name
        };
        let folder = root.join(s(c, key, "")).join(if mac { "Contents" } else { "" });
        let binary = folder.join(if mac { "MacOS" } else { "" });
        std::fs::create_dir_all(&binary)?;
        let dict = PlistDict::new();
        dict.set_value("CFBundleIdentifier", format!("com.example.{name}"));
        dict.set_value("CFBundleName", name.to_owned());
        let edit = s(c, "edit", "");
        if edit != "absent-platforms" || key != "app" {
            dict.set_array(
                "CFBundleSupportedPlatforms",
                if edit == format!("{key}-platforms") || edit == "both-invalid" {
                    vec!["Other".to_owned(), "Second".into()]
                } else {
                    vec![sdk(c).platform_name().into()]
                },
            );
        }
        if edit != "minimum-absent" || key != "app" {
            dict.set_value(
                "LSMinimumSystemVersion",
                if edit == "minimum-old" && key == "app" {
                    "99.0"
                } else if edit == "minimum-invalid" && key == "app" {
                    "bad"
                } else {
                    "10.0"
                }
                .to_owned(),
            );
        }
        if b(c, "installer") {
            let mut text = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n    <key>CFBundleIdentifier</key><string>com.example.{name}</string>\n    <key>CFBundleName</key><string>{name}</string>\n    <key>CFBundleSupportedPlatforms</key><array><string>{}</string></array>\n</dict>\n</plist>",
                sdk(c).platform_name()
            );
            if mac {
                text = text.replace(
                    "</dict>",
                    "    <key>LSMinimumSystemVersion</key><string>10.0</string>\n</dict>",
                );
            }
            std::fs::write(folder.join("Info.plist"), text)?;
        } else {
            BundleInfo::wrap(dict).save_as_xml_to_file(&folder.join("Info.plist"))?;
        }
        if !b(c, "noBinary") || key != "app" {
            std::fs::write(binary.join(name), [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0])?;
        }
    }
    if s(c, "mode", "").starts_with("extras") {
        for name in ["first.app", "second.app", "wrong.APP"] {
            std::fs::create_dir(root.join(name))?;
        }
        std::fs::write(root.join(".app"), "file")?;
    }
    if b(c, "frameworks") {
        std::fs::create_dir(root.join(s(c, "app", "")).join("Frameworks"))?;
    }
    if b(c, "frameworkFile") {
        std::fs::write(root.join(s(c, "app", "")).join("Frameworks"), "file")?;
    }
    if b(c, "contentsFrameworks") {
        std::fs::create_dir_all(root.join(s(c, "app", "")).join("Contents/Frameworks"))?;
    }
    if b(c, "dylibs") || b(c, "uppercaseDylib") || b(c, "dylibDirectory") {
        let dir = root
            .join(s(c, "runner", ""))
            .join(if mac { "Contents/Frameworks" } else { "Frameworks" });
        std::fs::create_dir_all(&dir)?;
        if b(c, "dylibDirectory") {
            std::fs::create_dir(dir.join("directory.dylib"))?;
        } else {
            std::fs::write(
                dir.join(if b(c, "uppercaseDylib") {
                    "lib.DYLIB"
                } else {
                    "lib.dylib"
                }),
                "file",
            )?;
        }
    }
    Ok(())
}
pub(crate) async fn bundle(c: &Value, root: &Path, logging: SshLogging) -> TestResult<AppleTestBundle> {
    let mut bundle = v(AppleTestBundle::new(
        Some(root.join(s(c, "app", ""))),
        b(c, "reuse").then(|| root.join(s(c, "runner", ""))),
        root.join(s(c, "test", "")),
        sdk(c),
        Arc::new(support::Runtime),
        logging,
    )
    .await)?;
    if b(c, "noApp") {
        bundle.application = None;
    }
    Ok(bundle)
}
pub(crate) fn env(c: &Value) -> XctestrunEnvConfiguration {
    let entries = |key: &str| {
        c[key].as_array().map_or_else(Vec::new, |a| {
            a.iter()
                .map(|a| (a[0].as_str().unwrap().into(), String::from(a[1].as_str().unwrap())))
                .collect()
        })
    };
    XctestrunEnvConfiguration {
        app_envs: entries("appEnv"),
        test_envs: entries("testEnv"),
    }
}
pub(crate) fn xcresult(c: &Value) -> XcresultConfiguration {
    let lifetime = |key, default| match s(c, key, default) {
        "KEEP_NEVER" => Lifetime::KeepNever,
        "KEEP_ALWAYS" => Lifetime::KeepAlways,
        _ => Lifetime::DeleteOnSuccess,
    };
    XcresultConfiguration {
        pull: None,
        pulling_policy: PullingPolicy::Always,
        remote_clean: true,
        attachments: AttachmentsConfiguration {
            system_attachment_lifetime: lifetime("systemLifetime", "DELETE_ON_SUCCESS"),
            user_attachment_lifetime: lifetime("userLifetime", "KEEP_ALWAYS"),
        },
        preferred_screen_capture_format: c["capture"].as_str().map(|s| {
            if s == "SCREEN_RECORDING" {
                ScreenCaptureFormat::ScreenRecording
            } else {
                ScreenCaptureFormat::Screenshots
            }
        }),
        queue: XcresultQueueConfiguration {
            capacity: 8,
            concurrency: 4,
            worker_capacity: None,
        },
    }
}
