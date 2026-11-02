//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/plistbuddy/PlistBuddy.kt

use super::{Tools, argv};
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct PlistBuddy {
    tools: Tools,
}
impl PlistBuddy {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }
    pub async fn set(&self, file: &str, path: &str, value: &str, context: &VendorContext) -> VendorResult<bool> {
        self.exec(format!("'Set {path} {value}'"), file, context).await
    }
    pub async fn add(
        &self,
        file: &str,
        path: &str,
        kind: &str,
        value: &str,
        context: &VendorContext,
    ) -> VendorResult<bool> {
        self.exec(format!("'Add {path} {kind} {value}'"), file, context).await
    }
    /// Kotlin names this `criticalExec`, but it is a safe execution.
    async fn exec(&self, command: String, file: &str, context: &VendorContext) -> VendorResult<bool> {
        let result = self
            .tools
            .safe(
                argv(["/usr/libexec/PlistBuddy", "-c", &command, file]),
                self.tools.timeouts.shell,
                context,
            )
            .await?;
        Ok(result.is_some_and(|result| result.successful()))
    }
}
