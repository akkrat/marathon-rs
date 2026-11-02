//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/plist/bundle/Naming.kt

use crate::plist::{PlistDict, PlistResult};

#[derive(Clone, Debug, PartialEq)]
pub struct Naming {
    dict: PlistDict,
}

impl Naming {
    pub fn wrap(dict: PlistDict) -> Self {
        Self { dict }
    }

    pub fn bundle_name(&self) -> PlistResult<String> {
        self.dict.required("CFBundleName")
    }

    pub fn bundle_display_name(&self) -> PlistResult<String> {
        self.dict.required("CFBundleDisplayName")
    }

    pub fn bundle_spoken_name(&self) -> PlistResult<String> {
        self.dict.required("CFBundleSpokenName")
    }
}
