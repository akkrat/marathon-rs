//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/AppleDevice.kt
mod support;
use marathon_logging::{Component, LogSettings};

use jcompat::jtime::Duration;
use marathon_apple::{
    apple_device::{AppleDevice, apple_worker_id, storage_path_for},
    apple_device_registry::AppleDeviceRegistry,
    bin::AppleBinaryEnvironment,
    cmd::{CommandResult, remote::ssh::sshj::config::SshLogging},
    model::{Arch, Sdk},
    remote_file_manager::RemoteFileManager,
    worker_commands::WorkerCommands,
};
use marathon_config::{Configuration, devices::Transport};
use marathon_core::{
    error::VendorResult,
    listener::{LineListener, LogProducer},
    model::{BatchRef, DeviceInfo, DevicePoolId, TestBatchResults},
    runtime::{CancellationSignal, Deferred, VendorFuture},
    screenshot::{RgbaFrame, Rotation, Screenshottable},
    vendor::{Device, VendorContext},
};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
struct NullCommands;
impl WorkerCommands for NullCommands {
    fn execute_worker_command<'a>(
        &'a self,
        _: Vec<String>,
        _: &'a VendorContext,
    ) -> VendorFuture<'a, Option<CommandResult>> {
        Box::pin(async { Ok(None) })
    }
}
struct Fake {
    manager: RemoteFileManager,
    logger: Component,
    transport: Transport,
    arm: AtomicBool,
    pushes: AtomicUsize,
}
impl Fake {
    fn new() -> Self {
        let registry = Arc::new(Mutex::new(LogSettings::default()));
        let logger = registry.lock().unwrap().component("Fake");
        Self {
            manager: RemoteFileManager::new(
                "/s".into(),
                "U".into(),
                Arc::new(NullCommands),
                SshLogging::new(registry, Arc::new(|_, _, _, _| {})),
            ),
            logger,
            transport: Transport::Local,
            arm: AtomicBool::new(false),
            pushes: AtomicUsize::new(0),
        }
    }
}
impl Device for Fake {
    fn info(&self) -> DeviceInfo {
        panic!("unused by identity lookup")
    }
    fn abi(&self) -> String {
        if self.arm.load(Ordering::Relaxed) {
            "arm64".into()
        } else {
            "x86_64".into()
        }
    }
    fn worker_id(&self) -> Option<String> {
        apple_worker_id(&self.transport)
    }
    fn component(&self) -> &Component {
        &self.logger
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
        panic!("unused")
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
        "U"
    }
    fn remote_file_manager(&self) -> &RemoteFileManager {
        &self.manager
    }
    fn storage_path(&self) -> &str {
        "/s"
    }
    fn transport(&self) -> &Transport {
        &self.transport
    }
    fn sdk(&self) -> Sdk {
        Sdk::IphoneSimulator
    }
    fn binary_environment(&self) -> &AppleBinaryEnvironment {
        panic!("unused")
    }
    fn push_file<'a>(&'a self, src: &'a Path, dst: &'a str, _: &'a VendorContext) -> VendorFuture<'a, bool> {
        assert_eq!(src, Path::new("generated"));
        assert_eq!(dst, "remote");
        self.pushes.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { Ok(true) })
    }
    unused!(setup()->());
    unused!(execute_worker_command(command: Vec<String>) -> Option<CommandResult>);
    unused!(pull_file(src: &'a str, dst: &'a Path) -> bool);
    unused!(push_folder(src: &'a Path, dst: &'a str) -> bool);
    unused!(pull_folder(src: &'a str, dst: &'a Path) -> bool);
    unused!(push_shared(src: &'a Path, dst: &'a str) -> bool);
    unused!(install(path: &'a str) -> bool);
    unused!(get_screenshot(timeout: Duration, dst: &'a Path) -> bool);
    unused!(start_video_recording(path: &'a str) -> Option<CommandResult>);
    unused!(stop_video_recording()->bool);
    unused!(shutdown()->bool);
    unused!(erase()->bool);
}
#[tokio::test]
async fn object_safe_defaults_delegate_and_recompute_after_setup() -> VendorResult<()> {
    let fake = Arc::new(Fake::new());
    let apple: Arc<dyn AppleDevice> = fake.clone();
    assert_eq!(apple.arch(), Arch::x86_64);
    fake.arm.store(true, Ordering::Relaxed);
    assert_eq!(apple.arch(), Arch::arm64);
    assert!(
        apple
            .push_generated_file(Path::new("generated"), "remote", &support::context())
            .await?
    );
    assert_eq!(fake.pushes.load(Ordering::Relaxed), 1);
    assert_eq!(apple.worker_id(), Some("local".into()));
    assert_eq!(
        apple_worker_id(&Transport::Ssh {
            addr: "addr".into(),
            port: 2222,
            authentication: None,
            check_reachability: false
        }),
        Some("addr:2222".into())
    );
    assert_eq!(storage_path_for("U"), "/tmp/marathon/U");
    Ok(())
}
#[test]
fn registry_uses_object_identity_and_removes_disposed_devices() {
    let registry = AppleDeviceRegistry::default();
    let first: Arc<dyn AppleDevice> = Arc::new(Fake::new());
    let second: Arc<dyn AppleDevice> = Arc::new(Fake::new());
    let first_id = registry.register(first.clone());
    let duplicate = registry.register(first.clone());
    let second_id = registry.register(second.clone());
    assert_eq!(first_id, duplicate);
    assert_ne!(first_id, second_id);
    assert!(Arc::ptr_eq(&registry.lookup(&first_id.clone()).ok().unwrap(), &first));
    registry.remove(&first_id);
    registry.remove(&first_id);
    let error = registry.lookup(&first_id).err().unwrap();
    assert_eq!(error.failure().kind.description(), "configuration rejected".to_owned());
    assert_eq!(
        error.failure().message,
        Some("Unexpected device type for remote test parsing".into())
    );
    assert!(Arc::ptr_eq(&registry.lookup(&second_id).ok().unwrap(), &second));
}
