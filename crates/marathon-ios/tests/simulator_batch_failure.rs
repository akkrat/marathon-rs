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
