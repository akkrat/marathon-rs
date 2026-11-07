//! The shared state of a running server: devices, their filesystems and reverse rules, the expectations
//! with their use counts, and the signals that end sessions.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::sync::watch;

use super::fs::DeviceFs;
use super::script::{CompiledDevice, CompiledScript, KillBehaviour, Response, Rule, ServiceKind};
use super::transcript::{Transcript, lock};

pub(crate) struct World {
    pub capture_output: std::sync::atomic::AtomicBool,
    pub transcript: Transcript,
    pub adb_version: u32,
    pub host_features: Vec<String>,
    pub on_kill: KillBehaviour,
    pub builtins: bool,
    pub strict: bool,
    pub default: Response,
    state: Mutex<State>,
    /// Bumped whenever the device list (serials or states) changes; `host:track-devices` follows it.
    pub devices_changed: watch::Sender<u64>,
    /// The gates the test has opened, for `gate` output steps.
    pub gates: watch::Sender<BTreeSet<String>>,
    /// Asks the listener supervisor to drop every connection (and maybe listen again later).
    pub kill: tokio::sync::mpsc::UnboundedSender<KillRequest>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum KillRequest {
    /// Stop for good, like the real server after `host:kill`.
    Exit,
    /// Drop every connection and stop listening, as if the server died; listen again after the delay
    /// if there is one.
    Down { restart_after: Option<std::time::Duration> },
}

struct State {
    devices: Vec<Device>,
    common: Vec<RuleState>,
    next_transport_id: u64,
    next_reverse_port: u16,
}

struct RuleState {
    rule: Rule,
    remaining: Option<u32>,
}

pub(crate) struct Device {
    pub serial: String,
    pub state: String,
    pub features: Vec<String>,
    pub getprop: BTreeMap<String, String>,
    pub fs: DeviceFs,
    rules: Vec<RuleState>,
    pub framebuffer: Option<Arc<Vec<u8>>>,
    pub connected: bool,
    pub transport_id: u64,
    /// Bumped when the device disconnects or leaves the `device` state; its open sessions end.
    life: watch::Sender<u64>,
    /// Reverse forwards, local spec → remote spec, in creation order.
    pub reverse: Vec<(String, String)>,
}

/// Which device a host request or transport switch addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Selector {
    Serial(String),
    Any,
    Usb,
    Local,
    Id(u64),
}

/// A device session: the serial, and a receiver that changes when the session must end.
pub(crate) struct Transport {
    pub serial: String,
    pub transport_id: u64,
    pub life: watch::Receiver<u64>,
}

/// Emulators and network devices are "local" transports in adb's terms; the rest are USB.
fn is_local(serial: &str) -> bool {
    serial.starts_with("emulator-") || serial.contains(':')
}

fn rules(list: &[Rule]) -> Vec<RuleState> {
    list.iter()
        .map(|r| RuleState {
            rule: r.clone(),
            remaining: r.times,
        })
        .collect()
}

impl World {
    pub fn snapshot(&self) -> serde_json::Value {
        let state = lock(&self.state);
        let mut value = json!({"devices": state.devices.iter().map(|d| json!({
            "serial": d.serial, "state": d.state, "connected": d.connected,
            "features": d.features, "getprop": d.getprop, "reverse": d.reverse,
            "files": d.fs.entries().map(|(path, node)| json!({
                "path": path, "mode": node.mode, "mtime": node.mtime,
                "content": node.content
            })).collect::<Vec<_>>()
        })).collect::<Vec<_>>()});
        value.sort_all_objects();
        value
    }

    pub fn new(
        script: &CompiledScript,
        transcript: Transcript,
        kill: tokio::sync::mpsc::UnboundedSender<KillRequest>,
    ) -> World {
        let mut state = State {
            devices: Vec::new(),
            common: rules(&script.shell),
            next_transport_id: 1,
            next_reverse_port: 40000,
        };
        for d in &script.devices {
            state.add(d);
        }
        World {
            capture_output: false.into(),
            transcript,
            adb_version: script.adb_version,
            host_features: script.host_features.clone(),
            on_kill: script.on_kill,
            builtins: script.builtins,
            strict: script.strict,
            default: script.default.clone(),
            state: Mutex::new(state),
            devices_changed: watch::Sender::new(0),
            gates: watch::Sender::new(BTreeSet::new()),
            kill,
        }
    }

