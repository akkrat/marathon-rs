//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/device/BatchFailurePolicy.kt

use crate::{apple_device_test_runner::RunError, cmd::failure, logparser::parser::DeviceFailureReason};
use marathon_core::error::{Failure, VendorError};

#[derive(Clone, Debug)]
pub struct BatchLoss {
    pub cause: Failure,
    pub suppressed: Vec<Failure>,
}
impl BatchLoss {
    pub fn new(cause: Failure) -> Self {
        Self {
            cause,
            suppressed: vec![],
        }
    }
    pub fn into_vendor(self) -> VendorError {
        let mut detail = failure(
            "com.malinskiy.marathon.exceptions.DeviceLostException",
            self.cause.to_java_string().to_string_lossy(),
        );
        detail.cause = Some(Box::new(self.cause));
        // Policy suppression belongs to DeviceLost itself, rather than its cause.
        let flattened = RunError {
            error: VendorError::Other(detail),
            reason: None,
            suppressed: self.suppressed.into_boxed_slice(),
        }
        .into_vendor();
        VendorError::DeviceLost(flattened.failure().clone())
    }
}
/// Owned by the serial execute path, or protected by the device's short state lock.
pub struct BatchFailurePolicy {
    max_consecutive_batch_failures: i32,
    max_batches_without_progress: i32,
    consecutive_batch_failures: i32,
    batches_without_progress: i32,
}
impl Default for BatchFailurePolicy {
    fn default() -> Self {
        Self::new(3, 0)
    }
}
impl BatchFailurePolicy {
    pub fn new(max_consecutive_batch_failures: i32, max_batches_without_progress: i32) -> Self {
        Self {
            max_consecutive_batch_failures,
            max_batches_without_progress,
            consecutive_batch_failures: 0,
            batches_without_progress: 0,
        }
    }
    pub fn on_batch_completed(&mut self, progress: bool) -> Option<BatchLoss> {
        self.consecutive_batch_failures = 0;
        self.count_progress(progress)
    }
    pub fn on_batch_failed(
        &mut self,
        reason: DeviceFailureReason,
        error: Failure,
        passed_tests: bool,
        progress: bool,
    ) -> Option<BatchLoss> {
        if !reason.is_batch_failure() {
            return Some(BatchLoss::new(error));
        }
        if let Some(mut loss) = self.count_progress(progress) {
            loss.suppressed.push(error);
            return Some(loss);
        }
        if passed_tests {
            self.consecutive_batch_failures = 0;
            return None;
        }
        self.consecutive_batch_failures = self.consecutive_batch_failures.wrapping_add(1);
        (self.consecutive_batch_failures >= self.max_consecutive_batch_failures).then(|| BatchLoss::new(error))
    }
    fn count_progress(&mut self, progress: bool) -> Option<BatchLoss> {
        if progress {
            self.batches_without_progress = 0;
            return None;
        }
        self.batches_without_progress = self.batches_without_progress.wrapping_add(1);
        (self.max_batches_without_progress > 0 && self.batches_without_progress >= self.max_batches_without_progress)
            .then(|| {
                BatchLoss::new(failure(
                    "java.lang.IllegalStateException",
                    format!(
                        "No test has passed or failed in {} batches in a row",
                        self.batches_without_progress
                    ),
                ))
            })
    }
}
