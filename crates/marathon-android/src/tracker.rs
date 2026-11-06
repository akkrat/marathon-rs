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
pub struct DeviceStateTracker {
    devices: Mutex<Vec<(String, DeviceState)>>,
    /// D22: false while this tracker's adb server is away, so that batches wait instead of burning their
    /// retries on connections that cannot work.
    server_available: tokio::sync::watch::Sender<bool>,
}
impl Default for DeviceStateTracker {
    fn default() -> Self {
        Self {
            devices: Mutex::default(),
            server_available: tokio::sync::watch::Sender::new(true),
        }
    }
}
impl DeviceStateTracker {
    pub(crate) fn set_server_available(&self, available: bool) {
        self.server_available.send_replace(available);
    }
    pub(crate) fn is_server_available(&self) -> bool {
        *self.server_available.borrow()
    }
    /// Returns at once while the server is there, as it always is outside an outage.
    pub(crate) async fn server_available(&self) {
        let mut available = self.server_available.subscribe();
        // The sender lives in self, so this can only end by finding `true`.
        let _ = available.wait_for(|available| *available).await;
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
