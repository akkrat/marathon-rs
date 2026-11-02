//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDeviceXcodeVersionTest.kt
mod simulator_support;
mod support;
use simulator_support::*;
#[tokio::test(start_paused = true)]
async fn xcodebuild_and_xcresulttool_follow_the_worker_s_xcode() -> TestResult {
    for (version, diagnostics, legacy) in [
        ("13.4.1", false, false),
        ("14.0", true, false),
        ("15.4", true, false),
        ("16.4", true, true),
        ("26.6", true, true),
        ("27.0", true, true),
    ] {
        let h = Harness::new(|v| v.xcodebuild_test_args = vec![("-test-timeouts-enabled".into(), "YES".into())])?;
        *h.worker.xcodebuild_version.lock().unwrap() = vec![format!("Xcode {version}"), "Build version example".into()];
        let (d, s) = prepared(&h).await?;
        s.output(failed(1));
        assert!(run(&h, &d).await.0.is_ok());
        let cmds = h.worker.commands.lock().unwrap().clone();
        let xcb = cmds
            .iter()
            .find(|c| c.get(2).is_some_and(|s| s == "test-without-building"))
            .unwrap();
        assert_eq!(
            xcb.windows(2).any(|s| s == ["-collect-test-diagnostics", "never"]),
            diagnostics
        );
        assert!(xcb.windows(2).any(|s| s == ["-test-timeouts-enabled", "YES"]));
        let xcresults: Vec<_> = cmds
            .iter()
            .filter(|c| c.get(1).is_some_and(|s| s == "xcresulttool"))
            .collect();
        assert_eq!(xcresults.len(), 1);
        assert_eq!(xcresults[0].iter().any(|s| s == "--legacy"), legacy);
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn the_test_diagnostics_argument_of_the_user_wins_on_xcode_27() -> TestResult {
    let h = Harness::new(|v| v.xcodebuild_test_args = vec![("-collect-test-diagnostics".into(), "on-failure".into())])?;
    *h.worker.xcodebuild_version.lock().unwrap() = vec!["Xcode 27.0".into(), "Build version 27A266a".into()];
    let (d, s) = prepared(&h).await?;
    s.output(failed(1));
    assert!(run(&h, &d).await.0.is_ok());
    let commands = h.worker.commands.lock().unwrap().clone();
    let xcb = commands
        .iter()
        .find(|c| c.get(2).is_some_and(|s| s == "test-without-building"))
        .unwrap();
    assert!(xcb.windows(2).any(|s| s == ["-collect-test-diagnostics", "on-failure"]));
    assert_eq!(
        xcb.iter().filter(|s| s.as_str() == "-collect-test-diagnostics").count(),
        1
    );
    h.finish(&[d]).await;
    Ok(())
}
