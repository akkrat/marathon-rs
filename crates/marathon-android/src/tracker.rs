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
    /// D48: since when, on the vendor clock, the server has been away, or each held serial.
    server_away_since: Option<i64>,
    held_since: HashMap<String, i64>,
    /// D48: when the grace periods an outage took over began. Each goes on counting for its own serial alone.
    held_before_outage: HashMap<String, i64>,
    /// D49: how many lost requests each serial has outlived in this run.
    request_losses: HashMap<String, u32>,
    /// D22 (U170): the serials whose logcat and reverse port forwards are being restored after their server came back,
    /// with since when, on the vendor clock, each has been kept waiting. Each opens on its own.
    restoring: HashMap<String, i64>,
}
impl Gate {
    fn closed_for(&self, serial: &str) -> bool {
        !self.server || self.held.iter().any(|held| held == serial) || self.restoring.contains_key(serial)
    }
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
                server_away_since: None,
                held_since: HashMap::new(),
                held_before_outage: HashMap::new(),
                request_losses: HashMap::new(),
                restoring: HashMap::new(),
            }),
        }
    }
}
impl DeviceStateTracker {
    /// An open grace period's time carries on in the outage that takes over from it, for its own serial. The other devices
    /// on the server were fine until now.
    pub(crate) fn server_lost(&self, now: i64) {
        self.gate.send_modify(|gate| {
            gate.server = false;
            gate.server_away_since = Some(now);
            for (serial, since) in &gate.held_since {
                gate.held_before_outage.entry(serial.clone()).or_insert(*since);
            }
            // The new outage's gate covers these; they are restored again once it is over.
            gate.restoring.clear();
        });
    }
    pub(crate) fn server_back(&self) {
        self.server_back_restoring(&[]);
    }
    /// The server is back, and `restoring` stay closed until [`Self::restored`], each for itself, in the same step, so
    /// that no batch passes in between. They go on counting from when they were first kept waiting (D48).
    pub(crate) fn server_back_restoring(&self, restoring: &[String]) {
        self.gate.send_modify(|gate| {
            for serial in restoring {
                if let Some(since) = Self::away_since_in(gate, serial) {
                    gate.restoring.insert(serial.clone(), since);
                }
            }
            gate.server = true;
            gate.server_away_since = None;
            gate.held_before_outage.clear();
        });
    }
    /// D22 (U170): this serial's restoration is over, or no longer needed.
    pub(crate) fn restored(&self, serial: &str) {
        self.gate
            .send_if_modified(|gate| gate.restoring.remove(serial).is_some());
    }
    pub(crate) fn is_restoring(&self, serial: &str) -> bool {
        self.gate.borrow().restoring.contains_key(serial)
    }
    pub(crate) fn is_server_available(&self) -> bool {
        self.gate.borrow().server
    }
    /// D41: a grace period starts for this serial. One that went offline again while its last grace's hold is still on
    /// (its recovery is running) has been unavailable throughout, so it keeps that hold and its start (D48).
    pub(crate) fn hold(&self, serial: &str, now: i64) {
        self.gate.send_modify(|gate| {
            if !gate.held.iter().any(|held| held == serial) {
                gate.held.push(serial.to_owned());
            }
            gate.held_since.entry(serial.to_owned()).or_insert(now);
            *gate.blips.entry(serial.to_owned()).or_default() += 1;
        });
    }
    pub(crate) fn release(&self, serial: &str) {
        self.gate.send_modify(|gate| {
            gate.held.retain(|held| held != serial);
            gate.held_since.remove(serial);
        });
    }
    pub(crate) fn release_all(&self) {
        self.gate.send_modify(|gate| {
            gate.held.clear();
            gate.held_since.clear();
        });
    }
    /// D48: since when this serial has been kept waiting by an outage or a grace period, if it is.
    pub(crate) fn away_since(&self, serial: &str) -> Option<i64> {
        Self::away_since_in(&self.gate.borrow(), serial)
    }
    fn away_since_in(gate: &Gate, serial: &str) -> Option<i64> {
        let held = gate
            .held_since
            .get(serial)
            .or_else(|| gate.held_before_outage.get(serial))
            .or_else(|| gate.restoring.get(serial))
            .copied();
        match (held, gate.server_away_since) {
            (Some(held), Some(server)) => Some(held.min(server)),
            (held, server) => held.or(server),
        }
    }
    pub(crate) fn request_losses(&self, serial: &str) -> u32 {
        self.gate.borrow().request_losses.get(serial).copied().unwrap_or(0)
    }
    pub(crate) fn count_request_loss(&self, serial: &str) {
        self.gate.send_modify(|gate| {
            *gate.request_losses.entry(serial.to_owned()).or_default() += 1;
        });
    }
    pub(crate) fn is_held(&self, serial: &str) -> bool {
        self.gate.borrow().held.iter().any(|held| held == serial)
    }
    pub(crate) fn blips(&self, serial: &str) -> u32 {
        self.gate.borrow().blips.get(serial).copied().unwrap_or(0)
    }
    /// Returns at once outside an outage, a grace period or a restoration, so at once in a healthy run.
    pub(crate) async fn available(&self, serial: &str) {
        let mut gate = self.gate.subscribe();
        // The sender lives in self, so this can only end by finding the way clear.
        let _ = gate.wait_for(|gate| !gate.closed_for(serial)).await;
    }
    /// The opposite of [`Self::available`].
    pub(crate) async fn unavailable(&self, serial: &str) {
        let mut gate = self.gate.subscribe();
        let _ = gate.wait_for(|gate| gate.closed_for(serial)).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// D48: the reviewer's witness. A device whose grace began at 1,000 has been away since then; one that was healthy
    /// until its server went at 9,000 has been away since 9,000, not since the other's grace.
    #[test]
    fn an_outage_takes_over_each_grace_for_its_own_serial_d48() {
        let tracker = DeviceStateTracker::default();
        tracker.hold("graced", 1000);
        tracker.server_lost(9000);
        // The grace periods end as the outage takes over (`Blips::abandon`).
        tracker.release_all();
        assert_eq!(tracker.away_since("graced"), Some(1000));
        assert_eq!(tracker.away_since("healthy"), Some(9000));
        tracker.server_back();
        assert_eq!(tracker.away_since("graced"), None);
        assert_eq!(tracker.away_since("healthy"), None);
        // A later outage starts afresh for both.
        tracker.server_lost(20_000);
        assert_eq!(tracker.away_since("graced"), Some(20_000));
        assert_eq!(tracker.away_since("healthy"), Some(20_000));
    }
    /// D48, U170: a device going offline again while its recovery from a grace still holds it was unavailable throughout:
    /// it is held once, from the first grace, and one release frees it.
    #[test]
    fn a_second_grace_during_a_recovery_keeps_the_first_holds_start_d48() {
        let tracker = DeviceStateTracker::default();
        tracker.hold("flapping", 1000);
        // The grace ends, the device's recovery runs, and the device goes offline again before it reports.
        tracker.hold("flapping", 12_000);
        assert_eq!(tracker.away_since("flapping"), Some(1000));
        assert_eq!(tracker.blips("flapping"), 2);
        tracker.release("flapping");
        assert!(!tracker.is_held("flapping"));
        assert_eq!(tracker.away_since("flapping"), None);
    }
}
