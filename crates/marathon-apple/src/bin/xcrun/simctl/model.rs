//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/simctl/model/SimctlDevice.kt,
//! model/SimctlDeviceList.kt, model/SimctlDeviceType.kt, model/SimctlListDevicesOutput.kt and model/SimctlRuntime.kt.
//!
//! Approved P2: the shapes `simctl list --json` produces decode as Gson does (field names, scalar-to-string
//! conversion, unknown states, JSON object order). Any other shape, including the ones where Gson leaks a null
//! into a non-null Kotlin property or throws outside `JsonSyntaxException`, is one [`SimctlDecodeError`].
//! Approved P3: `SimctlListDevicesOutput.supportedDeviceTypeIds`, an unused empty lazy, is not ported.

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimctlDeviceState {
    Unknown,
    Booted,
    Shutdown,
    Creating,
    Booting,
    ShuttingDown,
}
impl SimctlDeviceState {
    pub const ALL: [Self; 6] = [
        Self::Unknown,
        Self::Booted,
        Self::Shutdown,
        Self::Creating,
        Self::Booting,
        Self::ShuttingDown,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Booted => "Booted",
            Self::Shutdown => "Shutdown",
            Self::Creating => "Creating",
            Self::Booting => "Booting",
            Self::ShuttingDown => "ShuttingDown",
        }
    }
    /// The `@SerializedName`, which simctl prints (`Shutting Down` with a space).
    pub fn serialized_name(self) -> &'static str {
        match self {
            Self::ShuttingDown => "Shutting Down",
            other => other.name(),
        }
    }
    /// Gson's enum adapter: the serialized name first, then `toString()` (the constant name).
    fn from_gson(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|state| state.serialized_name() == text)
            .or_else(|| Self::ALL.into_iter().find(|state| state.name() == text))
    }
}

/// `deviceTypeIdentifier` is None when the device type went missing in an Xcode migration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimctlDevice {
    pub runtime: Option<String>,
    pub state: SimctlDeviceState,
    pub name: String,
    pub udid: String,
    pub log_path: Option<String>,
    pub is_available: Option<bool>,
    pub availability_error: Option<String>,
    pub data_path: Option<String>,
    pub device_type_identifier: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimctlDeviceList {
    pub devices: Vec<SimctlDevice>,
}

/// Gson fills these by reflection, so a missing key is None despite the non-null Kotlin types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimctlDeviceType {
    pub name: Option<String>,
    pub identifier: Option<String>,
}

/// A8: Gson ignores the `@JsonProperty` annotations, so the keys are the Kotlin field names. simctl's
/// `bundlePath`/`buildversion`/`availability` never reach `bundle`/`build_version`/`is_available`;
/// a modern runtime's boolean `isAvailable` arrives as the string `"true"`/`"false"`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimctlRuntime {
    pub bundle: Option<String>,
    pub build_version: Option<String>,
    pub is_available: Option<String>,
    pub runtime_root: Option<String>,
    pub name: Option<String>,
    pub identifier: Option<String>,
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimctlListDevicesOutput {
    pub devicetypes: Vec<SimctlDeviceType>,
    pub runtimes: Vec<SimctlRuntime>,
    pub devices: SimctlDeviceList,
}

/// The output is not a shape this decoder supports; the caller reports ServicesUnavailable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimctlDecodeError(pub String);
impl fmt::Display for SimctlDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for SimctlDecodeError {}

type Result<T> = std::result::Result<T, SimctlDecodeError>;

fn error(message: impl Into<String>) -> SimctlDecodeError {
    SimctlDecodeError(message.into())
}

/// A JSON object in document order; a repeated key replaces the value at its first position, as Gson's
/// `LinkedTreeMap` and reflective field assignment both do.
struct Object(Vec<(String, Box<RawValue>)>);
impl Object {
    fn get(&self, key: &str) -> Option<&RawValue> {
        self.0.iter().find(|(name, _)| name == key).map(|(_, value)| &**value)
    }
}
impl<'de> Deserialize<'de> for Object {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Object;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Object, A::Error> {
                let mut entries: Vec<(String, Box<RawValue>)> = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
                    match entries.iter_mut().find(|(name, _)| *name == key) {
                        Some(entry) => entry.1 = value,
                        None => entries.push((key, value)),
                    }
                }
                Ok(Object(entries))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}
struct Array(Vec<Box<RawValue>>);
impl<'de> Deserialize<'de> for Array {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct ArrayVisitor;
        impl<'de> Visitor<'de> for ArrayVisitor {
            type Value = Array;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON array")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Array, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element::<Box<RawValue>>()? {
                    items.push(item);
                }
                Ok(Array(items))
            }
        }
        deserializer.deserialize_seq(ArrayVisitor)
    }
}

fn object(raw: &RawValue, what: &str) -> Result<Object> {
    serde_json::from_str(raw.get()).map_err(|e| error(format!("{what}: {e}")))
}
fn array(raw: &RawValue, what: &str) -> Result<Array> {
    serde_json::from_str(raw.get()).map_err(|e| error(format!("{what}: {e}")))
}
fn is_null(raw: &RawValue) -> bool {
    raw.get() == "null"
}

