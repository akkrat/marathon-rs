//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/xctestrun/v0/TestTarget.kt

use crate::plist::delegates::OrderedStringMap;
use crate::plist::{PlistDict, PlistResult, PropertyList};

/// The secondary-constructor arguments of the Kotlin class, in constructor order.
/// `TestTarget::with_params` writes them in that order, which is writer-visible.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TestTargetParams {
    pub test_bundle_path: Option<String>,
    pub test_host_path: Option<String>,
    pub testing_environment_variables: OrderedStringMap,
    pub ui_target_app_path: Option<String>,
    pub environment_variables: Option<OrderedStringMap>,
    pub command_line_arguments: Option<Vec<String>>,
    pub ui_target_app_environment_variables: Option<OrderedStringMap>,
    pub ui_target_app_command_line_arguments: Option<Vec<String>>,
    pub baseline_path: Option<String>,
    pub skip_test_identifiers: Option<Vec<String>>,
    pub only_test_identifiers: Option<Vec<String>>,
    pub use_destination_artifacts: Option<bool>,
    pub test_host_bundle_identifier: Option<String>,
    pub test_bundle_destination_relative_path: Option<String>,
    pub ui_target_app_bundle_identifier: Option<String>,
}

impl TestTargetParams {
    pub fn new(testing_environment_variables: OrderedStringMap) -> Self {
        Self {
            testing_environment_variables,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TestTarget {
    dict: PlistDict,
}

impl TestTarget {
    /// The Kotlin secondary constructor: the first three fields are assigned
    /// unconditionally (including `null`, which stores nothing), the rest only when
    /// present — both leave the same dictionary, in the same key order.
    pub fn with_params(mut params: TestTargetParams) -> Self {
        let target = Self::wrap(PlistDict::new());
        target
            .dict
            .set_nullable("TestBundlePath", params.test_bundle_path.take());
        target.dict.set_nullable("TestHostPath", params.test_host_path.take());
        target.dict.set_map(
            "TestingEnvironmentVariables",
            std::mem::take(&mut params.testing_environment_variables),
        );
        target.apply_optional_params(params);
        target
    }

    /// The `?.let` tail of the constructor, shared with the v2 subclass constructor.
    pub(crate) fn apply_optional_params(&self, params: TestTargetParams) {
        self.dict.set_nullable("UITargetAppPath", params.ui_target_app_path);
        self.dict
            .set_nullable_map("EnvironmentVariables", params.environment_variables);
        if let Some(v) = params.command_line_arguments {
            self.dict.set_array("CommandLineArguments", v);
        }
        self.dict.set_nullable_map(
            "UITargetAppEnvironmentVariables",
            params.ui_target_app_environment_variables,
        );
        if let Some(v) = params.ui_target_app_command_line_arguments {
            self.dict.set_array("UITargetAppCommandLineArguments", v);
        }
        self.dict.set_nullable("BaselinePath", params.baseline_path);
        if let Some(v) = params.skip_test_identifiers {
            self.dict.set_array("SkipTestIdentifiers", v);
        }
        if let Some(v) = params.only_test_identifiers {
            self.dict.set_array("OnlyTestIdentifiers", v);
        }
        self.dict
            .set_nullable("UseDestinationArtifacts", params.use_destination_artifacts);
        self.dict
            .set_nullable("TestHostBundleIdentifier", params.test_host_bundle_identifier);
        self.dict.set_nullable(
            "TestBundleDestinationRelativePath",
            params.test_bundle_destination_relative_path,
        );
        self.dict
            .set_nullable("UITargetAppBundleIdentifier", params.ui_target_app_bundle_identifier);
    }

    // Declared nullable in Kotlin but read through the required delegate: an absent key
    // is an error, never `None`. The setters accept the nullable type and store nothing
    // for `None` (dd-plist drops the null write).
    pub fn test_bundle_path(&self) -> PlistResult<String> {
        self.dict.required("TestBundlePath")
    }
    pub fn set_test_bundle_path(&self, value: Option<String>) {
        self.dict.set_nullable("TestBundlePath", value);
    }

    pub fn test_host_path(&self) -> PlistResult<String> {
        self.dict.required("TestHostPath")
    }
    pub fn set_test_host_path(&self, value: Option<String>) {
        self.dict.set_nullable("TestHostPath", value);
    }

    pub fn testing_environment_variables(&self) -> PlistResult<OrderedStringMap> {
        self.dict.required_map("TestingEnvironmentVariables")
    }
    pub fn set_testing_environment_variables(&self, value: Option<OrderedStringMap>) {
        self.dict.set_nullable_map("TestingEnvironmentVariables", value);
    }

    pub fn ui_target_app_path(&self) -> PlistResult<String> {
        self.dict.required("UITargetAppPath")
    }
    pub fn set_ui_target_app_path(&self, value: Option<String>) {
        self.dict.set_nullable("UITargetAppPath", value);
    }

    pub fn environment_variables(&self) -> PlistResult<Option<OrderedStringMap>> {
        self.dict.optional_map("EnvironmentVariables")
    }
    pub fn set_environment_variables(&self, value: Option<OrderedStringMap>) {
        self.dict.set_nullable_map("EnvironmentVariables", value);
    }

    pub fn command_line_arguments(&self) -> PlistResult<Vec<String>> {
        self.dict.optional_array("CommandLineArguments")
    }
    pub fn set_command_line_arguments(&self, value: Vec<String>) {
        self.dict.set_array("CommandLineArguments", value);
    }

    pub fn ui_target_app_environment_variables(&self) -> PlistResult<Option<OrderedStringMap>> {
        self.dict.optional_map("UITargetAppEnvironmentVariables")
    }
    pub fn set_ui_target_app_environment_variables(&self, value: Option<OrderedStringMap>) {
        self.dict.set_nullable_map("UITargetAppEnvironmentVariables", value);
    }

    pub fn ui_target_app_command_line_arguments(&self) -> PlistResult<Vec<String>> {
        self.dict.optional_array("UITargetAppCommandLineArguments")
    }
    pub fn set_ui_target_app_command_line_arguments(&self, value: Vec<String>) {
        self.dict.set_array("UITargetAppCommandLineArguments", value);
    }

    pub fn baseline_path(&self) -> PlistResult<Option<String>> {
        self.dict.optional("BaselinePath")
    }
    pub fn set_baseline_path(&self, value: Option<String>) {
        self.dict.set_nullable("BaselinePath", value);
    }

    pub fn skip_test_identifiers(&self) -> PlistResult<Vec<String>> {
        self.dict.optional_array("SkipTestIdentifiers")
    }
    pub fn set_skip_test_identifiers(&self, value: Vec<String>) {
        self.dict.set_array("SkipTestIdentifiers", value);
    }

    pub fn only_test_identifiers(&self) -> PlistResult<Vec<String>> {
        self.dict.optional_array("OnlyTestIdentifiers")
    }
    pub fn set_only_test_identifiers(&self, value: Vec<String>) {
        self.dict.set_array("OnlyTestIdentifiers", value);
    }

    pub fn use_destination_artifacts(&self) -> PlistResult<Option<bool>> {
        self.dict.optional("UseDestinationArtifacts")
    }
    pub fn set_use_destination_artifacts(&self, value: Option<bool>) {
        self.dict.set_nullable("UseDestinationArtifacts", value);
    }

    pub fn test_host_bundle_identifier(&self) -> PlistResult<Option<String>> {
        self.dict.optional("TestHostBundleIdentifier")
    }
    pub fn set_test_host_bundle_identifier(&self, value: Option<String>) {
        self.dict.set_nullable("TestHostBundleIdentifier", value);
    }

    pub fn test_bundle_destination_relative_path(&self) -> PlistResult<Option<String>> {
        self.dict.optional("TestBundleDestinationRelativePath")
    }
    pub fn set_test_bundle_destination_relative_path(&self, value: Option<String>) {
        self.dict.set_nullable("TestBundleDestinationRelativePath", value);
    }

    pub fn ui_target_app_bundle_identifier(&self) -> PlistResult<Option<String>> {
        self.dict.optional("UITargetAppBundleIdentifier")
    }
    pub fn set_ui_target_app_bundle_identifier(&self, value: Option<String>) {
        self.dict.set_nullable("UITargetAppBundleIdentifier", value);
    }
}

impl PropertyList for TestTarget {
    fn wrap(dict: PlistDict) -> Self {
        Self { dict }
    }

    fn dict(&self) -> &PlistDict {
        &self.dict
    }
}
