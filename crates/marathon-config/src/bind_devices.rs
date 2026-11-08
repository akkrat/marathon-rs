//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/configuration/Marathondevices.kt,
//! vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/configuration/AppleTarget.kt,
//! vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/configuration/Transport.kt and
//! vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/IosVendor.kt:31-41
//! (the P2 mapper construction, identical in MacosVendor.kt).
//!
//! Hand-written `bind_*` schema functions over the WP 2.1 binder under
//! `Profile::Marathondevices`: the mapper registers no `SerializeModule`
//! and no JavaTimeModule, so `File` is the standard Jackson deserializer
//! (spelling trimmed and kept verbatim; F4) and `instant`/`duration`
//! reject — both already the binder's non-P1 behaviour. Defaults live
//! here, never in the types. Null collection elements are rejected (D9,
//! WP 2.2 S4), where Kotlin keeps them (corpus rows
//! `workers-null-element` / `worker-devices-null-element`).

use crate::{
    AppleTarget, MarathonDevices, SimulatorProfile, Transport, Worker,
    bind::{list_non_null, singleton},
    bind_vendor::bind_ssh_authentication,
    binder::{Field, MISMATCHED, Result, Value},
    vendor::SshAuthentication,
};

/// A non-null composite behind `nullable`, failing as a creator parameter
/// when the whole value is null, as in `bind_vendor`'s `bind_object` use.
fn non_null<T>(value: &Value<'_>, bound: Result<Option<T>>) -> Result<T> {
    bound.and_then(|bound| bound.ok_or_else(|| value.error(MISMATCHED, "non-null parameter is null")))
}

pub fn bind_marathon_devices(value: Value<'_>) -> Result<Option<MarathonDevices>> {
    value.nullable(|v| {
        let mut workers = Field::reference(&v, None::<Vec<Worker>>);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "workers" => {
                    let bound = list_non_null(&child, bind_worker)?;
                    workers.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(MarathonDevices {
            workers: workers.required("workers")?,
        })
    })
}

pub fn bind_worker(value: Value<'_>) -> Result<Option<Worker>> {
    value.nullable(|v| {
        let mut transport = Field::reference(&v, None::<Transport>);
        let mut devices = Field::reference(&v, None::<Vec<AppleTarget>>);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "transport" => {
                    let bound = bind_transport(child.clone())?;
                    transport.set(child, bound);
                }
                "devices" => {
                    let bound = list_non_null(&child, bind_apple_target)?;
                    devices.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(Worker {
            transport: transport.required("transport")?,
            devices: devices.required("devices")?,
        })
    })
}

pub fn bind_transport(value: Value<'_>) -> Result<Option<Transport>> {
    value.polymorphic(|id, v| match id {
        "local" => Some(singleton(v).map(|()| Transport::Local)),
        "ssh" => Some(non_null(&v, bind_ssh_transport(v.clone()))),
        _ => None,
    })
}

fn bind_ssh_transport(value: Value<'_>) -> Result<Option<Transport>> {
    value.nullable(|v| {
        let mut addr = Field::reference(&v, None::<String>);
        let mut port = Field::primitive(&v, 22);
        let mut authentication = Field::nullable(&v, None::<SshAuthentication>);
        let mut check_reachability = Field::primitive(&v, true);
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "addr" => {
                    let bound = child.string()?;
                    addr.set(child, bound);
                }
                "port" => {
                    let bound = child.int()?;
                    port.set(child, Some(bound));
                }
                "authentication" => {
                    let bound = bind_ssh_authentication(child.clone())?;
                    authentication.set(child, bound);
                }
                "checkReachability" => {
                    let bound = child.boolean()?;
                    check_reachability.set(child, Some(bound));
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(Transport::Ssh {
            addr: addr.required("addr")?,
            port: port.required("port")?,
            authentication: authentication.optional(),
            check_reachability: check_reachability.required("checkReachability")?,
        })
    })
}

pub fn bind_apple_target(value: Value<'_>) -> Result<Option<AppleTarget>> {
    value.polymorphic(|id, v| match id {
        "simulator" => Some(non_null(
            &v,
            bind_udid(v.clone(), |udid| AppleTarget::Simulator { udid }),
        )),
        "physical" => Some(non_null(
            &v,
            bind_udid(v.clone(), |udid| AppleTarget::Physical { udid }),
        )),
        "simulatorProfile" => Some(non_null(&v, bind_simulator_profile(v.clone()))),
        "host" => Some(singleton(v).map(|()| AppleTarget::Host)),
        _ => None,
    })
}

fn bind_udid(value: Value<'_>, make: impl FnOnce(String) -> AppleTarget) -> Result<Option<AppleTarget>> {
    value
        .nullable(|v| {
            let mut udid = Field::reference(&v, None::<String>);
            let mut object = v.object()?;
            while let Some((name, child)) = object.next_field()? {
                match name.as_str() {
                    "udid" => {
                        let bound = child.string()?;
                        udid.set(child, bound);
                    }
                    _ => child.unknown(false)?,
                }
            }
            udid.required("udid")
        })
        .map(|bound| bound.map(make))
}

fn bind_simulator_profile(value: Value<'_>) -> Result<Option<AppleTarget>> {
    value.nullable(|v| {
        let mut device_type = Field::reference(&v, None::<String>);
        let mut runtime = Field::nullable(&v, None::<String>);
        let mut new_name_prefix = Field::reference(&v, Some(String::from("marathon")));
        let mut object = v.object()?;
        while let Some((name, child)) = object.next_field()? {
            match name.as_str() {
                "deviceType" => {
                    let bound = child.string()?;
                    device_type.set(child, bound);
                }
                "runtime" => {
                    let bound = child.string()?;
                    runtime.set(child, bound);
                }
                "newNamePrefix" => {
                    let bound = child.string()?;
                    new_name_prefix.set(child, bound);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(AppleTarget::SimulatorProfile(SimulatorProfile {
            device_type_id: device_type.required("deviceType")?,
            runtime_id: runtime.optional(),
            new_name_prefix: new_name_prefix.required("newNamePrefix")?,
        }))
    })
}
