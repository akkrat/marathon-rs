//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/execution/ArgumentsFactory.kt
use marathon_logging::Level;

use crate::{device::AndroidDevice, support::trim};
use jcompat::{jnum, jstring, };
use marathon_adb::{instrument_request::*, testrunner::MetricMap};
use marathon_config::vendor::TestAccessConfiguration;

pub struct ArgumentsFactory<'a> {
    pub device: &'a AndroidDevice,
}
impl ArgumentsFactory<'_> {
    pub fn generate(&self, coverage: bool, args: &[(String, String)], access: &TestAccessConfiguration) -> MetricMap {
        generate(
            self.device.adb_serial(),
            self.device.client().port(),
            self.device.adb_reverse_port(),
            coverage,
            args,
            access,
            |level, text| self.device.log_named("ArgumentsFactory", level, &text.to_owned(), None),
        )
    }
}
/// Pure argument corpus seam; the reverse port is only present for the named adb TCP rule.
pub fn generate(
    serial: &str,
    client_port: u16,
    reverse_port: Option<i32>,
    coverage: bool,
    args: &[(String, String)],
    access: &TestAccessConfiguration,
    mut log: impl FnMut(Level, String),
) -> MetricMap {
    let mut result: MetricMap = args.iter().map(|(k, v)| (k.as_str().into(), v.clone())).collect();
    let local = serial.starts_with("emulator-");
    if access.adb
        && let Some(port) = if local {
            Some(i32::from(client_port))
        } else {
            reverse_port
        }
    {
        result.insert(ADB_PORT.into(), port.to_string().as_str().to_owned());
        result.insert(ADB_HOST.into(), if local { "10.0.2.2" } else { "localhost" }.into());
        result.insert(ADB_SERIAL.into(), serial.into());
    }
    for (enabled, grpc) in [(access.console, false), (access.grpc, true)] {
        if !enabled {
            continue;
        }
        if !local {
            log(
                Level::WARN,
                format!(
                    "Access to emulator {} port is requested, but device {serial} is not a local emulator",
                    if grpc { "gRPC" } else { "console" }
                )
                .as_str()
                .into(),
            );
            log(
                Level::WARN,
                format!("If {serial} is an emulator, connect the adb server instead of `adb connect` for this to work")
                    .as_str()
                    .into(),
            );
        } else if let Some(port) = jnum::parse_int(&trim(&serial[9..])) {
            if grpc {
                let grpc_port = port.wrapping_add(3000);
                log(
                    Level::DEBUG,
                    format!("Assuming gRPC port is console port {port} + 3000 = {grpc_port}")
                        .as_str()
                        .into(),
                );
                result.insert(GRPC_PORT.into(), grpc_port.to_string().as_str().to_owned());
                result.insert(GRPC_HOST.into(), "10.0.2.2".into());
            } else {
                result.insert(CONSOLE_PORT.into(), port.to_string().as_str().to_owned());
                result.insert(CONSOLE_HOST.into(), "10.0.2.2".into());
                if !access.console_token.chars().all(jstring::is_whitespace) {
                    result.insert(AUTH_TOKEN.into(), access.console_token.clone());
                }
            }
        } else {
            log(
                Level::DEBUG,
                format!("Unable to parse emulator console port for serial {serial}")
                    .as_str()
                    .into(),
            );
        }
    }
    if coverage {
        result.insert("coverage".into(), "true".into());
    }
    result
}
