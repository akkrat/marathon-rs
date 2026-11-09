//! Rust-only bridge replacing the `device as? AppleDevice` casts in
//! vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/NmTestParser.kt and XCTestParser.kt
//! (WP 4.10a decision 3): Apple devices are looked up by identity instead of downcast.

use crate::{apple_device::AppleDevice, cmd::failure};
use marathon_core::{
    error::{VendorError, VendorResult},
    vendor::DeviceRef,
};
use marathon_error::FailureKind;
use std::sync::{Arc, Mutex};

type Entry = (DeviceRef, Arc<dyn AppleDevice>);

/// Per-vendor identity lookup. Providers register on connect and remove on
/// dispose; parsers use it off core. A serial reused by a new device is distinct.
#[derive(Default)]
pub struct AppleDeviceRegistry {
    entries: Mutex<Vec<Entry>>,
}
impl AppleDeviceRegistry {
    pub fn register(&self, device: Arc<dyn AppleDevice>) -> DeviceRef {
        let identity = DeviceRef(device.clone());
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if !entries.iter().any(|(key, _)| *key == identity) {
            entries.push((identity.clone(), device));
        }
        identity
    }
    pub fn remove(&self, device: &DeviceRef) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(key, _)| key != device);
    }
    pub fn lookup(&self, device: &DeviceRef) -> VendorResult<Arc<dyn AppleDevice>> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(key, _)| key == device)
            .map(|(_, apple)| apple.clone())
            .ok_or_else(|| {
                VendorError::Other(failure(
                    FailureKind::Config(marathon_error::ConfigKind::Rejected),
                    "Unexpected device type for remote test parsing",
                ))
            })
    }
}
