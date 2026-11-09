//! Port of vendor/vendor-apple/macos/src/main/kotlin/com/malinskiy/marathon/apple/macos/MacosApplicationInstaller.kt

use crate::device::MacosDevice;
use marathon_apple::{
    apple_application_installer::AfterInstall,
    apple_device::AppleDevice,
    cmd::failure,
    model::AppleTestBundle,
    xctestrun::test_root_factory::{FactoryError, FactoryResult},
};
use marathon_core::{error::VendorError, vendor::VendorContext};
use marathon_error::FailureKind;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
};

/// Identity dispatch lets the shared remote parser use the same concrete permission hook as prepare.
#[derive(Default)]
pub struct MacosApplicationInstaller {
    devices: Mutex<Vec<Weak<MacosDevice>>>,
}
impl MacosApplicationInstaller {
    pub fn register(&self, device: &Arc<MacosDevice>) {
        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        devices.retain(|d| d.strong_count() > 0);
        devices.push(Arc::downgrade(device));
    }
    pub async fn grant_permissions(device: &MacosDevice, context: &VendorContext) -> FactoryResult<()> {
        let grants = &device.options.vendor.permissions.grant;
        if grants.is_empty() {
            return Ok(());
        }
        if device.binary_environment().csrutil.status(context).await? {
            return Err(VendorError::IncompatibleDevice(failure(
                FailureKind::IncompatibleDevice,
                "Unable to add permissions: requires System Integrity Protection to be disabled",
            ))
            .into());
        }
        for (client, permissions) in grants {
            device.grant(client, permissions, context).await?;
        }
        Ok(())
    }
}
fn missing() -> FactoryError {
    VendorError::Other(failure(
        FailureKind::IllegalState,
        "macOS installer requires its device",
    ))
    .into()
}
impl AfterInstall for MacosApplicationInstaller {
    fn after_install<'a>(
        &'a self,
        _: &'a AppleTestBundle,
        _: &'a VendorContext,
    ) -> Pin<Box<dyn Future<Output = FactoryResult<()>> + Send + 'a>> {
        Box::pin(async { Err(missing()) })
    }
    fn after_install_on<'a>(
        &'a self,
        device: &'a dyn AppleDevice,
        _: &'a AppleTestBundle,
        context: &'a VendorContext,
    ) -> Pin<Box<dyn Future<Output = FactoryResult<()>> + Send + 'a>> {
        let found = self
            .devices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(Weak::upgrade)
            .find(|d| std::ptr::addr_eq(Arc::as_ptr(d), device as *const dyn AppleDevice));
        Box::pin(async move {
            let device = found.ok_or_else(missing)?;
            Self::grant_permissions(&device, context).await
        })
    }
}
