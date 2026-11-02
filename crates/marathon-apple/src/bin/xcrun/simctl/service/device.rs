//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/simctl/service/DeviceService.kt

use super::SimctlService;
use crate::{
    bin::{argv, kt::list_to_string, xcrun::simctl::model},
    cmd::blocking,
    device::connection_factory::{ConnectionError, ConnectionResult},
    logparser::parser::{DeviceFailureException, DeviceFailureReason},
};
use jcompat::logging::Logger;
use marathon_core::vendor::VendorContext;

pub struct DeviceService {
    service: SimctlService,
    _logger: Logger,
}
impl DeviceService {
    pub fn new(service: SimctlService) -> Self {
        // Declared and never used in Kotlin; the lookup still registers the name.
        let _logger = service
            .tools
            .logger("com.malinskiy.marathon.apple.bin.xcrun.simctl.service.DeviceService");
        Self { service, _logger }
    }
    /// Command failures are `ConnectionError::Vendor`; ServicesUnavailable is `ConnectionError::Device`.
    pub async fn list(&self, context: &VendorContext) -> ConnectionResult<model::SimctlListDevicesOutput> {
        let timeout = self.service.timeouts().shell;
        let result = self
            .service
            .critical_exec(timeout, argv(["list", "--json"]), context)
            .await?;
        let host = &self.service.tools.executor.host().id;
        let unavailable = |message| {
            ConnectionError::Device(DeviceFailureException::new(
                DeviceFailureReason::ServicesUnavailable,
                Some(message),
                None,
            ))
        };
        // Unreachable after criticalExecute, kept from Kotlin.
        if !result.successful() {
            return Err(unavailable(format!(
                "simctl list errored on {host}: stdout={}, stderr={}, exitCode={}",
                list_to_string(&result.stdout),
                list_to_string(&result.stderr),
                result.exit_code.map_or_else(|| "null".into(), |code| code.to_string())
            )));
        }
        let json = result.combined_stdout();
        let decoded = blocking(&*self.service.tools.runtime, move || {
            Ok(model::decode_list_devices_output(&json))
        })
        .await?;
        decoded.map_err(|_| {
            unavailable(format!(
                "Error parsing simctl output on {host}: {}",
                list_to_string(&result.stdout)
            ))
        })
    }
    pub async fn list_devices(&self, context: &VendorContext) -> ConnectionResult<Vec<model::SimctlDevice>> {
        Ok(self.list(context).await?.devices.devices)
    }
}
