//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/AppleLogConfigurator.kt
mod support;
use jcompat::logging::{Layout, Level, MarathonLogging};
use marathon_apple::apple_log_configurator::AppleLogConfigurator;
use marathon_core::{error::VendorResult, vendor::LogConfigurator};
use std::sync::{Arc, Mutex};
#[tokio::test]
async fn configures_debug_before_appenders_and_preserves_the_six_noise_levels() -> VendorResult<()> {
    for debug in [false, true] {
        for compact_output in [false, true] {
            let registry = Arc::new(Mutex::new(MarathonLogging::default()));
            {
                registry.lock().unwrap().debug = !debug;
            }
            let configurator = AppleLogConfigurator {
                compact_output,
                runtime: Arc::new(support::Runtime),
            };
            configurator.configure(debug, registry.clone()).await?;
            let mut logging = registry.lock().unwrap();
            assert_eq!(logging.debug, debug);
            assert_eq!(
                logging.layout(),
                if compact_output {
                    Layout::Compact
                } else {
                    Layout::Runtime
                }
            );
            for name in [
                "TimelineSummaryProvider",
                "net.schmizz.sshj.transport.kex.Curve25519SHA256",
                "net.schmizz.sshj.transport.random.BouncyCastleRandom",
                "net.schmizz.sshj.DefaultConfig",
                "net.schmizz.sshj.common.KeyType",
                "net.schmizz.sshj.common.ECDSAVariationsAdapter",
            ] {
                assert_eq!(logging.effective_level(name), Level::Error);
            }
            let logger = logging.logger("after", None);
            assert_eq!(logging.enabled(&logger, Level::Debug), debug);
        }
    }
    Ok(())
}
