//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDeviceBatchFailureTest.kt
mod simulator_support;
mod support;
use marathon_apple::logparser::parser::{DeviceFailureReason, device_failure::FAILURE_PATTERNS};
use simulator_support::*;
use std::sync::atomic::Ordering;
#[tokio::test(start_paused = true)]
async fn a_dead_system_app_drops_the_simulator_after_one_failed_batch() -> TestResult {
    for message in [
        "Terminating since there is no system app.",
        "Exiting because the workspace server has disconnected.",
    ] {
        let h = Harness::new(|_| {})?;
        let (d, s) = prepared(&h).await?;
        s.output(vec![message.into()]);
        let (r, results) = run(&h, &d).await;
        assert!(matches!(r, Err(marathon_core::error::VendorError::DeviceLost(_))));
        let error = r.unwrap_err();
        let cause = error.failure().cause.as_ref().unwrap();
        assert_eq!(
            cause.class.to_string_lossy(),
            "com.malinskiy.marathon.apple.logparser.parser.DeviceFailureException"
        );
        // The core bridge retains the reason's matching pattern as the cause message.
        let pattern = cause.message.as_ref().unwrap().to_string_lossy();
        assert_eq!(
            FAILURE_PATTERNS.iter().find(|(p, _)| *p == pattern).unwrap().1,
            DeviceFailureReason::SystemAppUnavailable
        );
        assert_eq!(s.count.load(Ordering::SeqCst), 1);
        assert_eq!(results.unwrap().uncompleted.len(), 2);
        assert!(d.retired());
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_batch_level_failure_keeps_the_simulator() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    s.output(vec!["Test runner exited before starting test execution.".into()]);
    let (r, results) = run(&h, &d).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(results.unwrap().uncompleted.len(), 2);
    assert!(!d.lost());
    assert!(!d.retired());
    assert_eq!(s.count.load(Ordering::SeqCst), 1);
    s.output(vec!["Terminating since there is no system app.".into()]);
    let (r, _) = run(&h, &d).await;
    assert!(matches!(r, Err(marathon_core::error::VendorError::DeviceLost(_))));
    assert_eq!(s.count.load(Ordering::SeqCst), 2);
    assert!(d.retired());
    h.finish(&[d]).await;
    Ok(())
}
/// Each failure literal in a test's name, an assertion, an activity line and an application's log.
fn test_text() -> Vec<String> {
    FAILURE_PATTERNS
        .iter()
        .flat_map(|(literal, _)| {
            [
                format!("Test Suite '{literal}' started at 2026-11-06 12:08:08.118."),
                format!("/a/T.swift:33: error: -[Suite test1] : XCTAssertTrue failed - {literal}"),
                format!("    t =     4.20s Tap \"{literal}\" Button"),
                format!("2026-11-06 12:08:08.118250+0300 Some App[27239:2608175] [general] {literal}: E"),
            ]
        })
        .collect()
}
#[tokio::test(start_paused = true)]
async fn a_tests_own_text_does_not_end_the_batch_d39() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    let mut lines = test_text();
    lines.extend(passed(1));
    lines.extend(passed(2));
    s.output(lines);
    let (r, results) = run(&h, &d).await;
    assert!(r.is_ok(), "{r:?}");
    let results = results.unwrap();
    assert_eq!(results.finished.len(), 2);
    assert!(results.uncompleted.is_empty());
    assert!(!d.lost());
    assert!(!d.retired());
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_real_failure_after_a_tests_own_text_still_ends_the_batch_d39() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    let mut lines = test_text();
    lines.extend(passed(1));
    lines.push("2026-11-06 12:08:08.118250+0300 Tests-Runner[1:2] Terminating since there is no system app.".into());
    lines.extend(passed(2));
    s.output(lines);
    let (r, results) = run(&h, &d).await;
    assert!(matches!(r, Err(marathon_core::error::VendorError::DeviceLost(_))));
    let results = results.unwrap();
    assert_eq!(results.finished.len(), 1);
    assert_eq!(results.uncompleted.len(), 1);
    assert!(d.retired());
    h.finish(&[d]).await;
    Ok(())
}
