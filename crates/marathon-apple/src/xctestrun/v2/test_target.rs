//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/xctestrun/v2/TestTarget.kt

use std::ops::Deref;

use crate::plist::delegates::OrderedStringMap;
use crate::plist::{PlistDict, PlistResult, PropertyList};
use crate::xctestrun::v0::{TestTarget as V0TestTarget, TestTargetParams as V0Params};

/// The v2-only constructor tail, written after the v0 fields and `BlueprintName`.
#[derive(Clone, Debug, Default, PartialEq)]
struct V2Tail {
    treat_missing_baselines_as_failures: Option<bool>,
    ui_target_app_main_thread_checker_enabled: Option<bool>,
    gather_localizable_strings_data: Option<bool>,
    dependent_product_paths: Option<Vec<String>>,
    product_module_name: Option<String>,
    system_attachment_lifetime: Option<String>,
    user_attachment_lifetime: Option<String>,
    parallelization_enabled: Option<bool>,
    test_execution_ordering: Option<String>,
    test_language: Option<String>,
    test_region: Option<String>,
    is_ui_test_bundle: Option<bool>,
    is_app_hosted_test_bundle: Option<bool>,
    preferred_screen_capture_format: Option<String>,
}

/// `withArtifactReinstall` arguments. The optional fields are the `?.let` writes.
#[derive(Clone, Debug, PartialEq)]
pub struct ArtifactReinstallParams {
    pub name: String,
    pub test_bundle_path: String,
    pub test_host_path: String,
    pub testing_environment_variables: OrderedStringMap,
    pub ui_target_app_path: Option<String>,
    pub environment_variables: Option<OrderedStringMap>,
    pub command_line_arguments: Option<Vec<String>>,
    pub ui_target_app_environment_variables: Option<OrderedStringMap>,
    pub ui_target_app_command_line_arguments: Option<Vec<String>>,
    pub baseline_path: Option<String>,
    pub skip_test_identifiers: Option<Vec<String>>,
    pub only_test_identifiers: Option<Vec<String>>,
    pub treat_missing_baselines_as_failures: Option<bool>,
    pub ui_target_app_main_thread_checker_enabled: Option<bool>,
    pub gather_localizable_strings_data: Option<bool>,
    pub dependent_product_paths: Option<Vec<String>>,
    pub product_module_name: Option<String>,
    pub system_attachment_lifetime: Option<String>,
    pub user_attachment_lifetime: Option<String>,
    pub parallelization_enabled: Option<bool>,
    pub test_execution_ordering: Option<String>,
    pub test_language: Option<String>,
    pub test_region: Option<String>,
    pub is_ui_test_bundle: Option<bool>,
    pub is_app_hosted_test_bundle: Option<bool>,
    pub preferred_screen_capture_format: Option<String>,
}

impl ArtifactReinstallParams {
    pub fn new(
        name: String,
        test_bundle_path: String,
        test_host_path: String,
        testing_environment_variables: OrderedStringMap,
    ) -> Self {
        Self {
            name,
            test_bundle_path,
            test_host_path,
            testing_environment_variables,
            ui_target_app_path: None,
            environment_variables: None,
            command_line_arguments: None,
            ui_target_app_environment_variables: None,
            ui_target_app_command_line_arguments: None,
            baseline_path: None,
            skip_test_identifiers: None,
            only_test_identifiers: None,
            treat_missing_baselines_as_failures: None,
            ui_target_app_main_thread_checker_enabled: None,
            gather_localizable_strings_data: None,
            dependent_product_paths: None,
            product_module_name: None,
            system_attachment_lifetime: None,
            user_attachment_lifetime: None,
            parallelization_enabled: None,
            test_execution_ordering: None,
            test_language: None,
            test_region: None,
            is_ui_test_bundle: None,
            is_app_hosted_test_bundle: None,
            preferred_screen_capture_format: None,
        }
    }
}

