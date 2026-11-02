//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/getconf/Getconf.kt

use super::{Tools, argv, kt};
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct Getconf {
    tools: Tools,
}
impl Getconf {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }
    pub async fn get_darwin_user_cache_dir(&self, context: &VendorContext) -> VendorResult<String> {
        self.get("DARWIN_USER_CACHE_DIR", context).await
    }
    pub async fn get(&self, key: &str, context: &VendorContext) -> VendorResult<String> {
        let result = self
            .tools
            .critical(argv(["getconf", key]), self.tools.timeouts.shell, context)
            .await?;
        Ok(kt::trim(&result.combined_stdout()).to_owned())
    }
}
