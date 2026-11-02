//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/ioreg/Ioreg.kt

use super::{Tools, argv, device_setup, kt};
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct Ioreg {
    tools: Tools,
}
impl Ioreg {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }
    pub async fn get_udid(&self, context: &VendorContext) -> VendorResult<String> {
        self.get_param("IOPlatformUUID", context).await
    }
    pub async fn get_manufacturer(&self, context: &VendorContext) -> VendorResult<String> {
        self.get_param("manufacturer", context).await
    }
    pub async fn get_model(&self, context: &VendorContext) -> VendorResult<String> {
        self.get_param("model", context).await
    }
    async fn get_param(&self, name: &str, context: &VendorContext) -> VendorResult<String> {
        // The outer quotes are literal: the SSH executor passes a quoted argument through unquoted.
        let script =
            format!("'ioreg -ad2 -c IOPlatformExpertDevice | plutil -extract IORegistryEntryChildren.0.{name} raw -'");
        self.tools
            .critical(argv(["sh", "-c", &script]), self.tools.timeouts.shell, context)
            .await?
            .successful_or_null()
            .map(|result| kt::trim(&result.combined_stdout()).to_owned())
            .ok_or_else(|| device_setup("failed to detect UDID"))
    }
}