/// `withArtifactReuse` arguments: no bundle/host paths, `UseDestinationArtifacts` forced
/// on. The v0 constructor still assigns the absent paths as `null`, which stores nothing
/// (probe `null-writes`), so the written file has neither key.
#[derive(Clone, Debug, PartialEq)]
pub struct ArtifactReuseParams {
    pub name: String,
    pub test_host_bundle_identifier: String,
    pub test_bundle_destination_relative_path: String,
    pub ui_target_app_bundle_identifier: String,
    pub testing_environment_variables: OrderedStringMap,
    pub environment_variables: Option<OrderedStringMap>,
    pub command_line_arguments: Option<Vec<String>>,
    pub ui_target_app_environment_variables: Option<OrderedStringMap>,
    pub ui_target_app_command_line_arguments: Option<Vec<String>>,
    pub baseline_path: Option<String>,
    pub skip_test_identifiers: Option<Vec<String>>,
    pub only_test_identifiers: Option<Vec<String>>,
    pub treat_missing_baselines_as_failures: Option<bool>,
    pub ui_target_app_main_thread_checker_enabled: Option<bool>,
    pub gather_localizable_strings_data: Option<bool>,
    pub dependent_product_paths: Option<Vec<String>>,
    pub product_module_name: Option<String>,
    pub system_attachment_lifetime: Option<String>,
    pub user_attachment_lifetime: Option<String>,
    pub parallelization_enabled: Option<bool>,
    pub test_execution_ordering: Option<String>,
    pub test_language: Option<String>,
    pub test_region: Option<String>,
    pub is_ui_test_bundle: Option<bool>,
}

impl ArtifactReuseParams {
    pub fn new(
        name: String,
        test_host_bundle_identifier: String,
        test_bundle_destination_relative_path: String,
        ui_target_app_bundle_identifier: String,
        testing_environment_variables: OrderedStringMap,
    ) -> Self {
        Self {
            name,
            test_host_bundle_identifier,
            test_bundle_destination_relative_path,
            ui_target_app_bundle_identifier,
            testing_environment_variables,
            environment_variables: None,
            command_line_arguments: None,
            ui_target_app_environment_variables: None,
            ui_target_app_command_line_arguments: None,
            baseline_path: None,
            skip_test_identifiers: None,
            only_test_identifiers: None,
            treat_missing_baselines_as_failures: None,
            ui_target_app_main_thread_checker_enabled: None,
            gather_localizable_strings_data: None,
            dependent_product_paths: None,
            product_module_name: None,
            system_attachment_lifetime: None,
            user_attachment_lifetime: None,
            parallelization_enabled: None,
            test_execution_ordering: None,
            test_language: None,
            test_region: None,
            is_ui_test_bundle: None,
        }
    }
}

/// The Kotlin class extends the v0 target; `Deref` gives the same field access over the
/// same shared dictionary, and the types stay distinct so a v0 and a v2 target over
/// identical dictionaries are never compared (Kotlin's equality is class-sensitive).
#[derive(Clone, Debug, PartialEq)]
pub struct TestTarget {
    base: V0TestTarget,
}

impl Deref for TestTarget {
    type Target = V0TestTarget;

    fn deref(&self) -> &V0TestTarget {
        &self.base
    }
}

impl TestTarget {
    fn build(name: String, v0: V0Params, tail: V2Tail) -> Self {
        let target = Self {
            base: V0TestTarget::with_params(v0),
        };
        target.set_name(name);
        target.apply_tail(tail);
        target
    }

    fn apply_tail(&self, tail: V2Tail) {
        let dict = self.base.dict();
        dict.set_nullable(
            "TreatMissingBaselinesAsFailures",
            tail.treat_missing_baselines_as_failures,
        );
        dict.set_nullable(
            "UITargetAppMainThreadCheckerEnabled",
            tail.ui_target_app_main_thread_checker_enabled,
        );
        dict.set_nullable("GatherLocalizableStringsData", tail.gather_localizable_strings_data);
        if let Some(v) = tail.dependent_product_paths {
            dict.set_array("DependentProductPaths", v);
        }
        dict.set_nullable("ProductModuleName", tail.product_module_name);
        dict.set_nullable("SystemAttachmentLifetime", tail.system_attachment_lifetime);
        dict.set_nullable("UserAttachmentLifetime", tail.user_attachment_lifetime);
        dict.set_nullable("ParallelizationEnabled", tail.parallelization_enabled);
        dict.set_nullable("TestExecutionOrdering", tail.test_execution_ordering);
        dict.set_nullable("TestLanguage", tail.test_language);
        dict.set_nullable("TestRegion", tail.test_region);
        dict.set_nullable("IsUITestBundle", tail.is_ui_test_bundle);
        dict.set_nullable("IsAppHostedTestBundle", tail.is_app_hosted_test_bundle);
        dict.set_nullable("PreferredScreenCaptureFormat", tail.preferred_screen_capture_format);
    }

