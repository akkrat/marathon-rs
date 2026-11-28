//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/device/BatchFailurePolicy.kt (the counter).

/// A vendor's serial execute path decides which batches to count and which reset it.
pub struct ConsecutiveBatches {
    limit: i32,
    count: i32,
}

impl ConsecutiveBatches {
    pub const DEFAULT_LIMIT: i32 = 3;

    pub fn new(limit: i32) -> Self {
        Self { limit, count: 0 }
    }

    pub fn reset(&mut self) {
        self.count = 0;
    }

    pub fn record(&mut self) -> Option<i32> {
        self.count = self.count.wrapping_add(1);
        (self.count >= self.limit).then_some(self.count)
    }
}
