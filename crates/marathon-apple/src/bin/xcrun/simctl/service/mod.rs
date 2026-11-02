//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/simctl/service/ApplicationService.kt
//! (the other six services are in the sibling modules).

mod device;
mod io;
mod media;
mod privacy;
mod simulator;
mod spawn;

pub use device::DeviceService;
pub use io::IoService;
pub use media::MediaService;
pub use privacy::PrivacyService;
pub use simulator::SimulatorService;
pub use spawn::SpawnService;

use super::SimctlService;
use crate::{bin::argv, cmd::CommandResult};
use marathon_core::{error::VendorResult, vendor::VendorContext};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerType {
    Application,
    Data,
    Groups,
}
impl ContainerType {
    pub fn value(self) -> &'static str {
        match self {
            Self::Application => "app",
            Self::Data => "data",
            Self::Groups => "groups",
        }
    }
}

pub struct ApplicationService {
    service: SimctlService,
}
impl ApplicationService {
    pub fn new(service: SimctlService) -> Self {
        Self { service }
    }
    pub async fn install(&self, udid: &str, remote_path: &str, context: &VendorContext) -> VendorResult<CommandResult> {
        let timeout = self.service.timeouts().install;
        self.service
            .critical_exec(timeout, argv(["install", udid, remote_path]), context)
            .await
    }
    pub async fn container_path(
        &self,
        udid: &str,
        bundle_id: &str,
        container_type: ContainerType,
        context: &VendorContext,
    ) -> VendorResult<CommandResult> {
        let timeout = self.service.timeouts().shell;
        let args = argv(["get_app_container", udid, bundle_id, container_type.value()]);
        self.service.critical_exec(timeout, args, context).await
    }
    pub async fn terminate_application(
        &self,
        udid: &str,
        bundle_id: &str,
        context: &VendorContext,
    ) -> VendorResult<Option<CommandResult>> {
        let timeout = self.service.timeouts().shell;
        self.service
            .safe_execute(timeout, argv(["terminate", udid, bundle_id]), context)
            .await
    }
    pub async fn uninstall_application(
        &self,
        udid: &str,
        bundle_id: &str,
        context: &VendorContext,
    ) -> VendorResult<CommandResult> {
        let timeout = self.service.timeouts().uninstall;
        self.service
            .critical_exec(timeout, argv(["uninstall", udid, bundle_id]), context)
            .await
    }
}
