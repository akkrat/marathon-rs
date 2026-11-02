//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/xctestrun/v2/TestPlan.kt

use crate::plist::{PlistDict, PlistResult, PropertyList};

#[derive(Clone, Debug, PartialEq)]
pub struct TestPlan {
    dict: PlistDict,
}

impl TestPlan {
    pub fn new(name: String, is_default: bool) -> Self {
        let plan = Self::wrap(PlistDict::new());
        plan.set_name(name);
        plan.set_is_default(is_default);
        plan
    }

    pub fn name(&self) -> PlistResult<String> {
        self.dict.required("Name")
    }
    pub fn set_name(&self, value: String) {
        self.dict.set_value("Name", value);
    }

    pub fn is_default(&self) -> PlistResult<bool> {
        self.dict.required("IsDefault")
    }
    pub fn set_is_default(&self, value: bool) {
        self.dict.set_value("IsDefault", value);
    }
}

impl PropertyList for TestPlan {
    fn wrap(dict: PlistDict) -> Self {
        Self { dict }
    }

    fn dict(&self) -> &PlistDict {
        &self.dict
    }
}
