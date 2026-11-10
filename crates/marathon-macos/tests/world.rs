//! Port of vendor/vendor-apple/macos/src/main/kotlin/com/malinskiy/marathon/apple/macos/AppleMacosProvider.kt (in-process world lifecycle).
use marathon_error::FailureKind;
mod support;

use fake_worker::{
    apple::{Scenario, World},
    fs::VirtualFs,
    router::Router,
    script::{Action, Matcher, Rule, Script},
};
use marathon_apple::{
    apple_device::AppleDevice,
    apple_device_registry::AppleDeviceRegistry,
    cmd::{CommandExecutor, FileBridge},
    device::connection_factory::{Bridge, Executor},
};
use marathon_core::{
    executor::shared::SharedDeferred,
    runtime::VendorFuture,
    vendor::{Device, DeviceProvider, VendorContext},
};
use marathon_macos::{AppleMacosProvider, provider::MacosConnections};
use marathon_test_support::apple::WorldExecutor;
use std::{path::PathBuf, sync::Arc};
use support::*;
struct Connections(Arc<WorldExecutor>, VirtualFs);
struct DirectoryBridge {
    inner: Bridge,
    fs: VirtualFs,
}
impl FileBridge for DirectoryBridge {
    fn send<'a>(&'a self, src: &'a std::path::Path, dst: &'a str) -> VendorFuture<'a, bool> {
        self.inner.send(src, dst)
    }
    fn receive<'a>(&'a self, src: &'a str, dst: &'a std::path::Path) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            // WorldExecutor's raw export targets the exact destination, unlike rsync's directory target.
            self.fs
                .export(src, dst.join(src.rsplit('/').next().unwrap_or(src)))
                .await
                .map(|_| true)
                .map_err(|e| {
                    marathon_core::error::VendorError::Other(marathon_apple::cmd::failure(
                        FailureKind::Io,
                        e.to_string(),
                    ))
                })
        })
    }
}
impl MacosConnections for Connections {
    fn create(
        &self,
        _: marathon_config::devices::Transport,
        _: VendorContext,
    ) -> VendorFuture<'_, (Option<Executor>, Bridge)> {
        Box::pin(async move {
            Ok((
                Some(self.0.clone() as Executor),
                Arc::new(DirectoryBridge {
                    inner: Arc::new(self.0.file_bridge()),
                    fs: self.1.clone(),
                }) as Bridge,
            ))
        })
    }
    fn release(&self, _: Executor) -> VendorFuture<'_, ()> {
        self.0.close()
    }
}
#[tokio::test]
async fn provider_setup_prepare_batch_inline_results_and_terminate_in_apple_world() -> R {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let mut h = Harness::new("")?;
        let config = Arc::make_mut(&mut h.options.vendor);
        config.devices_file = None;
        config.xcresult.pulling_policy = marathon_config::vendor::PullingPolicy::Always;
        config.xcresult.remote_clean = true;
        std::fs::write(
            h.root.path().join("Marathondevices"),
            "workers:\n- transport: {type: local}\n  devices: [{type: host}]\n",
        )?;
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let mut scenario: Scenario = serde_json::from_slice(&std::fs::read(
            fixtures.join("fake-worker/apple/macos-smoke/script/world.json"),
        )?)?;
        scenario.expectations.clear();
        for (dir, executable) in [("MacApp.app", "MacApp"), ("UITests.xctest", "UITests")] {
            let path = h.root.path().join(dir).join("Contents/Info.plist");
            let plist = std::fs::read_to_string(&path)?.replace(
                "</dict>",
                &format!("<key>CFBundleExecutable</key><string>{executable}</string></dict>"),
            );
            std::fs::write(path, plist)?;
        }
        let world = World::new(scenario)?;
        let fs = VirtualFs::create(h.root.path().join("worker")).await?;
        let platform = "/tmp/marathon/Xcode/Platforms/MacOSX.platform";
        let template = format!("{platform}/Developer/Library/Xcode/Agents/XCTRunner.app");
        fs.mkdir(&format!("{template}/Contents/MacOS")).await?;
        fs.write(
            &format!("{template}/Contents/MacOS/XCTRunner"),
            vec![0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0],
            0o755,
        )
        .await?;
        fs.write(
            &format!("{template}/Contents/Info.plist"),
            std::fs::read_to_string(h.root.path().join("MacApp.app/Contents/Info.plist"))?
                .replace("<string>MacApp</string>", "<string>UITests-Runner</string>")
                .into_bytes(),
            0o644,
        )
        .await?;
        let script = Script {
            rules: vec![
                Rule {
                    name: "macos-sdk-template".into(),
                    matcher: Matcher::Exact("xcrun --sdk macosx --show-sdk-platform-path".into()),
                    action: Action::output(&format!("{platform}\n"), 0),
                },
                Rule {
                    name: "template-plist-edit".into(),
                    matcher: Matcher::Prefix("/usr/libexec/PlistBuddy ".into()),
                    action: Action::output("", 0),
                },
            ],
            ..Default::default()
        };
        let worker = Arc::new(WorldExecutor::new(
            h.options.dependencies.io.clone(),
            &mut h.options.logging.registry.lock().unwrap(),
            Router {
                script,
                rsync_remote: "/usr/bin/rsync".into(),
                rsync_binary: "/usr/bin/rsync".into(),
                extensions: vec![Arc::new(world.clone())],
            },
            fs.clone(),
            fixtures.clone(),
        ));
        let provider = AppleMacosProvider::new(
            h.options.clone(),
            Arc::new(AppleDeviceRegistry::default()),
            Arc::new(Connections(worker.clone(), fs.clone())),
            std::path::PathBuf::from(h.root.path().to_string_lossy().into_owned()),
            vec![],
        );
        v(provider.initialize(h.context.clone()).await)?;
        let devices = provider.snapshot();
        assert_eq!(devices.len(), 1);
        let device = devices[0].clone();
        v(device
            .prepare(h.options.dependencies.configuration.clone(), h.context.clone())
            .await)?;
        let root = fs.read(&device.remote_file_manager().remote_xctestrun_file()).await?;
        let root = String::from_utf8(root)?;
        assert!(root.contains("Contents/PlugIns") || root.contains("marathon.xctest"));
        let copied = format!(
            "{}/UITests-Runner.app/Contents/PlugIns/marathon.xctest/Contents/Info.plist",
            device.storage_path()
        );
        assert!(!fs.read(&copied).await?.is_empty());
        let result = SharedDeferred::new();
        v(execute(device.clone(), h.context.clone(), result.clone()).await)?;
        assert!(result.completed().is_some());
        assert!(!world.snapshot()?["sessions"].as_array().ok_or("sessions")?.is_empty());
        let remote = format!("{}/{}.batch.xcresult", device.storage_path(), device.udid());
        assert!(fs.stat(&remote).await.is_err());
        let local = h
            .root
            .path()
            .join("output/device-files/pool")
            .join(&device.info().serial_number)
            .join("xcresult/batch.xcresult");
        // FileManager escapes the serial for local path names; use the shared plan to locate it.
        let plan = h.options.files.plan_folder(
            jcompat::file_manager::FolderType::DeviceFiles,
            Some("pool"),
            Some(&device.info().serial_number),
        );
        let expected = plan.materialize(|_| {})?.join("xcresult/batch.xcresult");
        assert!(
            expected.is_dir(),
            "expected {} (raw {})",
            expected.display(),
            local.display()
        );
        v(provider.join_pending().await)?;
        v(provider.terminate().await)?;
        v(worker.close().await)?;
        assert!(worker.processes().active().await.is_empty());
        world.final_state(&fs).await?;
        world.validate()?;
        Ok(())
    })
    .await?
}
