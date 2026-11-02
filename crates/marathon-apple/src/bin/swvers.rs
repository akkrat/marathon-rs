//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/swvers/SwVers.kt

use super::{Tools, argv, device_setup, kt};
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct SwVers {
    tools: Tools,
}
impl SwVers {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }
    pub async fn get_version(&self, context: &VendorContext) -> VendorResult<String> {
        self.tools
            .critical(
                argv(["sw_vers", "--productVersion"]),
                self.tools.timeouts.shell,
                context,
            )
            .await?
            .successful_or_null()
            .map(|result| kt::trim(&result.combined_stdout()).to_owned())
            .ok_or_else(|| device_setup("failed to detect operating system version"))
    }
}