    fn changed(&self) {
        self.devices_changed.send_modify(|g| *g += 1);
    }

    /// The `host:devices` text, or `host:devices-l` with `long`.
    pub fn device_list(&self, long: bool) -> String {
        let st = lock(&self.state);
        let mut out = String::new();
        for d in st.devices.iter().filter(|d| d.connected) {
            if long {
                // adb's list_transports: "%-22s %s", then product, model and device, then the id last.
                out.push_str(&format!("{:<22} {}", d.serial, d.state));
                for (label, key) in [
                    ("product", "ro.product.name"),
                    ("model", "ro.product.model"),
                    ("device", "ro.product.device"),
                ] {
                    if let Some(v) = d.getprop.get(key).filter(|v| !v.is_empty()) {
                        let v: String = v
                            .chars()
                            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                            .collect();
                        out.push_str(&format!(" {label}:{v}"));
                    }
                }
                out.push_str(&format!(" transport_id:{}\n", d.transport_id));
            } else {
                out.push_str(&format!("{}\t{}\n", d.serial, d.state));
            }
        }
        out
    }

    /// Finds the device for a request. `online` demands the `device` state, as a transport switch does.
    pub fn resolve(&self, sel: &Selector, online: bool) -> Result<Transport, String> {
        let st = lock(&self.state);
        let connected = st.devices.iter().filter(|d| d.connected);
        let candidates: Vec<&Device> = match sel {
            Selector::Serial(s) => connected.filter(|d| &d.serial == s).collect(),
            Selector::Id(id) => connected.filter(|d| d.transport_id == *id).collect(),
            Selector::Any => connected.collect(),
            Selector::Usb => connected.filter(|d| !is_local(&d.serial)).collect(),
            Selector::Local => connected.filter(|d| is_local(&d.serial)).collect(),
        };
        // Messages as adb's acquire_one_transport words them.
        let d = match (candidates.as_slice(), sel) {
            ([d], _) => *d,
            ([], Selector::Serial(s)) => return Err(format!("device '{s}' not found")),
            ([], Selector::Id(id)) => return Err(format!("no device with transport id '{id}'")),
            ([], Selector::Usb) => return Err("no devices found".into()),
            ([], Selector::Local) => return Err("no emulators found".into()),
            ([], _) => return Err("no devices/emulators found".into()),
            (_, Selector::Usb) => return Err("more than one device".into()),
            (_, Selector::Local) => return Err("more than one emulator".into()),
            (_, _) => return Err("more than one device/emulator".into()),
        };
        if online {
            match d.state.as_str() {
                "offline" => return Err("device offline".into()),
                "unauthorized" => {
                    return Err("device unauthorized.\nThis adb server's $ADB_VENDOR_KEYS is not set\nTry 'adb kill-server' if that seems wrong.\nOtherwise check for a confirmation dialog on your device.".into());
                }
                "authorizing" => return Err("device still authorizing".into()),
                "connecting" => return Err("device still connecting".into()),
                _ => {}
            }
        }
        let mut life = d.life.subscribe();
        life.mark_unchanged();
        Ok(Transport {
            serial: d.serial.clone(),
            transport_id: d.transport_id,
            life,
        })
    }

    /// Runs `f` on a device, connected or not.
    pub fn with_device<R>(&self, serial: &str, f: impl FnOnce(&mut Device) -> R) -> Option<R> {
        let mut st = lock(&self.state);
        st.devices.iter_mut().find(|d| d.serial == serial).map(f)
    }

