//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/plist/bundle/Categorization.kt

use crate::plist::{PlistDict, PlistResult};

#[derive(Clone, Debug, PartialEq)]
pub struct Categorization {
    dict: PlistDict,
}

impl Categorization {
    pub fn wrap(dict: PlistDict) -> Self {
        Self { dict }
    }

    pub fn package_type(&self) -> PlistResult<String> {
        self.dict.required("CFBundlePackageType")
    }

    pub fn application_category_type(&self) -> PlistResult<String> {
        self.dict.required("LSApplicationCategoryType")
    }
}
