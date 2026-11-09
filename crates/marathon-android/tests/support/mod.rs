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
fn on_core(
    core: std::rc::Rc<CoreExecutor>,
    c: Configuration,
    body: impl FnOnce(Arc<AndroidRuntime>, VendorContext) -> VendorFuture<'static, ()> + Send + 'static,
) -> marathon_core::runtime::LocalFuture<'static, marathon_core::error::OperationResult<()>> {
    {
        Box::pin(async move {
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
pub fn expect_setup(commands: &[String]) {
    assert_eq!(
        &commands[..6],
        [
            "getprop",
            "dumpsys input",
            "echo $EXTERNAL_STORAGE",
            "ls /system/bin/screenrecord",
            "ls /system/bin/md5",
            "getprop"
        ]
    )
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
    Arc::new(AndroidRuntime {
        dependencies: runtime.dependencies.clone(),
        configuration: config,
        file_sync: runtime.file_sync.clone(),
        bundles: runtime.bundles.clone(),
        identifier: runtime.identifier.clone(),
        executor: runtime.executor.clone(),
        log: runtime.log.clone(),
    })
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
