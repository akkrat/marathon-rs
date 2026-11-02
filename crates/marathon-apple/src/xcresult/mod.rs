//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/xcresulttool/AppleJsonTypeAdapterFactory.kt
//!
//! Vendor-local legacy xcresult models. Decode large documents off the core
//! executor, through the vendor runtime. No filesystem or command work occurs here.

mod decoder;
mod generated;

pub use decoder::{DecodeErrorKind, DecodePathSegment, LegacyDecode, XcresultDecodeError, decode_legacy};
pub use generated::*;
pub use jcompat::jackson::JavaDate;

#[cfg(test)]
mod tests;
