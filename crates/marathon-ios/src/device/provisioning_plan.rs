//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/ProvisioningPlan.kt
//! and AppleSimulatorProvider.kt:683–774 (shared planning/shutdown helper; provider owns final live eligibility).

use super::{
    error::ProvisioningResult,
    timing::{ProvisioningTiming, transport_text},
    worker_provisioning_context::WorkerProvisioningContext,
};
use indexmap::{IndexMap, IndexSet};
use marathon_logging::{Level, Component};
use marathon_apple::{
    bin::{AppleBinaryEnvironment, xcrun::simctl::model::SimctlDeviceState},
    cmd::remote::ssh::sshj::config::SshLogging,
};
use marathon_config::devices::{AppleTarget, SimulatorProfile, Transport};
use marathon_core::vendor::VendorContext;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProvisioningPlan {
    pub existing_simulators: IndexSet<String>,
    pub needs_provisioning: Vec<SimulatorProfile>,
    pub physical_devices: IndexSet<String>,
    pub reused_profiles: IndexMap<String, SimulatorProfile>,
}
/// Invoked after shutdown awaits. The provider must re-read device identity/eligibility here.
pub struct CurrentDevices {
    pub eligible_disposed: Vec<String>,
    pub kept: Vec<String>,
}
pub struct PlanCalculator {
    logging: SshLogging,
    logger: Component,
    timing: ProvisioningTiming,
}
impl PlanCalculator {
    pub fn new(logging: SshLogging, timing: ProvisioningTiming) -> Self {
        let logger = logging.component("marathon_ios::device::provisioning_plan");
        Self {
            logging,
            logger,
            timing,
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn plan(
        &self,
        transport: &Transport,
        bin: &AppleBinaryEnvironment,
        targets: &[AppleTarget],
        cycle: &WorkerProvisioningContext,
        registered: impl FnOnce() -> Vec<String>,
        shutdown_unused: bool,
        current: impl FnOnce() -> ProvisioningResult<CurrentDevices>,
        caller: &VendorContext,
    ) -> ProvisioningResult<ProvisioningPlan> {
        let inventory = cycle.inventory(bin, caller).await?;
        let mut available = IndexMap::new();
        for record in &inventory.devices.devices {
            if record.is_available == Some(true) {
                available.entry(record.udid.clone()).or_insert(record);
            }
        }
        let mut simulators = Vec::new();
        let mut profiles = Vec::new();
        let mut plan = ProvisioningPlan::default();
        for target in targets {
            match target {
                AppleTarget::Simulator { udid } => simulators.push(udid.to_owned()),
                AppleTarget::Physical { udid } => {
                    plan.physical_devices.insert(udid.to_owned());
                }
                AppleTarget::SimulatorProfile(profile) => profiles.push(profile),
                AppleTarget::Host => self.logging.log(
                    &self.logger,
                    Level::WARN,
                    || "iOS vendor runs do not use host device for testing. Skipping".into(),
                    None,
                ),
            }
        }
        let mut used: IndexSet<_> = registered().into_iter().collect();
        let mut explicit = IndexSet::new();
        for udid in simulators {
            if available.contains_key(&udid) {
                used.insert(udid.clone());
                explicit.insert(udid);
            } else {
                self.logging.log(
                    &self.logger,
                    Level::ERROR,
                    || format!("udid {udid} is not available at {}", transport_text(transport, "Local")),
                    None,
                );
            }
        }
        available.retain(|udid, _| !used.contains(udid));
        for profile in profiles {
            let kind = profile.fully_qualified_device_type_id().to_owned();
            let runtime = profile.fully_qualified_runtime_id().map(|s| s.to_owned());
            let reusable = available
                .iter()
                .find(|(_, d)| {
                    d.device_type_identifier.as_ref() == Some(&kind)
                        && runtime.as_ref().is_none_or(|r| d.runtime.as_ref() == Some(r))
                })
                .map(|(udid, _)| udid.clone());
            if let Some(udid) = reusable {
                available.shift_remove(&udid);
                plan.reused_profiles.insert(udid, profile.clone());
            } else {
                plan.needs_provisioning.push(profile.clone());
            }
        }
        if shutdown_unused {
            let booted: Vec<_> = available
                .values()
                .filter(|d| d.state == SimctlDeviceState::Booted)
                .collect();
            if !booted.is_empty() {
                let started = self.timing.elapsed()?;
                for record in &booted {
                    bin.xcrun.simctl.simulator.shutdown(&record.udid, caller).await?;
                }
                self.timing.info(|| {
                    Ok(format!(
                        "Worker {}: shut down {} unused simulators in {} ms",
                        transport.id().to_owned(),
                        booted.len(),
                        self.timing.elapsed()?.wrapping_sub(started)
                    ))
                })?;
            }
        }
        let current = current()?;
        plan.existing_simulators = explicit;
        plan.existing_simulators.extend(current.eligible_disposed);
        plan.existing_simulators.extend(plan.reused_profiles.keys().cloned());
        for udid in current.kept {
            plan.existing_simulators.shift_remove(&udid);
        }
        Ok(plan)
    }
}
