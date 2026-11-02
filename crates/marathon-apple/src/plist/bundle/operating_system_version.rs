//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/plist/bundle/OperatingSystemVersion.kt

use crate::plist::{PlistDict, PlistResult};

#[derive(Clone, Debug, PartialEq)]
pub struct OperatingSystemVersion {
    dict: PlistDict,
}

impl OperatingSystemVersion {
    pub fn wrap(dict: PlistDict) -> Self {
        Self { dict }
    }

    pub fn ls_minimum_system_version(&self) -> PlistResult<String> {
        self.dict.required("LSMinimumSystemVersion")
    }

    pub fn minimum_os_version(&self) -> PlistResult<String> {
        self.dict.required("MinimumOSVersion")
    }

    pub fn ls_requires_iphone_os(&self) -> PlistResult<bool> {
        self.dict.required("LSRequiresIPhoneOS")
    }

    pub fn watch_kit_app(&self) -> PlistResult<bool> {
        self.dict.required("WKWatchKitApp")
    }
}
