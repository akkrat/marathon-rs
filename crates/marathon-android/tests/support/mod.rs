//! Port of vendor/vendor-android/src/test/kotlin/com/malinskiy/marathon/android/adam/TestDeviceFactory.kt
//! and TestConfigurationFactory.kt, DeviceExpectationExtensions.kt.
#![allow(dead_code)]

use jcompat::file_manager::FileManager;
use marathon_adb::{
    client::{
        AndroidDebugBridgeClient, SocketFactory,
        device::{Device as AdbDevice, DeviceState},
    },
    server::{FakeAdbServer, Script, ServerOptions},
};
use marathon_android::{
    device::{AndroidDevice, PendingBatchExecutor},
    services::AndroidRuntime,
    tracker::DeviceStateTracker,
};
use marathon_config::{
    Configuration, VendorConfiguration,
    binding::{BindingContext, Profile},
    loader::MarathonConfigurationLoader,
    vendor::AndroidConfiguration,
};
use marathon_core::{
    analytics::Track,
    artifacts::FileManagerOutput,
    error::*,
    executor::{CoreExecutor, Signal, bridge::call_vendor, hooks::RuntimeHooks},
    runtime::*,
    time::SystemTimer,
    vendor::VendorContext,
    wiring::VendorDependencies,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
pub fn checked<T, E: std::fmt::Debug>(r: Result<T, E>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("fixture: {e:?}"),
    }
}
pub fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(path)
}
pub fn configuration() -> Configuration {
    let base = PathBuf::from("/");
    let context = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: PathBuf::from("/Marathonfile"),
        marathonfile_dir: base.clone(),
        process_dir: base,
        environment: vec![],
        now: jcompat::jtime::instant_of(0, 0).unwrap(),
        analytics_tracking: false,
        bugsnag_reporting: false,
    };
    checked(MarathonConfigurationLoader.parse_sync(b"name: android-test\noutputDir: /tmp/android-test-output\nvendorConfiguration:\n  type: Android\n  androidSdk: /nonexistent\n  autoGrantPermission: false\n  mockLocation: false\n  allureConfiguration:\n    enabled: false\n",&context))
}
pub fn android(c: &mut Configuration) -> &mut AndroidConfiguration {
    let VendorConfiguration::Android(a) = &mut c.vendor_configuration else {
        panic!("android config")
    };
    a
}
pub fn run(
    c: Configuration,
    body: impl FnOnce(Arc<AndroidRuntime>, VendorContext) -> VendorFuture<'static, ()> + Send + 'static,
) {
    checked(CoreExecutor::run(move |core| on_core(core, c, body)));
}
/// As [`run`], with a vendor clock that nothing advances: a test can hold a timer open as long as it needs, and a
/// timer that something wrongly waits for hangs the test.
pub fn run_with_stopped_clock(
    c: Configuration,
    body: impl FnOnce(Arc<AndroidRuntime>, VendorContext) -> VendorFuture<'static, ()> + Send + 'static,
) {
    checked(CoreExecutor::run_virtual(move |core, _replay| on_core(core, c, body)));
}
/// Moves the vendor clock of [`run_with_driven_clock`] from the test body, which runs on a vendor thread.
#[derive(Clone)]
pub struct DrivenClock(tokio::sync::mpsc::UnboundedSender<(i64, tokio::sync::oneshot::Sender<Result<(), String>>)>);
impl DrivenClock {
    /// Runs every timer due by `millis`, in order, and what they wake, as the replay driver does.
    pub async fn advance_to(&self, millis: i64) {
        let (done, wait) = tokio::sync::oneshot::channel();
        checked(self.0.send((millis, done)).map_err(|_| "clock driver ended"));
        checked(checked(wait.await));
    }
}
/// As [`run_with_stopped_clock`], with a clock that the test moves on itself.
pub fn run_with_driven_clock(
    c: Configuration,
    body: impl FnOnce(Arc<AndroidRuntime>, VendorContext, DrivenClock) -> VendorFuture<'static, ()> + Send + 'static,
) {
    use marathon_core::time::ReplayDriver;
    checked(CoreExecutor::run_virtual(move |core, replay| {
        let (requests, mut received) = tokio::sync::mpsc::unbounded_channel();
        let clock = DrivenClock(requests);
        let run = on_core(core, c, move |runtime, context| body(runtime, context, clock));
        Box::pin(async move {
            let drive = async {
                while let Some((millis, done)) = received.recv().await {
                    let advanced: Result<(), String> = replay.advance_to(millis).await.map_err(|e| format!("{e:?}"));
                    let _ = done.send(advanced);
                }
                std::future::pending::<()>().await
            };
            tokio::select! {
                result = run => result,
                () = drive => unreachable!(),
            }
        })
    }));
}
fn on_core(
    core: std::rc::Rc<CoreExecutor>,
    mut c: Configuration,
    body: impl FnOnce(Arc<AndroidRuntime>, VendorContext) -> VendorFuture<'static, ()> + Send + 'static,
) -> marathon_core::runtime::LocalFuture<'static, marathon_core::error::OperationResult<()>> {
    {
        Box::pin(async move {
            let _default_output = (c.output_dir == Path::new("/tmp/android-test-output")).then(|| {
                let output = checked(tempfile::tempdir());
                c.output_dir = output.path().to_owned();
                output
            });
            let hooks = RuntimeHooks::live();
            let logging = core.logging();
            let io = core.vendor();
            let clock = core.vendor_clock();
            let output = c.output_dir.clone();
            let log: marathon_core::ServiceLog =
                Arc::new(|_, _, _, _| assert_ne!(std::thread::current().name(), Some("marathon-core")));
            let files = Arc::new(FileManagerOutput::new(
                io.clone(),
                FileManager::new(4096, 255, &output.to_string_lossy(), ""),
                output,
                logging.lock().unwrap().component("FileManager"),
                log.clone(),
            ));
            let dependencies = VendorDependencies {
                logs: core.log_queue().sender(),
                wall: Arc::new(marathon_core::logging::host_millis),
                configuration: Arc::new(c),
                io: io.clone(),
                clock: clock.clone(),
                logging,
                log: log.clone(),
                hooks: hooks.clone(),
                timer: Arc::new(checked(SystemTimer::new(hooks.clone()))),
                tracker: Arc::new(Track::new(io.clone(), hooks.clone())),
                files,
                barriers: hooks.barriers(),
            };
            let context = VendorContext {
                cancellation: Signal::new(),
                clock,
                hooks,
            };
            let result = call_vendor(io.as_ref(), context.cancellation.clone(), move |_| {
                Box::pin(async move {
                    let runtime = AndroidRuntime::new(dependencies, Arc::new(PendingBatchExecutor)).await?;
                    body(runtime, context).await
                })
            })
            .await;
            result.map_err(|e| match e {
                VendorError::Cancelled(c) => OperationError::Cancelled(c),
                e => OperationError::Failed(e.failure().clone()),
            })
        })
    }
}
pub fn base_script(sdk: i32, features: Vec<&str>) -> Value {
    json!({"server":{"strict":true},"devices":[{"serial":"emulator-5554","features":features,"getprop":{"sys.boot_completed":"0","ro.product.cpu.abi":"x86","ro.build.version.sdk":sdk,"ro.build.version.codename":"REL","ro.product.model":"Android SDK built for x86","ro.product.manufacturer":"Google","ro.boot.serialno":"EMULATOR30X6X5X0"}}],"shell":[
    {"exact":"dumpsys input","stdout":"SurfaceOrientation: 0\n"},
    {"exact":"echo $EXTERNAL_STORAGE","stdout":"/sdcard\r\n"},
    {"exact":"ls /system/bin/screenrecord","stdout":"/system/bin/screenrecord"},
    {"exact":"ls /system/bin/md5","stdout":"/system/bin/md5"},
    {"exact":"logcat -v long","hang":true}
    ]})
}
/// A device that went dead while the server still lists it: setup's two `getprop`s answer, and the next one, D49's question,
/// shows it no longer booted. Later ones answer again, as after a restart.
pub fn stops_answering_after_setup(mut script: Value) -> Value {
    let props = script["devices"][0]["getprop"].as_object().unwrap().clone();
    let all: String = props
        .iter()
        .map(|(k, v)| format!("[{k}]: [{}]\n", v.as_str().map_or_else(|| v.to_string(), str::to_owned)))
        .collect();
    let rules = script["shell"].as_array_mut().unwrap();
    rules.insert(0, json!({"exact":"getprop","times":2,"stdout":all}));
    rules.insert(
        1,
        json!({"exact":"getprop","times":1,"stdout":"[ro.build.version.sdk]: [27]\n"}),
    );
    script
}
pub async fn server(script: Value) -> FakeAdbServer {
    let script = checked(Script::from_yaml(&script.to_string()));
    checked(FakeAdbServer::start(checked(script.compile(Path::new("."))), ServerOptions::default()).await)
}
pub fn device(server: &FakeAdbServer, runtime: Arc<AndroidRuntime>) -> AndroidDevice {
    let client = AndroidDebugBridgeClient::new(
        server.addr().ip(),
        server.port(),
        Arc::new(SocketFactory::new(1000, 30000)),
        None,
    );
    let tracker = Arc::new(DeviceStateTracker::default());
    tracker.update(&[AdbDevice {
        serial: "emulator-5554".into(),
        state: DeviceState::Device,
    }]);
    AndroidDevice::new(client, "emulator-5554".into(), tracker, runtime)
}
pub fn commands(server: &FakeAdbServer) -> Vec<String> {
    server
        .transcript()
        .iter()
        .filter(|r| r["type"] == "shell" && r["command"] != "logcat -v long")
        .filter_map(|r| r["command"].as_str().map(str::to_owned))
        .collect()
}
pub fn request_services(server: &FakeAdbServer) -> Vec<String> {
    server
        .transcript()
        .iter()
        .filter(|r| r["type"] == "request")
        .filter_map(|r| r["service"].as_str().map(str::to_owned))
        .collect()
}
pub const D85_BOOT_ID_SERVICE: &str = "shell:cat /proc/sys/kernel/random/boot_id;echo x$?";

