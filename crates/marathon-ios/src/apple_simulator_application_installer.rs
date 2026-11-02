//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorApplicationInstaller.kt

use crate::apple_simulator_device::AppleSimulatorDevice;
use marathon_apple::{
    apple_application_installer::{AfterInstall, AppleApplicationInstaller},
    model::AppleTestBundle,
    xctestrun::test_root_factory::FactoryResult,
};
use marathon_config::{VendorConfiguration, vendor::GrantLifecycle};
use marathon_core::{error::VendorResult, vendor::VendorContext};
use std::{future::Future, pin::Pin};

pub struct AppleSimulatorApplicationInstaller<'a> {
    device: &'a AppleSimulatorDevice,
}
impl<'a> AppleSimulatorApplicationInstaller<'a> {
    pub fn new(device: &'a AppleSimulatorDevice) -> Self {
        Self { device }
    }
    pub async fn prepare_installation(&self, context: &VendorContext) -> VendorResult<()> {
        let services = self.device.services();
        let config = VendorConfiguration::Ios((*services.vendor).clone());
        AppleApplicationInstaller::new(&config, services.runtime.clone(), services.logging.clone())
            .with_code_coverage(services.configuration.is_code_coverage_enabled)
            .with_after_install(self)
            .prepare_installation(self.device, false, context)
            .await
            .map_err(|e| e.into_vendor())
    }
}
impl AfterInstall for AppleSimulatorApplicationInstaller<'_> {
    fn after_install<'a>(
        &'a self,
        bundle: &'a AppleTestBundle,
        context: &'a VendorContext,
    ) -> Pin<Box<dyn Future<Output = FactoryResult<()>> + Send + 'a>> {
        Box::pin(async move {
            let permissions = &self.device.services().vendor.permissions;
            if permissions.lifecycle == GrantLifecycle::BeforeTestRun {
                for permission in &permissions.grant {
                    self.device.grant(*permission, bundle.app_id(), context).await?;
                }
            }
            Ok(())
        })
    }
}
