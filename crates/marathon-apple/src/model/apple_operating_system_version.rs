//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/model/AppleOperatingSystemVersion.kt
//!
//! Unlike [`super::XcodeVersion`]: no trimming, at most three components, and an unparseable minor or
//! patch is zero. Approved P1: ASCII digits only.

use crate::cmd::failure;
use marathon_core::error::{VendorError, VendorResult};

const INCOMPATIBLE_DEVICE_EXCEPTION: &str = "com.malinskiy.marathon.exceptions.IncompatibleDeviceException";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AppleOperatingSystemVersion {
    pub major: i32,
    pub minor: i32,
    pub patch: i32,
}
impl AppleOperatingSystemVersion {
    pub fn new(value: &str) -> VendorResult<Self> {
        let split: Vec<&str> = value.split('.').collect();
        let incompatible = |message: String| VendorError::Other(failure(INCOMPATIBLE_DEVICE_EXCEPTION, message));
        if split.len() > 3 {
            return Err(incompatible(format!("Unknown operating system version {value}")));
        }
        let major = split[0]
            .parse::<i32>()
            .map_err(|_| incompatible(format!("Unable to parser operating system version {value}")))?;
        let component = |index: usize| split.get(index).and_then(|c| c.parse::<i32>().ok()).unwrap_or(0);
        Ok(Self {
            major,
            minor: component(1),
            patch: component(2),
        })
    }
}