    pub fn with_artifact_reinstall(params: ArtifactReinstallParams) -> Self {
        let v0 = V0Params {
            test_bundle_path: Some(params.test_bundle_path),
            test_host_path: Some(params.test_host_path),
            testing_environment_variables: params.testing_environment_variables,
            ui_target_app_path: params.ui_target_app_path,
            environment_variables: params.environment_variables,
            command_line_arguments: params.command_line_arguments,
            ui_target_app_environment_variables: params.ui_target_app_environment_variables,
            ui_target_app_command_line_arguments: params.ui_target_app_command_line_arguments,
            baseline_path: params.baseline_path,
            skip_test_identifiers: params.skip_test_identifiers,
            only_test_identifiers: params.only_test_identifiers,
            use_destination_artifacts: None,
            test_host_bundle_identifier: None,
            test_bundle_destination_relative_path: None,
            ui_target_app_bundle_identifier: None,
        };
        let tail = V2Tail {
            treat_missing_baselines_as_failures: params.treat_missing_baselines_as_failures,
            ui_target_app_main_thread_checker_enabled: params.ui_target_app_main_thread_checker_enabled,
            gather_localizable_strings_data: params.gather_localizable_strings_data,
            dependent_product_paths: params.dependent_product_paths,
            product_module_name: params.product_module_name,
            system_attachment_lifetime: params.system_attachment_lifetime,
            user_attachment_lifetime: params.user_attachment_lifetime,
            parallelization_enabled: params.parallelization_enabled,
            test_execution_ordering: params.test_execution_ordering,
            test_language: params.test_language,
            test_region: params.test_region,
            is_ui_test_bundle: params.is_ui_test_bundle,
            is_app_hosted_test_bundle: params.is_app_hosted_test_bundle,
            preferred_screen_capture_format: params.preferred_screen_capture_format,
        };
        Self::build(params.name, v0, tail)
    }

    pub fn with_artifact_reuse(params: ArtifactReuseParams) -> Self {
        let v0 = V0Params {
            test_bundle_path: None,
            test_host_path: None,
            testing_environment_variables: params.testing_environment_variables,
            ui_target_app_path: None,
            environment_variables: params.environment_variables,
            command_line_arguments: params.command_line_arguments,
            ui_target_app_environment_variables: params.ui_target_app_environment_variables,
            ui_target_app_command_line_arguments: params.ui_target_app_command_line_arguments,
            baseline_path: params.baseline_path,
            skip_test_identifiers: params.skip_test_identifiers,
            only_test_identifiers: params.only_test_identifiers,
            use_destination_artifacts: Some(true),
            test_host_bundle_identifier: Some(params.test_host_bundle_identifier),
            test_bundle_destination_relative_path: Some(params.test_bundle_destination_relative_path),
            ui_target_app_bundle_identifier: Some(params.ui_target_app_bundle_identifier),
        };
        let tail = V2Tail {
            treat_missing_baselines_as_failures: params.treat_missing_baselines_as_failures,
            ui_target_app_main_thread_checker_enabled: params.ui_target_app_main_thread_checker_enabled,
            gather_localizable_strings_data: params.gather_localizable_strings_data,
            dependent_product_paths: params.dependent_product_paths,
            product_module_name: params.product_module_name,
            system_attachment_lifetime: params.system_attachment_lifetime,
            user_attachment_lifetime: params.user_attachment_lifetime,
            parallelization_enabled: params.parallelization_enabled,
            test_execution_ordering: params.test_execution_ordering,
            test_language: params.test_language,
            test_region: params.test_region,
            is_ui_test_bundle: params.is_ui_test_bundle,
            is_app_hosted_test_bundle: None,
            preferred_screen_capture_format: None,
        };
        Self::build(params.name, v0, tail)
    }

    pub fn name(&self) -> PlistResult<String> {
        self.base.dict().required("BlueprintName")
    }
    pub fn set_name(&self, value: String) {
        self.base.dict().set_value("BlueprintName", value);
    }

    pub fn treat_missing_baselines_as_failures(&self) -> PlistResult<Option<bool>> {
        self.base.dict().optional("TreatMissingBaselinesAsFailures")
    }
    pub fn set_treat_missing_baselines_as_failures(&self, value: Option<bool>) {
        self.base.dict().set_nullable("TreatMissingBaselinesAsFailures", value);
    }