/// A JSON primitive as text: Gson's string adapter and `JsonPrimitive.getAsString` both give a number's
/// literal text and a boolean's `true`/`false`. None for objects/arrays.
fn primitive_text(raw: &RawValue) -> Option<String> {
    let text = raw.get();
    match text.as_bytes().first() {
        Some(b'"') => serde_json::from_str(text).ok(),
        Some(b'{' | b'[') | Some(b'n') | None => None,
        Some(_) => Some(text.to_owned()),
    }
}

/// A reflective `String` field: missing or null is None, as Gson leaves it.
fn gson_string(object: &Object, key: &str, what: &str) -> Result<Option<String>> {
    match object.get(key) {
        None => Ok(None),
        Some(raw) if is_null(raw) => Ok(None),
        Some(raw) => primitive_text(raw)
            .map(Some)
            .ok_or_else(|| error(format!("{what}.{key}: expected a string"))),
    }
}

/// `deviceJson.get(key)?.asString` in the custom adapter. A JSON null throws in Gson; P2 reports it.
fn tree_string(object: &Object, key: &str) -> Result<Option<String>> {
    match object.get(key) {
        None => Ok(None),
        Some(raw) => primitive_text(raw)
            .map(Some)
            .ok_or_else(|| error(format!("device.{key}: expected a primitive"))),
    }
}

/// `JsonPrimitive.getAsBoolean`: a boolean, otherwise `Boolean.parseBoolean` of the text.
fn tree_boolean(object: &Object, key: &str) -> Result<Option<bool>> {
    match object.get(key) {
        None => Ok(None),
        Some(raw) => match raw.get() {
            "true" => Ok(Some(true)),
            "false" => Ok(Some(false)),
            _ => primitive_text(raw)
                .map(|text| Some(text.eq_ignore_ascii_case("true")))
                .ok_or_else(|| error(format!("device.{key}: expected a primitive"))),
        },
    }
}

fn list<T>(object: &Object, key: &str, item: impl Fn(&Object) -> Result<T>) -> Result<Vec<T>> {
    let raw = object
        .get(key)
        .filter(|raw| !is_null(raw))
        .ok_or_else(|| error(format!("{key}: missing")))?;
    array(raw, key)?
        .0
        .iter()
        .map(|element| item(&object_of(element, key)?))
        .collect()
}
fn object_of(raw: &RawValue, what: &str) -> Result<Object> {
    if is_null(raw) {
        return Err(error(format!("{what}: null element")));
    }
    object(raw, what)
}

/// Port of `SimctlDeviceListDeserializer`.
fn device_list(raw: &RawValue) -> Result<SimctlDeviceList> {
    let groups = object_of(raw, "devices")?;
    let mut devices = Vec::new();
    for (runtime, group) in &groups.0 {
        for element in array(group, "devices group")?.0 {
            let device = object(&element, "device")?;
            let state = device.get("state");
            let name = tree_string(&device, "name")?;
            let udid = tree_string(&device, "udid")?;
            let log_path = tree_string(&device, "logPath")?;
            let is_available = tree_boolean(&device, "isAvailable")?;
            let device_type_identifier = tree_string(&device, "deviceTypeIdentifier")?;
            // A missing state, name or udid drops the device; a null or unknown state is Unknown.
            let (Some(state), Some(name), Some(udid)) = (state, name, udid) else {
                continue;
            };
            let state = primitive_text(state)
                .filter(|_| state.get().starts_with('"') || !matches!(state.get(), "true" | "false"))
                .and_then(|text| SimctlDeviceState::from_gson(&text))
                .unwrap_or(SimctlDeviceState::Unknown);
            devices.push(SimctlDevice {
                runtime: Some(runtime.clone()),
                state,
                name,
                udid,
                log_path,
                is_available,
                availability_error: None,
                data_path: None,
                device_type_identifier,
            });
        }
    }
    Ok(SimctlDeviceList { devices })
}

/// `gson.fromJson(json, SimctlListDevicesOutput::class.java)` for the supported shapes.
pub fn decode_list_devices_output(json: &str) -> Result<SimctlListDevicesOutput> {
    let root: Object = serde_json::from_str(json).map_err(|e| error(format!("output: {e}")))?;
    let devices = root
        .get("devices")
        .filter(|raw| !is_null(raw))
        .ok_or_else(|| error("devices: missing"))?;
    Ok(SimctlListDevicesOutput {
        devicetypes: list(&root, "devicetypes", |o| {
            Ok(SimctlDeviceType {
                name: gson_string(o, "name", "devicetype")?,
                identifier: gson_string(o, "identifier", "devicetype")?,
            })
        })?,
        runtimes: list(&root, "runtimes", |o| {
            let field = |key| gson_string(o, key, "runtime");
            Ok(SimctlRuntime {
                bundle: field("bundle")?,
                build_version: field("buildVersion")?,
                is_available: field("isAvailable")?,
                runtime_root: field("runtimeRoot")?,
                name: field("name")?,
                identifier: field("identifier")?,
                version: field("version")?,
            })
        })?,
        devices: device_list(devices)?,
    })
}
