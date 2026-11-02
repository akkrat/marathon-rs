//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/xctestrun/v2/TestConfiguration.kt

use super::test_target::TestTarget;
use crate::plist::{PlistDict, PlistResult, PropertyList};

#[derive(Clone, Debug, PartialEq)]
pub struct TestConfiguration {
    dict: PlistDict,
}

impl TestConfiguration {
    pub fn new(name: String, test_targets: &[TestTarget]) -> Self {
        let configuration = Self::wrap(PlistDict::new());
        configuration.set_name(name);
        configuration.set_test_targets(test_targets);
        configuration
    }

    pub fn name(&self) -> PlistResult<String> {
        self.dict.required("Name")
    }
    pub fn set_name(&self, value: String) {
        self.dict.set_value("Name", value);
    }

    pub fn test_targets(&self) -> PlistResult<Vec<TestTarget>> {
        self.dict.required_model_list("TestTargets")
    }
    pub fn set_test_targets(&self, value: &[TestTarget]) {
        self.dict.set_model_list("TestTargets", value);
    }
}

impl PropertyList for TestConfiguration {
    fn wrap(dict: PlistDict) -> Self {
        Self { dict }
    }

    fn dict(&self) -> &PlistDict {
        &self.dict
    }
}
