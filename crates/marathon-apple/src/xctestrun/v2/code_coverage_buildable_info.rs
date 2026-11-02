//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/xctestrun/v2/CodeCoverageBuildableInfo.kt

use crate::plist::{PlistDict, PlistResult, PropertyList};

/// The constructor arguments, in constructor (i.e. writer) order.
#[derive(Clone, Debug, PartialEq)]
pub struct CodeCoverageBuildableInfoParams {
    pub name: String,
    pub buildable_identifier: String,
    pub include_in_report: bool,
    pub is_static: bool,
    pub product_paths: Vec<String>,
    pub architectures: Vec<String>,
    pub source_files: Vec<String>,
    pub source_files_common_path_prefix: String,
    pub toolchains: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CodeCoverageBuildableInfo {
    dict: PlistDict,
}

impl CodeCoverageBuildableInfo {
    pub fn with_params(params: CodeCoverageBuildableInfoParams) -> Self {
        let info = Self::wrap(PlistDict::new());
        info.set_name(params.name);
        info.set_buildable_identifier(params.buildable_identifier);
        info.set_include_in_report(params.include_in_report);
        info.set_is_static(params.is_static);
        info.set_product_paths(params.product_paths);
        info.set_architectures(params.architectures);
        info.set_source_files(params.source_files);
        info.set_source_files_common_path_prefix(params.source_files_common_path_prefix);
        info.set_toolchains(params.toolchains);
        info
    }

    pub fn name(&self) -> PlistResult<String> {
        self.dict.required("Name")
    }
    pub fn set_name(&self, value: String) {
        self.dict.set_value("Name", value);
    }

    pub fn buildable_identifier(&self) -> PlistResult<String> {
        self.dict.required("BuildableIdentifier")
    }
    pub fn set_buildable_identifier(&self, value: String) {
        self.dict.set_value("BuildableIdentifier", value);
    }

    pub fn include_in_report(&self) -> PlistResult<bool> {
        self.dict.required("IncludeInReport")
    }
    pub fn set_include_in_report(&self, value: bool) {
        self.dict.set_value("IncludeInReport", value);
    }

    pub fn is_static(&self) -> PlistResult<bool> {
        self.dict.required("IsStatic")
    }
    pub fn set_is_static(&self, value: bool) {
        self.dict.set_value("IsStatic", value);
    }

    pub fn product_paths(&self) -> PlistResult<Vec<String>> {
        self.dict.optional_array("ProductPaths")
    }
    pub fn set_product_paths(&self, value: Vec<String>) {
        self.dict.set_array("ProductPaths", value);
    }

    pub fn architectures(&self) -> PlistResult<Vec<String>> {
        self.dict.optional_array("Architectures")
    }
    pub fn set_architectures(&self, value: Vec<String>) {
        self.dict.set_array("Architectures", value);
    }

    pub fn source_files(&self) -> PlistResult<Vec<String>> {
        self.dict.optional_array("SourceFiles")
    }
    pub fn set_source_files(&self, value: Vec<String>) {
        self.dict.set_array("SourceFiles", value);
    }

    pub fn source_files_common_path_prefix(&self) -> PlistResult<String> {
        self.dict.required("SourceFilesCommonPathPrefix")
    }
    pub fn set_source_files_common_path_prefix(&self, value: String) {
        self.dict.set_value("SourceFilesCommonPathPrefix", value);
    }

    pub fn toolchains(&self) -> PlistResult<Vec<String>> {
        self.dict.optional_array("Toolchains")
    }
    pub fn set_toolchains(&self, value: Vec<String>) {
        self.dict.set_array("Toolchains", value);
    }
}

impl PropertyList for CodeCoverageBuildableInfo {
    fn wrap(dict: PlistDict) -> Self {
        Self { dict }
    }

    fn dict(&self) -> &PlistDict {
        &self.dict
    }
}
