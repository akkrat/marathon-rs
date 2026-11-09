//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/DeviceTracker.kt

use super::error::ProvisioningResult;
use indexmap::IndexMap;
use marathon_logging::{Level, Component};
use marathon_apple::cmd::{failure, remote::ssh::sshj::config::SshLogging};
use marathon_config::devices::{AppleTarget, MarathonDevices, Transport};
use marathon_core::error::VendorError;
use marathon_error::FailureKind;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackingUpdate {
    Connected(AppleTarget),
    Disconnected(AppleTarget),
}
#[derive(Clone)]
enum DeviceState {
    Online(AppleTarget),
    Provisioned(AppleTarget, usize),
}
impl DeviceState {
    fn target(&self) -> &AppleTarget {
        match self {
            Self::Online(t) | Self::Provisioned(t, _) => t,
        }
    }
}
#[derive(Clone)]
pub struct WorkerTracker {
    pub transport: Transport,
    devices: IndexMap<String, DeviceState>,
    logging: SshLogging,
    logger: Component,
}
pub fn target_id(target: &AppleTarget) -> String {
    match target {
        AppleTarget::Host => "host".into(),
        AppleTarget::Physical { udid } | AppleTarget::Simulator { udid } => udid.clone(),
        AppleTarget::SimulatorProfile(profile) => {
            let mut id = profile.fully_qualified_device_type_id();
            id.push('-');
            id.push_str(&profile.fully_qualified_runtime_id().unwrap_or_default());
            id
        }
    }
}
fn same_kind(left: &AppleTarget, right: &AppleTarget) -> bool {
    std::mem::discriminant(left) == std::mem::discriminant(right)
}
impl WorkerTracker {
    pub fn new(transport: Transport, logging: SshLogging) -> Self {
        let logger = logging.component("marathon_ios::device::device_tracker");
        Self {
            transport,
            devices: IndexMap::new(),
            logging,
            logger,
        }
    }
    fn warn(&self, current: usize, desired: usize) {
        self.logging.log(
            &self.logger,
            Level::WARN,
            || format!("Deprovisioning simulators is not supported. Current = {current}, desired = {desired}"),
            None,
        );
    }
    pub fn update(&mut self, updated: &[AppleTarget]) -> ProvisioningResult<Vec<(AppleTarget, TrackingUpdate)>> {
        // S3: validate before mutating, including aliases against retained high-water entries.
        let mut keys: IndexMap<String, &AppleTarget> = IndexMap::new();
        for target in updated {
            let id = target_id(target);
            let previous = keys
                .get(&id)
                .copied()
                .or_else(|| self.devices.get(&id).map(DeviceState::target));
            if previous.is_some_and(|previous| !same_kind(previous, target)) {
                return Err(VendorError::Other(failure(
                    FailureKind::IllegalArgument,
                    format!("Conflicting target kinds for tracking key {}", id.to_owned()),
                ))
                .into());
            }
            keys.insert(id, target);
        }
        let mut updates = Vec::new();
        let removed: Vec<_> = self
            .devices
            .keys()
            .filter(|id| !keys.contains_key(*id))
            .cloned()
            .collect();
        for id in removed {
            match &self.devices[&id] {
                DeviceState::Online(target) => {
                    updates.push((target.clone(), TrackingUpdate::Disconnected(target.clone())));
                    self.devices.shift_remove(&id);
                }
                DeviceState::Provisioned(_, count) => self.warn(*count, 0),
            }
        }
        let mut profiles: IndexMap<String, (&AppleTarget, usize)> = IndexMap::new();
        for target in updated {
            let id = target_id(target);
            if matches!(target, AppleTarget::SimulatorProfile(_)) {
                profiles.entry(id).or_insert((target, 0)).1 += 1;
            } else if !self.devices.contains_key(&id) {
                self.devices.insert(id, DeviceState::Online(target.clone()));
                updates.push((target.clone(), TrackingUpdate::Connected(target.clone())));
            }
        }
        for (id, (target, desired)) in profiles {
            let current = match self.devices.get(&id) {
                Some(DeviceState::Provisioned(_, count)) => *count,
                _ => 0,
            };
            if desired > current {
                self.devices
                    .insert(id, DeviceState::Provisioned(target.clone(), desired));
                for _ in current..desired {
                    updates.push((target.clone(), TrackingUpdate::Connected(target.clone())));
                }
            } else if desired < current {
                self.warn(current, desired);
            }
        }
        Ok(updates)
    }
}
pub struct DeviceTracker {
    workers: IndexMap<String, WorkerTracker>,
    logging: SshLogging,
}
impl DeviceTracker {
    pub fn new(logging: SshLogging) -> Self {
        Self {
            workers: IndexMap::new(),
            logging,
        }
    }
    pub fn update(
        &mut self,
        devices: &MarathonDevices,
    ) -> ProvisioningResult<IndexMap<Transport, Vec<TrackingUpdate>>> {
        // A malformed snapshot cannot leave half of the workers updated.
        let mut workers = self.workers.clone();
        let mut result = IndexMap::new();
        for (id, tracker) in &mut workers {
            if !devices.workers.iter().any(|w| w.transport.id() == *id) {
                let updates = tracker.update(&[])?.into_iter().map(|(_, update)| update).collect();
                result.insert(tracker.transport.clone(), updates);
            }
        }
        for worker in &devices.workers {
            let tracker = workers
                .entry(worker.transport.id())
                .or_insert_with(|| WorkerTracker::new(worker.transport.clone(), self.logging.clone()));
            let updates = tracker
                .update(&worker.devices)?
                .into_iter()
                .map(|(_, update)| update)
                .collect();
            result.insert(tracker.transport.clone(), updates);
        }
        self.workers = workers;
        Ok(result)
    }
}
