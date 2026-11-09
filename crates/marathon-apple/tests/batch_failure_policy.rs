//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/device/BatchFailurePolicyTest.kt
use marathon_apple::{
    device::batch_failure_policy::BatchFailurePolicy,
    logparser::parser::{DeviceFailureException, DeviceFailureReason},
};
use marathon_error::FailureKind;
fn fail(policy: &mut BatchFailurePolicy, passed: bool, progress: bool) -> bool {
    policy
        .on_batch_failed(
            DeviceFailureReason::Unknown,
            DeviceFailureException::new(DeviceFailureReason::Unknown, Some("Test runner exited".into()), None)
                .to_failure(),
            passed,
            progress,
        )
        .is_some()
}
#[test]
fn keeps_the_device_on_batch_failures() {
    let mut p = BatchFailurePolicy::default();
    assert!(!fail(&mut p, false, false));
    assert!(
        p.on_batch_failed(
            DeviceFailureReason::FailedRunner,
            DeviceFailureException::new(DeviceFailureReason::FailedRunner, None, None).to_failure(),
            false,
            false
        )
        .is_none()
    );
}
#[test]
fn loses_the_device_after_consecutive_batch_failures() {
    let mut p = BatchFailurePolicy::default();
    assert!(!fail(&mut p, false, false));
    assert!(!fail(&mut p, false, false));
    assert!(fail(&mut p, false, false));
}
#[test]
fn completed_batch_resets_the_failure_count() {
    let mut p = BatchFailurePolicy::new(2, 0);
    assert!(!fail(&mut p, false, false));
    assert!(p.on_batch_completed(false).is_none());
    assert!(!fail(&mut p, false, false));
    assert!(fail(&mut p, false, false));
}
#[test]
fn failed_batch_with_passed_tests_resets_the_failure_count() {
    let mut p = BatchFailurePolicy::new(2, 0);
    assert!(!fail(&mut p, false, false));
    assert!(!fail(&mut p, true, true));
    assert!(!fail(&mut p, false, false));
    assert!(fail(&mut p, false, false));
}
#[test]
fn keeps_the_device_after_batches_without_progress_by_default() {
    let mut p = BatchFailurePolicy::default();
    for _ in 0..10 {
        assert!(p.on_batch_completed(false).is_none());
    }
}
#[test]
fn loses_the_device_after_consecutive_batches_in_which_no_test_passed_or_failed() {
    let mut p = BatchFailurePolicy::new(3, 3);
    assert!(p.on_batch_completed(false).is_none());
    assert!(p.on_batch_completed(false).is_none());
    let e = p.on_batch_completed(false).unwrap().into_vendor();
    assert_eq!(
        e.failure().cause.as_ref().unwrap().message.as_ref().unwrap().to_owned(),
        "No test has passed or failed in 3 batches in a row"
    );
}
#[test]
fn a_batch_in_which_a_test_passed_or_failed_resets_the_count_of_batches_without_progress() {
    let mut p = BatchFailurePolicy::new(3, 2);
    assert!(p.on_batch_completed(false).is_none());
    assert!(p.on_batch_completed(true).is_none());
    assert!(p.on_batch_completed(false).is_none());
    assert!(!fail(&mut p, false, true));
    assert!(p.on_batch_completed(false).is_none());
    assert!(p.on_batch_completed(false).is_some());
}
#[test]
fn batch_failures_without_progress_count_as_batches_without_progress() {
    let mut p = BatchFailurePolicy::new(3, 3);
    assert!(p.on_batch_completed(false).is_none());
    assert!(!fail(&mut p, false, false));
    assert!(p.on_batch_completed(false).is_some());
}
#[test]
fn loses_the_device_on_the_first_device_failure() {
    use DeviceFailureReason::*;
    for reason in [
        ConnectionAbort,
        InvalidSimulatorIdentifier,
        UnreachableHost,
        ServicesUnavailable,
        IncompatibleDevice,
        SystemAppUnavailable,
    ] {
        for passed in [true, false] {
            assert!(
                BatchFailurePolicy::default()
                    .on_batch_failed(
                        reason,
                        DeviceFailureException::new(reason, None, None).to_failure(),
                        passed,
                        passed
                    )
                    .is_some()
            );
        }
    }
}
#[test]
fn progress_limit_precedes_passed_reset_and_preserves_suppression() {
    let mut p = BatchFailurePolicy::new(3, 1);
    let loss = p
        .on_batch_failed(
            DeviceFailureReason::Unknown,
            DeviceFailureException::new(DeviceFailureReason::Unknown, Some("runner".into()), None).to_failure(),
            true,
            false,
        )
        .unwrap();
    assert_eq!(loss.suppressed.len(), 1);
    let trace = loss.into_vendor().failure().stack_trace().to_owned();
    assert!(trace.find("Suppressed:").unwrap() < trace.find("Caused by:").unwrap());
}
fn timed_out() -> marathon_core::error::Failure {
    marathon_apple::cmd::failure(FailureKind::Timeout, "idle timeout reached")
}
#[test]
fn output_timeouts_without_progress_count_as_batch_failures_d39() {
    let mut p = BatchFailurePolicy::default();
    assert!(p.on_batch_timed_out(timed_out(), false).is_none());
    assert!(p.on_batch_timed_out(timed_out(), false).is_none());
    let loss = p.on_batch_timed_out(timed_out(), false).unwrap();
    assert_eq!(
        loss.cause.kind.legacy_class().to_owned(),
        "java.util.concurrent.TimeoutException"
    );
}
#[test]
fn an_output_timeout_with_progress_resets_the_failure_count_d39() {
    let mut p = BatchFailurePolicy::new(2, 0);
    assert!(p.on_batch_timed_out(timed_out(), false).is_none());
    assert!(p.on_batch_timed_out(timed_out(), true).is_none());
    assert!(p.on_batch_timed_out(timed_out(), false).is_none());
    assert!(p.on_batch_timed_out(timed_out(), false).is_some());
}
#[test]
fn output_timeouts_and_batch_failures_count_together_d39() {
    let mut p = BatchFailurePolicy::new(2, 0);
    assert!(!fail(&mut p, false, false));
    assert!(p.on_batch_timed_out(timed_out(), false).is_some());
}
#[test]
fn a_completed_batch_resets_the_count_of_output_timeouts_d39() {
    let mut p = BatchFailurePolicy::new(2, 0);
    assert!(p.on_batch_timed_out(timed_out(), false).is_none());
    assert!(p.on_batch_completed(false).is_none());
    assert!(p.on_batch_timed_out(timed_out(), false).is_none());
}
