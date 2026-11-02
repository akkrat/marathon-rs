//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/systemprofiler/SystemProfiler.kt

use super::{Tools, argv, kt};
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct SystemProfiler {
    tools: Tools,
}
impl SystemProfiler {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }
    pub async fn get_provisioning_udid(&self, context: &VendorContext) -> VendorResult<String> {
        let stdout = self
            .tools
            .critical(
                argv(["sh", "-c", "'system_profiler SPHardwareDataType'"]),
                self.tools.timeouts.shell,
                context,
            )
            .await?
            .successful_or_null()
            .map(|result| kt::trim(&result.combined_stdout()).to_owned());
        Ok(stdout
            .as_deref()
            .and_then(|stdout| {
                kt::lines(stdout)
                    .into_iter()
                    .find(|line| line.contains("Provisioning UDID"))
            })
            .and_then(|line| line.split(':').nth(1))
            .map(|value| kt::trim(value).to_owned())
            .unwrap_or_default())
    }
}
