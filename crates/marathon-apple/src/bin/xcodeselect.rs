//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcodeselect/Xcodeselect.kt

use super::{Tools, argv, kt};
use crate::model::Sdk;
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct Xcodeselect {
    tools: Tools,
}
impl Xcodeselect {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }
    pub async fn get_developer_path(&self, context: &VendorContext) -> VendorResult<String> {
        let result = self
            .tools
            .critical(argv(["xcode-select", "-p"]), self.tools.timeouts.shell, context)
            .await?;
        Ok(kt::trim(&result.combined_stdout()).to_owned())
    }
    /// The platform directory interpolates the enum *name* `IPHONESIMULATOR`, not `iphonesimulator` (A40).
    pub async fn get_swift5_fallback_libs_dir(&self, context: &VendorContext) -> VendorResult<Option<String>> {
        let swift_libs_path = format!(
            "{}/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.0",
            self.get_developer_path(context).await?
        );
        let platform_dir = format!("{swift_libs_path}/{}", Sdk::IphoneSimulator.name());
        let test = format!("'[ -d \"{platform_dir}\" ]'");
        let result = self
            .tools
            .safe(argv(["sh", "-c", &test]), self.tools.timeouts.shell, context)
            .await?;
        Ok(result.is_some_and(|result| result.successful()).then_some(platform_dir))
    }
}
