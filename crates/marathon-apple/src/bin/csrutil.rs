//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/csrutil/Csrutil.kt

use super::{Tools, argv, device_setup};
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct Csrutil {
    tools: Tools,
}
impl Csrutil {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }
    /// System Integrity Protection status: true unless some stdout *line* is exactly `disabled` (A18).
    pub async fn status(&self, context: &VendorContext) -> VendorResult<bool> {
        let stdout = self
            .tools
            .critical(argv(["csrutil", "status"]), self.tools.timeouts.shell, context)
            .await?
            .successful_or_null()
            .map(|result| result.stdout)
            .ok_or_else(|| device_setup("failed to detect SIP status"))?;
        Ok(!stdout.iter().any(|line| line == "disabled"))
    }
}
