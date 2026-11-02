//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/plist/bundle/Identification.kt

use crate::plist::{PlistDict, PlistResult};

#[derive(Clone, Debug, PartialEq)]
pub struct Identification {
    dict: PlistDict,
}

impl Identification {
    pub fn wrap(dict: PlistDict) -> Self {
        Self { dict }
    }

    pub fn bundle_identifier(&self) -> PlistResult<String> {
        self.dict.required("CFBundleIdentifier")
    }

    pub fn watch_kit_bundle_identifier(&self) -> PlistResult<String> {
        self.dict.required("WKAppBundleIdentifier")
    }

    pub fn watch_kit_companion_app_bundle_identifier(&self) -> PlistResult<String> {
        self.dict.required("WKCompanionAppBundleIdentifier")
    }
}
