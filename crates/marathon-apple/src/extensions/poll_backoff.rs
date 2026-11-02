//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/extensions/PollBackoff.kt

#[derive(Debug)]
pub struct PollBackoff {
    min_millis: i64,
    pub max_millis: i64,
    next_millis: i64,
}
impl Default for PollBackoff {
    fn default() -> Self {
        Self {
            min_millis: 1,
            max_millis: 20,
            next_millis: 1,
        }
    }
}
impl PollBackoff {
    pub fn new(min_millis: i64, max_millis: i64) -> Result<Self, String> {
        if !(1..=max_millis).contains(&min_millis) {
            return Err(format!("Poll delay should be in 1..{max_millis} ms, got {min_millis}"));
        }
        Ok(Self {
            min_millis,
            max_millis,
            next_millis: min_millis,
        })
    }
    pub fn next_millis(&mut self) -> i64 {
        let value = self.next_millis;
        self.next_millis = value.wrapping_mul(2).min(self.max_millis);
        value
    }
    pub fn reset(&mut self) {
        self.next_millis = self.min_millis;
    }
}
