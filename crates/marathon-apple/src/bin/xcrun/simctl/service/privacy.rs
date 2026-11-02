//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/simctl/service/PrivacyService.kt

use super::SimctlService;
use crate::{bin::argv, cmd::CommandResult};
use marathon_config::vendor::IosPermission;
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct PrivacyService {
    service: SimctlService,
}
impl PrivacyService {
    pub fn new(service: SimctlService) -> Self {
        Self { service }
    }
    pub async fn grant(
        &self,
        udid: &str,
        service: IosPermission,
        bundle_id: &str,
        context: &VendorContext,
    ) -> VendorResult<CommandResult> {
        self.exec(argv(["privacy", udid, "grant", service.value(), bundle_id]), context)
            .await
    }
    pub async fn revoke(
        &self,
        udid: &str,
        service: IosPermission,
        bundle_id: &str,
        context: &VendorContext,
    ) -> VendorResult<CommandResult> {
        self.exec(argv(["privacy", udid, "revoke", service.value(), bundle_id]), context)
            .await
    }
    pub async fn reset(
        &self,
        udid: &str,
        service: IosPermission,
        bundle_id: &str,
        context: &VendorContext,
    ) -> VendorResult<CommandResult> {
        self.exec(argv(["privacy", udid, "reset", service.value(), bundle_id]), context)
            .await
    }
    pub async fn reset_all(
        &self,
        udid: &str,
        service: IosPermission,
        context: &VendorContext,
    ) -> VendorResult<CommandResult> {
        self.exec(argv(["privacy", udid, "reset", service.value()]), context)
            .await
    }
    async fn exec(&self, args: Vec<String>, context: &VendorContext) -> VendorResult<CommandResult> {
        let timeout = self.service.timeouts().shell;
        self.service.critical_exec(timeout, args, context).await
    }
}