/// The Kotlin recordings predate the boot ID and marker commands. Keep the return probe and any
/// duplicate, misplaced or unsuccessful command visible.
pub fn d85_omitted_connections(rows: &[Value]) -> std::collections::BTreeSet<u64> {
    use marathon_adb::client::shell::boot_marker::MarkerCommand;
    use std::collections::{BTreeMap, BTreeSet};

    let mut lanes = BTreeMap::<&str, Vec<(u64, &str)>>::new();
    for row in rows.iter().filter(|row| row["type"] == "request") {
        let (Some(conn), Some(service)) = (row["conn"].as_u64(), row["service"].as_str()) else {
            continue;
        };
        if service.starts_with("host:transport:") || service == "shell:logcat -v long" {
            continue;
        }
        let features = service
            .strip_prefix("host-serial:")
            .and_then(|rest| rest.strip_suffix(":features"));
        if let Some(serial) = row["serial"].as_str().or(features) {
            lanes.entry(serial).or_default().push((conn, service));
        }
    }
    let reply_matches = |conn, command, expected: &[u8]| {
        let mut stdout = Vec::new();
        let mut shell = false;
        for row in rows.iter().filter(|row| row["conn"].as_u64() == Some(conn)) {
            match row["type"].as_str() {
                Some("shell") => {
                    if shell
                        || row["command"] != command
                        || row["handler"] != "builtin"
                        || row["service"] != "shell"
                        || row["protocol"] != "raw"
                        || row["exit_code_trick"] != true
                        || row["exit_code"] != 0
                        || row["hang"] != false
                        || !row["stdin_bytes"].is_null()
                    {
                        return false;
                    }
                    shell = true;
                }
                Some("output") => {
                    if row["stream"] != "stdout" {
                        return false;
                    }
                    let Some(bytes) = row["bytes"].as_array() else {
                        return false;
                    };
                    for byte in bytes {
                        let Some(byte) = byte.as_u64().and_then(|byte| u8::try_from(byte).ok()) else {
                            return false;
                        };
                        stdout.push(byte);
                    }
                }
                _ => {}
            }
        }
        shell && stdout == expected
    };
    let mut omitted = BTreeSet::new();
    for (serial, requests) in lanes {
        let features = format!("host-serial:{serial}:features");
        let mut marked = BTreeSet::new();
        for (index, &(conn, service)) in requests.iter().enumerate() {
            let before = index.checked_sub(1).map(|before| requests[before].1);
            let Some(command) = service.strip_prefix("shell:").and_then(|s| s.strip_suffix(";echo x$?")) else {
                continue;
            };
            let skip = if service == D85_BOOT_ID_SERVICE {
                before == Some("shell:getprop;echo x$?")
                    && requests.get(index + 1).is_some_and(|(_, service)| *service == features)
                    && reply_matches(conn, command, b"00000000-0000-4000-8000-000000000001\n")
            } else {
                match MarkerCommand::parse(command) {
                    Some((MarkerCommand::Write, token)) => {
                        before == Some("shell:logcat -c;echo x$?")
                            && reply_matches(conn, command, format!("{token}\n").as_bytes())
                            && marked.insert(token)
                    }
                    Some((MarkerCommand::Remove, token)) => reply_matches(conn, command, b"") && marked.remove(&token),
                    _ => false,
                }
            };
            if skip {
                omitted.insert(conn);
            }
        }
    }
    omitted
}

