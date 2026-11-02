//! Port of vendor/vendor-apple/macos/src/test/kotlin/com/malinskiy/marathon/apple/macos/MacosTestRootTest.kt
mod support;
use marathon_apple::apple_device::AppleDevice;
use marathon_core::vendor::Device;
use support::*;
#[tokio::test]
async fn generated_test_runner_still_gets_a_copy_of_the_xctest_in_its_plugins() -> R {
    let h = Harness::new("")?;
    let d = h.device();
    v(d.setup(&h.context).await)?;
    v(
        d.prepare(h.options.dependencies.configuration.clone(), h.context.clone())
            .await,
    )?;
    let commands = h.worker.fake.commands.lock().unwrap().clone();
    assert!(commands.iter().any(|c| c
        == &[
            "cp",
            "-R",
            "/Xcode/Platforms/MacOSX.platform/Developer/Library/Xcode/Agents/XCTRunner.app",
            "/tmp/marathon/udid/UITests-Runner.app"
        ]));
    assert!(commands.iter().any(|c| c
        == &[
            "cp",
            "-R",
            "/tmp/marathon/shared/marathon.xctest",
            "/tmp/marathon/udid/UITests-Runner.app/Contents/PlugIns"
        ]));
    let sent = h.bridge.sent.lock().unwrap().clone();
    let roots = sent
        .iter()
        .filter(|(p, _)| p.ends_with(".xctestrun"))
        .collect::<Vec<_>>();
    assert_eq!(roots.len(), 1);
    use marathon_apple::plist::{PlistValue, dict_from_bytes};
    let root = v(dict_from_bytes(&roots[0].1))?;
    let Some(PlistValue::Array(configs)) = root.get_cloned("TestConfigurations") else {
        return Err("test configurations".into());
    };
    let PlistValue::Dict(config) = &configs[0] else {
        return Err("configuration".into());
    };
    let Some(PlistValue::Array(targets)) = config.get_cloned("TestTargets") else {
        return Err("test targets".into());
    };
    let PlistValue::Dict(target) = &targets[0] else {
        return Err("target".into());
    };
    assert!(
        matches!(target.get_cloned("TestBundlePath"),Some(PlistValue::String(s)) if s=="/tmp/marathon/shared/marathon.xctest")
    );
    assert!(
        matches!(target.get_cloned("TestHostPath"),Some(PlistValue::String(s)) if s=="/tmp/marathon/udid/UITests-Runner.app")
    );
    drop(sent);
    drop(commands);
    v(d.dispose().await)?;
    v(d.join_pending().await)?;
    Ok(())
}
