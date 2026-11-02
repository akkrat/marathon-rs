//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/model/XcodeVersion.kt
//!
//! Approved P1: components parse as ASCII `i32` (Kotlin's `toInt` also takes non-ASCII digits), and
//! comparison is lexicographic (Kotlin subtracts, which overflows only for components near `Int` bounds).

use crate::{bin::device_setup, bin::kt, cmd::failure};
use marathon_core::error::{VendorError, VendorResult};

/// The derived order is the lexicographic `(major, minor, patch)` comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct XcodeVersion {
    pub major: i32,
    pub minor: i32,
    pub patch: i32,
}
impl XcodeVersion {
    pub const fn new(major: i32, minor: i32, patch: i32) -> Self {
        Self { major, minor, patch }
    }
    /// A number fails before a wrong component count does.
    #[allow(clippy::should_implement_trait)]
    pub fn from(value: &str) -> VendorResult<Self> {
        let components = kt::trim(value)
            .split('.')
            .map(|component| {
                let component = kt::trim(component);
                component.parse::<i32>().map_err(|_| {
                    VendorError::Other(failure(
                        "java.lang.NumberFormatException",
                        format!("For input string: \"{component}\""),
                    ))
                })
            })
            .collect::<VendorResult<Vec<_>>>()?;
        match components[..] {
            [major] => Ok(Self::new(major, 0, 0)),
            [major, minor] => Ok(Self::new(major, minor, 0)),
            [major, minor, patch] => Ok(Self::new(major, minor, patch)),
            _ => Err(device_setup(format!(
                "Invalid xcode version: {value}. Expecting semi-symver format"
            ))),
        }
    }
}
