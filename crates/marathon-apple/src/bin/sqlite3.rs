//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/sqlite3/Sqlite3.kt

use super::{Tools, argv, device_setup};
use crate::extensions::bash_escape;
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct Sqlite3 {
    tools: Tools,
}
impl Sqlite3 {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }
    /// `sudo` is accepted and ignored, as in Kotlin.
    pub async fn query(&self, path: &str, query: &str, sudo: bool, context: &VendorContext) -> VendorResult<()> {
        let _ = sudo;
        let mut command = argv(["sqlite3"]);
        command.push(bash_escape(path));
        command.push(bash_escape(query));
        self.tools
            .critical(command, self.tools.timeouts.shell, context)
            .await?
            .successful_or_null()
            .map(|_| ())
            .ok_or_else(|| device_setup(format!("failed to execute sqlite path=\"{path}\" query=\"{query}\"")))
    }
}
