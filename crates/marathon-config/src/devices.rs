//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/configuration/Marathondevices.kt,
//! vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/configuration/AppleTarget.kt and
//! vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/configuration/Transport.kt.
//!
//! The P2 (Marathondevices) types only; binding is `crate::bind_devices`
//! and the pipeline entry is `crate::pipeline::bind_devices_bytes`.
//! Runtime use (providers, `DeviceTracker`, transports) is WP 4.10a/5.x,
//! which imports these types from here.

use crate::vendor::SshAuthentication;

use std::hash::{Hash, Hasher};

/// Port of `Marathondevices` (Marathondevices.kt:5-7).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MarathonDevices {
    pub workers: Vec<Worker>,
}

/// Port of `Worker` (Marathondevices.kt:9-12).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Worker {
    pub transport: Transport,
    pub devices: Vec<AppleTarget>,
}

/// Port of `Transport` (Transport.kt). `Eq`/`Hash` derive from the bound
/// fields: `DeviceTracker` (WP 5.4) and the macOS provider key maps by
/// transport. `Hash` follows the authentication fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    Local,
    Ssh {
        addr: String,
        port: i32,
        authentication: Option<SshAuthentication>,
        check_reachability: bool,
    },
}

impl Transport {
    /// `Transport.id()`: `"local"`, or `"addr:port"` for SSH.
    pub fn id(&self) -> String {
        match self {
            Self::Local => "local".into(),
            Self::Ssh { addr, port, .. } => {
                let mut out = addr.clone();
                out.push_str(&format!(":{port}"));
                out
            }
        }
    }
}

impl Hash for Transport {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        if let Self::Ssh {
            addr,
            port,
            authentication,
            check_reachability,
        } = self
        {
            addr.hash(state);
            port.hash(state);
            hash_authentication(authentication, state);
            check_reachability.hash(state);
        }
    }
}

fn hash_authentication<H: Hasher>(authentication: &Option<SshAuthentication>, state: &mut H) {
    std::mem::discriminant(authentication).hash(state);
    match authentication {
        Some(SshAuthentication::Password { username, password }) => {
            0u8.hash(state);
            username.hash(state);
            password.hash(state);
        }
        Some(SshAuthentication::PublicKey { username, key }) => {
            1u8.hash(state);
            username.hash(state);
            key.hash(state);
        }
        None => {}
    }
}

/// Port of `AppleTarget` (AppleTarget.kt).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AppleTarget {
    Simulator { udid: String },
    Physical { udid: String },
    SimulatorProfile(SimulatorProfile),
    Host,
}

/// Port of `AppleTarget.SimulatorProfile`. The raw bound ids are the value
/// (equality/hash use them); the fully-qualified forms are methods,
/// computed on demand as Kotlin's lazy properties are.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SimulatorProfile {
    pub device_type_id: String,
    pub runtime_id: Option<String>,
    pub new_name_prefix: String,
}

pub const FQCN_SIM_DEVICE_TYPE: &str = "com.apple.CoreSimulator.SimDeviceType";
pub const FQCN_SIM_RUNTIME: &str = "com.apple.CoreSimulator.SimRuntime";

impl SimulatorProfile {
    /// `fullyQualifiedDeviceTypeId`.
    pub fn fully_qualified_device_type_id(&self) -> String {
        expand_textual_id(&self.device_type_id, FQCN_SIM_DEVICE_TYPE)
    }

    /// `fullyQualifiedRuntimeId`.
    pub fn fully_qualified_runtime_id(&self) -> Option<String> {
        self.runtime_id
            .as_ref()
            .map(|id| expand_textual_id(id, FQCN_SIM_RUNTIME))
    }
}

/// `expandTextualId` (AppleTarget.kt:46-57): an id already starting with
/// the FQCN (no dot-boundary check) passes verbatim; otherwise every
/// space and dot becomes `-` and `"<FQCN>."` is prefixed.
fn expand_textual_id(id: &str, expected_fqcn: &str) -> String {
    if id.starts_with(expected_fqcn) {
        return id.to_owned();
    }
    format!("{expected_fqcn}.{}", id.replace([' ', '.'], "-"))
}