    pub fn ui_target_app_main_thread_checker_enabled(&self) -> PlistResult<Option<bool>> {
        self.base.dict().optional("UITargetAppMainThreadCheckerEnabled")
    }
    pub fn set_ui_target_app_main_thread_checker_enabled(&self, value: Option<bool>) {
        self.base
            .dict()
            .set_nullable("UITargetAppMainThreadCheckerEnabled", value);
    }

    pub fn gather_localizable_strings_data(&self) -> PlistResult<Option<bool>> {
        self.base.dict().optional("GatherLocalizableStringsData")
    }
    pub fn set_gather_localizable_strings_data(&self, value: Option<bool>) {
        self.base.dict().set_nullable("GatherLocalizableStringsData", value);
    }

    pub fn dependent_product_paths(&self) -> PlistResult<Vec<String>> {
        self.base.dict().optional_array("DependentProductPaths")
    }
    pub fn set_dependent_product_paths(&self, value: Vec<String>) {
        self.base.dict().set_array("DependentProductPaths", value);
    }

    pub fn product_module_name(&self) -> PlistResult<Option<String>> {
        self.base.dict().optional("ProductModuleName")
    }
    pub fn set_product_module_name(&self, value: Option<String>) {
        self.base.dict().set_nullable("ProductModuleName", value);
    }

    pub fn system_attachment_lifetime(&self) -> PlistResult<Option<String>> {
        self.base.dict().optional("SystemAttachmentLifetime")
    }
    pub fn set_system_attachment_lifetime(&self, value: Option<String>) {
        self.base.dict().set_nullable("SystemAttachmentLifetime", value);
    }

    pub fn user_attachment_lifetime(&self) -> PlistResult<Option<String>> {
        self.base.dict().optional("UserAttachmentLifetime")
    }
    pub fn set_user_attachment_lifetime(&self, value: Option<String>) {
        self.base.dict().set_nullable("UserAttachmentLifetime", value);
    }

    pub fn parallelization_enabled(&self) -> PlistResult<Option<bool>> {
        self.base.dict().optional("ParallelizationEnabled")
    }
    pub fn set_parallelization_enabled(&self, value: Option<bool>) {
        self.base.dict().set_nullable("ParallelizationEnabled", value);
    }

    pub fn test_execution_ordering(&self) -> PlistResult<Option<String>> {
        self.base.dict().optional("TestExecutionOrdering")
    }
    pub fn set_test_execution_ordering(&self, value: Option<String>) {
        self.base.dict().set_nullable("TestExecutionOrdering", value);
    }

    pub fn test_language(&self) -> PlistResult<Option<String>> {
        self.base.dict().optional("TestLanguage")
    }
    pub fn set_test_language(&self, value: Option<String>) {
        self.base.dict().set_nullable("TestLanguage", value);
    }

    pub fn test_region(&self) -> PlistResult<Option<String>> {
        self.base.dict().optional("TestRegion")
    }
    pub fn set_test_region(&self, value: Option<String>) {
        self.base.dict().set_nullable("TestRegion", value);
    }

    pub fn is_ui_test_bundle(&self) -> PlistResult<Option<bool>> {
        self.base.dict().optional("IsUITestBundle")
    }
    pub fn set_is_ui_test_bundle(&self, value: Option<bool>) {
        self.base.dict().set_nullable("IsUITestBundle", value);
    }

    pub fn is_app_hosted_test_bundle(&self) -> PlistResult<Option<bool>> {
        self.base.dict().optional("IsAppHostedTestBundle")
    }
    pub fn set_is_app_hosted_test_bundle(&self, value: Option<bool>) {
        self.base.dict().set_nullable("IsAppHostedTestBundle", value);
    }

    pub fn preferred_screen_capture_format(&self) -> PlistResult<Option<String>> {
        self.base.dict().optional("PreferredScreenCaptureFormat")
    }
    pub fn set_preferred_screen_capture_format(&self, value: Option<String>) {
        self.base.dict().set_nullable("PreferredScreenCaptureFormat", value);
    }
}

impl TestTarget {
    pub fn clang_profile_data_directory_path(&self) -> PlistResult<Option<String>> {
        self.base.dict().optional("ClangProfileDataDirectoryPath")
    }
    pub fn set_clang_profile_data_directory_path(&self, value: Option<String>) {
        self.base.dict().set_nullable("ClangProfileDataDirectoryPath", value);
    }
}

impl PropertyList for TestTarget {
    fn wrap(dict: PlistDict) -> Self {
        Self {
            base: V0TestTarget::wrap(dict),
        }
    }

    fn dict(&self) -> &PlistDict {
        self.base.dict()
    }
}
