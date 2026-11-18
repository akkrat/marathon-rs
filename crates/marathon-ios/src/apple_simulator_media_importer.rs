//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorMediaImporter.kt

use crate::apple_simulator_device::AppleSimulatorDevice;
use marathon_apple::apple_device::AppleDevice;
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct AppleSimulatorMediaImporter<'a> {
    device: &'a AppleSimulatorDevice,
    logger: marathon_logging::Component,
    /// D87: whether the next upload or import may start.
    may_continue: &'a (dyn Fn() -> bool + Send + Sync),
}
fn always() -> bool {
    true
}
impl<'a> AppleSimulatorMediaImporter<'a> {
    pub fn new(device: &'a AppleSimulatorDevice) -> Self {
        let logging = &device.services().logging;
        let _ = logging.component("marathon_ios::apple_simulator_media_importer");
        let logger = logging.component("marathon_ios::apple_simulator_media_importer");
        Self {
            device,
            logger,
            may_continue: &always,
        }
    }
    /// D87: each upload and import starts only while `may_continue` says so; the import then stops where it is, without
    /// an error of its own, for the preparation is already failing.
    pub fn with_step_gate(mut self, may_continue: &'a (dyn Fn() -> bool + Send + Sync)) -> Self {
        self.may_continue = may_continue;
        self
    }
    pub async fn import_media(&self, context: &VendorContext) -> VendorResult<()> {
        let services = self.device.services();
        let Some(files) = services.vendor.media_files.as_ref().filter(|v| !v.is_empty()) else {
            return Ok(());
        };
        let remote = self.device.remote_file_manager();
        remote.create_remote_shared_media_directory(context).await?;
        for file in files {
            if !(self.may_continue)() {
                return Ok(());
            }
            let local = file;
            let dst = remote.remote_media_file(local.file_name().and_then(|s| s.to_str()).unwrap_or(""));
            if !self.device.push_shared(local, &dst, context).await? {
                return Err(self
                    .device
                    .setup_failure(format!("Error transferring {dst} to {}", self.device.serial())));
            }
            for attempt in 0..3 {
                if !(self.may_continue)() {
                    return Ok(());
                }
                if services.logging.enabled(&self.logger, marathon_logging::Level::DEBUG) {
                    (services.logging.sink)(
                        &self.logger,
                        marathon_logging::Level::DEBUG,
                        format!(
                            "Importing media {} to {}",
                            file.to_string_lossy().into_owned(),
                            self.device.serial()
                        ),
                        None,
                    );
                }
                let outcome = self.device.import_media(&dst, context).await.and_then(|ok| {
                    if ok {
                        Ok(())
                    } else {
                        Err(self
                            .device
                            .setup_failure(format!("Error importing media {dst} to {}", self.device.serial())))
                    }
                });
                match outcome {
                    Ok(()) => break,
                    Err(e @ marathon_core::error::VendorError::Cancelled(_)) => return Err(e),
                    Err(e) if attempt == 2 => return Err(e),
                    Err(_) => context.clock.delay(1_000, context.cancellation.clone()).await?,
                }
            }
        }
        Ok(())
    }
}
