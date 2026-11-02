//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/Xcrun.kt

pub mod simctl;
pub mod xcodebuild;
pub mod xcresulttool;

use super::{Tools, argv, kt};
use crate::model::Sdk;
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct Xcrun {
    tools: Tools,
    pub simctl: simctl::Simctl,
    pub xcodebuild: xcodebuild::Xcodebuild,
    pub xcresulttool: xcresulttool::Xcresulttool,
}
impl Xcrun {
    pub fn new(tools: Tools, test_batch_timeout_millis: i64, test_output_timeout_millis: i64) -> Self {
        Self {
            simctl: simctl::Simctl::new(tools.clone()),
            xcodebuild: xcodebuild::Xcodebuild::new(
                tools.clone(),
                test_batch_timeout_millis,
                test_output_timeout_millis,
            ),
            xcresulttool: xcresulttool::Xcresulttool::new(tools.clone()),
            tools,
        }
    }
    pub async fn get_sdk_platform_path(&self, sdk: Sdk, context: &VendorContext) -> VendorResult<String> {
        self.show(sdk, "--show-sdk-platform-path", context).await
    }
    pub async fn get_sdk_version(&self, sdk: Sdk, context: &VendorContext) -> VendorResult<String> {
        self.show(sdk, "--show-sdk-version", context).await
    }
    /// Kotlin `getXctesttPath`: the xctest tool under the SDK platform.
    pub async fn get_xctest_path(&self, sdk: Sdk, context: &VendorContext) -> VendorResult<String> {
        Ok(format!(
            "{}/Developer/Library/Xcode/Agents/xctest",
            self.get_sdk_platform_path(sdk, context).await?
        ))
    }
    async fn show(&self, sdk: Sdk, flag: &str, context: &VendorContext) -> VendorResult<String> {
        let result = self
            .tools
            .critical(
                argv(["xcrun", "--sdk", sdk.value(), flag]),
                self.tools.timeouts.shell,
                context,
            )
            .await?;
        Ok(kt::trim(&result.combined_stdout()).to_owned())
    }
}