    /// The first expectation that matches, device ones before shared ones; counts the use.
    pub fn take_rule(&self, serial: &str, kind: ServiceKind, command: &str) -> Option<(String, Response)> {
        let mut st = lock(&self.state);
        let State { devices, common, .. } = &mut *st;
        let own = devices
            .iter_mut()
            .find(|d| d.serial == serial)
            .map(|d| d.rules.iter_mut());
        for r in own.into_iter().flatten().chain(common.iter_mut()) {
            if r.remaining == Some(0) || r.rule.service.is_some_and(|s| s != kind) || !r.rule.matcher.matches(command) {
                continue;
            }
            if let Some(n) = r.remaining.as_mut() {
                *n -= 1;
            }
            return Some((r.rule.id.clone(), r.rule.response.clone()));
        }
        None
    }

    /// Gives back a use taken by [`World::take_rule`] whose command didn't happen.
    pub fn release_rule(&self, serial: &str, id: &str) {
        let mut st = lock(&self.state);
        let State { devices, common, .. } = &mut *st;
        let own = devices
            .iter_mut()
            .find(|d| d.serial == serial)
            .map(|d| d.rules.iter_mut());
        if let Some(r) = own
            .into_iter()
            .flatten()
            .chain(common.iter_mut())
            .find(|r| r.rule.id == id)
            && let Some(n) = r.remaining.as_mut()
        {
            *n += 1;
        }
    }

    pub fn allocate_reverse_port(&self) -> u16 {
        let mut st = lock(&self.state);
        st.next_reverse_port = st.next_reverse_port.wrapping_add(1).max(40001);
        st.next_reverse_port
    }

    pub fn add_device(&self, d: &CompiledDevice) -> Result<(), String> {
        {
            let mut st = lock(&self.state);
            if st.devices.iter().any(|x| x.serial == d.serial) {
                return Err(format!("device {} already exists", d.serial));
            }
            st.add(d);
        }
        self.transcript
            .record(json!({"type": "event", "event": "add", "serial": d.serial, "connected": d.connected}));
        self.changed();
        Ok(())
    }

    pub fn connect(&self, serial: &str) -> Result<(), String> {
        let id = {
            let mut st = lock(&self.state);
            let next = st.next_transport_id;
            let d = st
                .devices
                .iter_mut()
                .find(|d| d.serial == serial)
                .ok_or(format!("no device {serial}"))?;
            if d.connected {
                return Ok(());
            }
            d.connected = true;
            d.transport_id = next;
            st.next_transport_id += 1;
            next
        };
        self.transcript
            .record(json!({"type": "event", "event": "connect", "serial": serial, "transport_id": id}));
        self.changed();
        Ok(())
    }

    pub fn disconnect(&self, serial: &str) -> Result<(), String> {
        self.with_device(serial, |d| {
            d.connected = false;
            d.reverse.clear();
            d.life.send_modify(|g| *g += 1);
        })
        .ok_or(format!("no device {serial}"))?;
        self.transcript
            .record(json!({"type": "event", "event": "disconnect", "serial": serial}));
        self.changed();
        Ok(())
    }

    pub fn set_state(&self, serial: &str, state: &str) -> Result<(), String> {
        self.with_device(serial, |d| {
            if d.state == "device" && state != "device" {
                d.life.send_modify(|g| *g += 1);
            }
            d.state = state.to_string();
        })
        .ok_or(format!("no device {serial}"))?;
        self.transcript
            .record(json!({"type": "event", "event": "state", "serial": serial, "state": state}));
        self.changed();
        Ok(())
    }
}

impl State {
    fn add(&mut self, d: &CompiledDevice) {
        let mut fs = DeviceFs::default();
        for f in &d.files {
            // Script::compile has already rejected clashes such as a file under a file.
            let _ = match &f.content {
                Some(c) => fs.write(&f.path, c.clone(), f.mode, f.mtime),
                None => fs.mkdir(&f.path, f.mode, f.mtime),
            };
        }
        let transport_id = if d.connected {
            self.next_transport_id += 1;
            self.next_transport_id - 1
        } else {
            0
        };
        self.devices.push(Device {
            serial: d.serial.clone(),
            state: d.state.clone(),
            features: d.features.clone(),
            getprop: d.getprop.clone(),
            fs,
            rules: rules(&d.shell),
            framebuffer: d.framebuffer.clone(),
            connected: d.connected,
            transport_id,
            life: watch::Sender::new(0),
            reverse: Vec::new(),
        });
    }
}
