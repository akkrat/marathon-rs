//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/chmod/Chmod.kt

use super::{Tools, argv, device_setup, kt};
use crate::extensions::bash_escape;
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct Chmod {
    tools: Tools,
}
impl Chmod {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }
    pub async fn make_executable(&self, path: &str, context: &VendorContext) -> VendorResult<String> {
        let mut command = argv(["chmod", "+x"]);
        command.push(bash_escape(path));
        // The fallback is unreachable after criticalExecute, and its text is copied from SwVers in Kotlin.
        self.tools
            .critical(command, self.tools.timeouts.shell, context)
            .await?
            .successful_or_null()
            .map(|result| kt::trim(&result.combined_stdout()).to_owned())
            .ok_or_else(|| device_setup("failed to detect operating system version"))
    }
}
