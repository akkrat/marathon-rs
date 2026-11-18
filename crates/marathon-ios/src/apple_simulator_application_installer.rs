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
    /// D87: whether the deadline refused a retry of a transfer.
    refused: std::sync::atomic::AtomicBool,
    /// D87: whether the installation may start its next step.
    may_continue: &'a (dyn Fn() -> bool + Send + Sync),
}
fn always() -> bool {
    true
}
impl<'a> AppleSimulatorApplicationInstaller<'a> {
    pub fn new(device: &'a AppleSimulatorDevice) -> Self {
        Self {
            device,
            refused: std::sync::atomic::AtomicBool::new(false),
            may_continue: &always,
        }
    }
    /// D87: the installation starts each later step only while `may_continue` says so.
    pub fn with_step_gate(mut self, may_continue: &'a (dyn Fn() -> bool + Send + Sync)) -> Self {
        self.may_continue = may_continue;
        self
    }
    /// D87: whether the installation's failure is one whose retry the preparation's deadline refused to start.
    pub fn refused(&self) -> bool {
        self.refused.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub async fn prepare_installation(&self, context: &VendorContext) -> VendorResult<()> {
        let services = self.device.services();
        let config = VendorConfiguration::Ios((*services.vendor).clone());
        // D87: a failed transfer is sent again only while the preparation goes on (the other half has not run out of time)
        // and before its deadline: one test for both.
        let may_retry = || {
            let may = (self.may_continue)() && self.device.may_start_attempt(0);
            if !may {
                self.refused.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            may
        };
        AppleApplicationInstaller::new(&config, services.runtime.clone(), services.logging.clone())
            .with_code_coverage(services.configuration.is_code_coverage_enabled)
            .with_after_install(self)
            .with_retry_gate(&may_retry)
            .with_step_gate(self.may_continue)
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
