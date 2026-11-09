//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/AppleLogConfigurator.kt

use crate::cmd::blocking;
use marathon_logging::{LogSettings, Vendor};
use marathon_core::{
    runtime::{VendorFuture, VendorRuntime},
    vendor::LogConfigurator,
};
use std::sync::{Arc, Mutex};

pub struct AppleLogConfigurator {
    pub compact_output: bool,
    pub runtime: Arc<dyn VendorRuntime>,
}
impl LogConfigurator for AppleLogConfigurator {
    fn configure(&self, debug: bool, logging: Arc<Mutex<LogSettings>>) -> VendorFuture<'_, ()> {
        let compact_output = self.compact_output;
        Box::pin(async move {
            blocking(self.runtime.as_ref(), move || {
                let mut registry = logging.lock().unwrap_or_else(|e| e.into_inner());
                registry.debug = debug;
                registry.configure(debug, Vendor::Apple { compact_output });
                Ok(())
            })
            .await
        })
    }
}
