//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/AppleLogConfigurator.kt
mod support;
use marathon_apple::apple_log_configurator::AppleLogConfigurator;
use marathon_core::{error::VendorResult, vendor::LogConfigurator};
use marathon_logging::{Level, LogSettings, Vendor};
use std::sync::{Arc, Mutex};
#[tokio::test]
async fn configures_debug_before_appenders_and_preserves_the_six_noise_levels() -> VendorResult<()> {
    for debug in [false, true] {
        for compact_output in [false, true] {
            let registry = Arc::new(Mutex::new(LogSettings::default()));
            {
                registry.lock().unwrap().debug = !debug;
            }
            let configurator = AppleLogConfigurator {
                compact_output,
                runtime: Arc::new(support::Runtime),
            };
            configurator.configure(debug, registry.clone()).await?;
            let logging = registry.lock().unwrap();
            assert_eq!(logging.debug, debug);
            assert_eq!(logging.vendor(), Vendor::Apple { compact_output });
            assert!(!logging.enabled_target("marathon_core::reporters::timeline", Level::WARN));
            assert!(logging.enabled_target("marathon_core::reporters::timeline", Level::ERROR));
            let logger = logging.component("marathon_apple::apple_log_configurator");
            assert_eq!(logging.enabled(&logger, Level::DEBUG), debug);
        }
    }
    Ok(())
}
