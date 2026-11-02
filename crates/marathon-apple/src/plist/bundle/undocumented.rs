//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/plist/bundle/Undocumented.kt

use crate::plist::{PlistDict, PlistResult};

#[derive(Clone, Debug, PartialEq)]
pub struct Undocumented {
    dict: PlistDict,
}

impl Undocumented {
    pub fn wrap(dict: PlistDict) -> Self {
        Self { dict }
    }

    /// The optional-array flavour: a bundle without the key yields an empty array and then
    /// fails 4.5b's platform check as incompatible; it does not fail the plist read.
    pub fn bundle_supported_platforms(&self) -> PlistResult<Vec<String>> {
        self.dict.optional_array("CFBundleSupportedPlatforms")
    }

    pub fn build_machine_os_build(&self) -> PlistResult<String> {
        self.dict.required("BuildMachineOSBuild")
    }

    pub fn contains_ui_tests(&self) -> PlistResult<bool> {
        self.dict.required("XCTContainsUITests")
    }

    pub fn dt_compiler(&self) -> PlistResult<String> {
        self.dict.required("DTCompiler")
    }

    pub fn dt_platform_build(&self) -> PlistResult<String> {
        self.dict.required("DTPlatformBuild")
    }

    pub fn dt_platform_name(&self) -> PlistResult<String> {
        self.dict.required("DTPlatformName")
    }

    pub fn dt_platform_version(&self) -> PlistResult<String> {
        self.dict.required("DTPlatformVersion")
    }

    pub fn dt_sdk_build(&self) -> PlistResult<String> {
        self.dict.required("DTSDKBuild")
    }

    pub fn dt_sdk_name(&self) -> PlistResult<String> {
        self.dict.required("DTSDKName")
    }

    pub fn dt_xcode(&self) -> PlistResult<String> {
        self.dict.required("DTXcode")
    }

    pub fn dt_xcode_build(&self) -> PlistResult<String> {
        self.dict.required("DTXcodeBuild")
    }

    pub fn ui_device_family(&self) -> PlistResult<Vec<i32>> {
        self.dict.optional_array("UIDeviceFamily")
    }
}
