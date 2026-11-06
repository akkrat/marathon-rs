//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/DeviceStateTracker.kt
use crate::support::lock;
use marathon_adb::client::{
    AndroidDebugBridgeClient,
    device::{Device, DeviceState},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackingUpdate {
    Connected,
    Disconnected,
    NothingToDo,
}
impl TrackingUpdate {
    pub fn name(self) -> &'static str {
        match self {
            Self::Connected => "CONNECTED",
            Self::Disconnected => "DISCONNECTED",
            Self::NothingToDo => "NOTHING_TO_DO",
        }
    }
}
/// D41: the states adb passes through while a transport comes back. `unauthorized` is not one, as it waits for a person.
pub(crate) fn is_passing(state: DeviceState) -> bool {
    matches!(
        state,
        DeviceState::Offline | DeviceState::Connecting | DeviceState::Authorizing
    )
}
/// What batches wait for, so that they do not burn their retries on connections that cannot work.
struct Gate {
    /// D22: false while this tracker's adb server is away.
    server: bool,
    /// D41: the serials in their grace period.
    held: Vec<String>,
    /// D41: how many grace periods each serial has had in this run.
    blips: HashMap<String, u32>,
}
pub struct DeviceStateTracker {
    devices: Mutex<Vec<(String, DeviceState)>>,
    gate: tokio::sync::watch::Sender<Gate>,
}
impl Default for DeviceStateTracker {
    fn default() -> Self {
        Self {
            devices: Mutex::default(),
            gate: tokio::sync::watch::Sender::new(Gate {
                server: true,
                held: vec![],
                blips: HashMap::new(),
            }),
        }
    }
}
impl DeviceStateTracker {
    pub(crate) fn set_server_available(&self, available: bool) {
        self.gate.send_modify(|gate| gate.server = available);
    }
    pub(crate) fn is_server_available(&self) -> bool {
        self.gate.borrow().server
    }
    /// D41: a grace period starts for this serial.
    pub(crate) fn hold(&self, serial: &str) {
        self.gate.send_modify(|gate| {
            gate.held.push(serial.to_owned());
            *gate.blips.entry(serial.to_owned()).or_default() += 1;
        });
    }
    pub(crate) fn release(&self, serial: &str) {
        self.gate.send_modify(|gate| gate.held.retain(|held| held != serial));
    }
    pub(crate) fn release_all(&self) {
        self.gate.send_modify(|gate| gate.held.clear());
    }
    pub(crate) fn is_held(&self, serial: &str) -> bool {
        self.gate.borrow().held.iter().any(|held| held == serial)
    }
    pub(crate) fn blips(&self, serial: &str) -> u32 {
        self.gate.borrow().blips.get(serial).copied().unwrap_or(0)
    }
    /// Returns at once outside an outage or a grace period, so at once in a healthy run.
    pub(crate) async fn available(&self, serial: &str) {
        let mut gate = self.gate.subscribe();
        // The sender lives in self, so this can only end by finding the way clear.
        let _ = gate
            .wait_for(|gate| gate.server && !gate.held.iter().any(|held| held == serial))
            .await;
    }
    /// The opposite of [`Self::available`].
    pub(crate) async fn unavailable(&self, serial: &str) {
        let mut gate = self.gate.subscribe();
        let _ = gate
            .wait_for(|gate| !gate.server || gate.held.iter().any(|held| held == serial))
            .await;
    }
    /// The serials currently in state `device`.
    pub(crate) fn device_serials(&self) -> Vec<String> {
        lock(&self.devices)
            .iter()
            .filter(|(_, state)| *state == DeviceState::Device)
            .map(|(serial, _)| serial.clone())
            .collect()
    }
    pub fn update(&self, devices: &[Device]) -> Vec<(String, TrackingUpdate)> {
        let mut states = lock(&self.devices);
        let mut updates = Vec::new();
        // Kotlin leaves vanished non-device entries in its map.
        states.retain(|(s, state)| {
            if *state == DeviceState::Device && !devices.iter().any(|d| d.serial == *s) {
                updates.push((s.clone(), TrackingUpdate::Disconnected));
                false
            } else {
                true
            }
        });
        for device in devices {
            let old = states.iter_mut().find(|(s, _)| *s == device.serial);
            let previous = old.as_ref().map(|(_, s)| *s);
            match old {
                Some((_, s)) => *s = device.state,
                None => states.push((device.serial.clone(), device.state)),
            }
            let update = match (previous, device.state) {
                (Some(DeviceState::Device), DeviceState::Device) => TrackingUpdate::NothingToDo,
                (Some(DeviceState::Device), _) => TrackingUpdate::Disconnected,
                (_, DeviceState::Device) => TrackingUpdate::Connected,
                _ => TrackingUpdate::NothingToDo,
            };
            updates.push((device.serial.clone(), update));
        }
        updates
    }
    pub fn get_state(&self, serial: &str) -> Option<DeviceState> {
        lock(&self.devices).iter().find(|(s, _)| s == serial).map(|(_, s)| *s)
    }
}
#[derive(Default)]
pub struct MultiServerDeviceStateTracker {
    trackers: Mutex<HashMap<String, Arc<DeviceStateTracker>>>,
}
impl MultiServerDeviceStateTracker {
    pub fn tracker(&self, client: &AndroidDebugBridgeClient) -> Arc<DeviceStateTracker> {
        let key = format!(
            "{}:{}/{}",
            client.host_name().unwrap_or(""),
            client.port(),
            client.host()
        );
        lock(&self.trackers).entry(key).or_default().clone()
    }
    pub fn update(&self, client: &AndroidDebugBridgeClient, devices: &[Device]) -> Vec<(String, TrackingUpdate)> {
        self.tracker(client).update(devices)
    }
}
