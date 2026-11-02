//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/xctestrun/v0/Xctestrun.kt

mod test_target;
mod xctestrun;

pub use test_target::{TestTarget, TestTargetParams};
pub use xctestrun::Xctestrun;
