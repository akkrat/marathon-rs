//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/xctestrun/v2/Metadata.kt

use crate::plist::{PlistDict, PlistResult, PropertyList};

#[derive(Clone, Debug, PartialEq)]
pub struct Metadata {
    dict: PlistDict,
}

impl Metadata {
    pub fn new(format_version: i32) -> Self {
        let metadata = Self::wrap(PlistDict::new());
        metadata.set_format_version(format_version);
        metadata
    }

    pub fn format_version(&self) -> PlistResult<i32> {
        self.dict.required("FormatVersion")
    }

    pub fn set_format_version(&self, value: i32) {
        self.dict.set_value("FormatVersion", value);
    }
}

impl PropertyList for Metadata {
    fn wrap(dict: PlistDict) -> Self {
        Self { dict }
    }

    fn dict(&self) -> &PlistDict {
        &self.dict
    }
}
