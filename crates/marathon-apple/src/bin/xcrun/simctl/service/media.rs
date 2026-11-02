//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/simctl/service/MediaService.kt

use super::SimctlService;
use crate::{bin::argv, cmd::CommandResult};
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct MediaService {
    service: SimctlService,
}
impl MediaService {
    pub fn new(service: SimctlService) -> Self {
        Self { service }
    }
    pub async fn add_media(
        &self,
        udid: &str,
        remote_path: &str,
        context: &VendorContext,
    ) -> VendorResult<CommandResult> {
        let timeout = self.service.timeouts().import_media;
        self.service
            .critical_exec(timeout, argv(["addmedia", udid, remote_path]), context)
            .await
    }
}
