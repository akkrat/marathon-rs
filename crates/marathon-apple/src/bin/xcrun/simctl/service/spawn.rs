//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/simctl/service/SpawnService.kt

use super::SimctlService;
use crate::{bin::argv, cmd::CommandResult};
use jcompat::jtime::Duration;
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct SpawnService {
    service: SimctlService,
}
impl SpawnService {
    pub fn new(service: SimctlService) -> Self {
        Self { service }
    }
    /// `timeout` None is Kotlin's default, the shell timeout.
    pub async fn spawn(
        &self,
        udid: &str,
        args: &[String],
        timeout: Option<Duration>,
        context: &VendorContext,
    ) -> VendorResult<CommandResult> {
        let timeout = timeout.unwrap_or(self.service.timeouts().shell);
        let mut command = argv(["spawn", udid]);
        command.extend_from_slice(args);
        self.service.critical_exec(timeout, command, context).await
    }
}