pub const SETUP_COMMANDS: [&str; 7] = [
    "getprop",
    "dumpsys input",
    "echo $EXTERNAL_STORAGE",
    "ls /system/bin/screenrecord",
    "ls /system/bin/md5",
    "getprop",
    "cat /proc/sys/kernel/random/boot_id",
];
pub fn expect_setup(commands: &[String]) {
    assert_eq!(&commands[..SETUP_COMMANDS.len()], SETUP_COMMANDS);
}
pub async fn finish(mut server: FakeAdbServer, device: AndroidDevice) {
    use marathon_core::vendor::Device;
    // A completed setup ends by starting logcat in the background, right after the features request; a
    // dispose that lands in logcat's transport handshake resets it and warns.
    let set_up = server.transcript().iter().any(|r| {
        r["type"] == "request"
            && r["service"]
                .as_str()
                .is_some_and(|s| s.starts_with("host-serial:") && s.ends_with(":features"))
    });
    if set_up {
        logcat_streams(&[(&server, 1)]).await;
    }
    checked(device.dispose().await);
    assert_eq!(server.warnings(), 0, "{:?}", server.transcript());
    checked(server.shutdown().await);
}
pub fn test() -> marathon_core::model::Test {
    marathon_core::model::Test {
        pkg: "pkg".into(),
        clazz: "clazz".into(),
        method: "method".into(),
        meta_properties: vec![],
    }
}
pub fn js(s: &str) -> String {
    s.into()
}

pub fn change_runtime(runtime: &Arc<AndroidRuntime>, ports: &[u16]) -> Arc<AndroidRuntime> {
    let mut config = runtime.configuration.clone();
    config.adb_servers = ports
        .iter()
        .map(|p| marathon_config::vendor::AdbEndpoint {
            host: js("127.0.0.1"),
            port: *p as i32,
        })
        .collect();
    let mut changed = runtime.as_ref().clone();
    changed.configuration = config;
    Arc::new(changed)
}
/// Waits until each fake server has seen at least `count` logcat streams, i.e. the devices it announced have finished
/// the transport handshake that follows `Connected`.
/// Polls rather than spinning on yield_now, which holds a vendor worker and starves the fake servers on small runners;
/// the deadline only catches a hang.
pub async fn logcat_streams(wanted: &[(&marathon_adb::server::FakeAdbServer, usize)]) {
    checked(
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            for (server, count) in wanted {
                while server
                    .transcript()
                    .iter()
                    .filter(|record| record["type"] == "shell" && record["command"] == "logcat -v long")
                    .count()
                    < *count
                {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            }
        })
        .await,
    );
}
