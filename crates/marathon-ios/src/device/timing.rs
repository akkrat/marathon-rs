//! Port of core/src/main/kotlin/com/malinskiy/marathon/log/TimingLog.kt
//! and vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt:474–476,518–532.

use marathon_logging::{Level, Component};

use super::error::clock_error;
use jcompat::{
    
    writers::KotlinValue,
};
use marathon_apple::cmd::remote::ssh::sshj::config::SshLogging;
use marathon_config::devices::{SimulatorProfile, Transport};
use marathon_core::{error::VendorResult, time::Timer};
use std::sync::Arc;

#[derive(Clone)]
pub struct ProvisioningTiming {
    pub timer: Arc<dyn Timer>,
    logging: SshLogging,
    logger: Component,
}
impl ProvisioningTiming {
    pub fn new(timer: Arc<dyn Timer>, logging: SshLogging) -> Self {
        let logger = logging.registry.lock().unwrap_or_else(|e| e.into_inner()).timings();
        Self { timer, logging, logger }
    }
    pub fn elapsed(&self) -> VendorResult<i64> {
        self.timer.elapsed_time_millis().map_err(clock_error)
    }
    pub fn info(&self, message: impl FnOnce() -> VendorResult<String>) -> VendorResult<()> {
        if self.logging.enabled(&self.logger, Level::INFO) {
            let message = message()?;
            // TimingLog evaluates the lazy message before its independent elapsed read.
            let elapsed = self.elapsed()?;
            let text = self
                .logging
                .registry
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .timing_message(|| Some(elapsed), || message);
            if let Some(text) = text {
                (self.logging.sink)(&self.logger, Level::INFO, text, None);
            }
        }
        Ok(())
    }
    /// Provider calls this at its actual cycle boundary (5.5a); timestamps are wall Timer reads.
    pub fn cycle(
        &self,
        worker: &Transport,
        connected: i64,
        planned: i64,
        count: usize,
        started_provisioning: i64,
    ) -> VendorResult<()> {
        self.info(|| Ok(format!("Worker {}: connected in {connected} ms, planned {count} simulators in {planned} ms, provisioned them in {} ms", worker.id().to_owned(), self.elapsed()?.wrapping_sub(started_provisioning))))
    }
}
fn data(name: &str, fields: Vec<(&str, KotlinValue)>) -> KotlinValue {
    KotlinValue::DataClass {
        name: name.into(),
        fields: fields.into_iter().map(|(k, v)| (k.into(), v)).collect(),
    }
}
pub fn profile_text(profile: &SimulatorProfile) -> String {
    data(
        "SimulatorProfile",
        vec![
            ("deviceTypeId", KotlinValue::Text(profile.device_type_id.clone())),
            (
                "runtimeId",
                profile.runtime_id.clone().map_or(KotlinValue::Null, KotlinValue::Text),
            ),
            ("newNamePrefix", KotlinValue::Text(profile.new_name_prefix.clone())),
        ],
    )
    .render()
    .to_owned()
}
pub use marathon_apple::device::connection_factory::transport_text;
