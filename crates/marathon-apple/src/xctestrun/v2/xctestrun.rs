//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/xctestrun/v2/Xctestrun.kt

use super::code_coverage_buildable_info::CodeCoverageBuildableInfo;
use super::metadata::Metadata;
use super::test_configuration::TestConfiguration;
use super::test_plan::TestPlan;
use crate::plist::{PlistDict, PlistResult, PropertyList};

#[derive(Clone, Debug, PartialEq)]
pub struct Xctestrun {
    dict: PlistDict,
}

impl Xctestrun {
    /// Constructor write order is writer-visible: metadata, configurations, then the
    /// optional plan and coverage infos.
    pub fn new(
        metadata: &Metadata,
        test_configurations: &[TestConfiguration],
        test_plan: Option<&TestPlan>,
        code_coverage_buildable_infos: Option<&[CodeCoverageBuildableInfo]>,
    ) -> Self {
        let run = Self::wrap(PlistDict::new());
        run.set_metadata(metadata);
        run.set_test_configurations(test_configurations);
        if let Some(plan) = test_plan {
            run.set_test_plan(Some(plan));
        }
        if let Some(infos) = code_coverage_buildable_infos {
            run.set_code_coverage_buildable_infos(infos);
        }
        run
    }

    pub fn metadata(&self) -> PlistResult<Metadata> {
        self.dict.required_model("__xctestrun_metadata__")
    }
    pub fn set_metadata(&self, value: &Metadata) {
        self.dict.set_model("__xctestrun_metadata__", value);
    }

    pub fn test_plan(&self) -> PlistResult<Option<TestPlan>> {
        self.dict.optional_model("TestPlan")
    }
    /// `optionalPlistDelegateFor` write: `None` never clears the key.
    pub fn set_test_plan(&self, value: Option<&TestPlan>) {
        self.dict.set_optional_model("TestPlan", value);
    }

    pub fn code_coverage_buildable_infos(&self) -> PlistResult<Vec<CodeCoverageBuildableInfo>> {
        self.dict.optional_model_list("CodeCoverageBuildableInfos")
    }
    pub fn set_code_coverage_buildable_infos(&self, value: &[CodeCoverageBuildableInfo]) {
        self.dict.set_model_list("CodeCoverageBuildableInfos", value);
    }

    pub fn test_configurations(&self) -> PlistResult<Vec<TestConfiguration>> {
        self.dict.required_model_list("TestConfigurations")
    }
    pub fn set_test_configurations(&self, value: &[TestConfiguration]) {
        self.dict.set_model_list("TestConfigurations", value);
    }
}

impl PropertyList for Xctestrun {
    fn wrap(dict: PlistDict) -> Self {
        Self { dict }
    }

    fn dict(&self) -> &PlistDict {
        &self.dict
    }
}
