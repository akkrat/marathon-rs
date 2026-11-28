//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/device/BatchFailurePolicy.kt

use crate::{apple_device_test_runner::RunError, cmd::failure, logparser::parser::DeviceFailureReason};
use marathon_core::error::{Failure, VendorError};
use marathon_core::execution::batch_health::ConsecutiveBatches;
use marathon_error::FailureKind;

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
        let mut detail = failure(FailureKind::DeviceLost, self.cause.to_string());
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
    consecutive_batch_failures: ConsecutiveBatches,
    batches_without_progress: Option<ConsecutiveBatches>,
}
impl Default for BatchFailurePolicy {
    fn default() -> Self {
        Self::new(ConsecutiveBatches::DEFAULT_LIMIT, 0)
    }
}
impl BatchFailurePolicy {
    pub fn new(max_consecutive_batch_failures: i32, max_batches_without_progress: i32) -> Self {
        Self {
            consecutive_batch_failures: ConsecutiveBatches::new(max_consecutive_batch_failures),
            batches_without_progress: (max_batches_without_progress > 0)
                .then(|| ConsecutiveBatches::new(max_batches_without_progress)),
        }
    }
    pub fn on_batch_completed(&mut self, progress: bool) -> Option<BatchLoss> {
        self.consecutive_batch_failures.reset();
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
        self.count_failure(error, passed_tests, progress)
    }
    /// D39 (U165): output that went silent before any test passed or failed counts as a batch failure. D39 left
    /// such a silence as the only sign of a real failure it misses, where Kotlin counted the failure's text.
    /// A batch with progress resets the count, as Kotlin does for every timeout.
    pub fn on_batch_timed_out(&mut self, error: Failure, progress: bool) -> Option<BatchLoss> {
        if progress {
            return self.on_batch_completed(true);
        }
        self.count_failure(error, false, false)
    }
    fn count_failure(&mut self, error: Failure, passed_tests: bool, progress: bool) -> Option<BatchLoss> {
        if let Some(mut loss) = self.count_progress(progress) {
            loss.suppressed.push(error);
            return Some(loss);
        }
        if passed_tests {
            self.consecutive_batch_failures.reset();
            return None;
        }
        self.consecutive_batch_failures.record().map(|_| BatchLoss::new(error))
    }
    fn count_progress(&mut self, progress: bool) -> Option<BatchLoss> {
        let count = self.batches_without_progress.as_mut()?;
        if progress {
            count.reset();
            return None;
        }
        count.record().map(|count| {
            BatchLoss::new(failure(
                FailureKind::IllegalState,
                format!("No test has passed or failed in {count} batches in a row"),
            ))
        })
    }
}
