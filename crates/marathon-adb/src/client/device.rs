//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/device/ListDevicesRequest.kt`,
//! `.../request/device/AsyncDeviceMonitorRequest.kt` and `.../request/device/FetchDeviceFeaturesRequest.kt`.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.

use std::collections::VecDeque;

use jcompat::jstring;
use jcompat::writers::JavaString;

use super::error::AdbError;
use super::feature::Feature;
use super::request::{AsyncChannelRequest, ComplexRequest, Request, create_base_request};
use super::target::Target;
use super::transport::{Socket, SocketWriter};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub serial: String,
    pub state: DeviceState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceState {
    Offline,
    Bootloader,
    Device,
    Host,
    Recovery,
    Rescue,
    Sideload,
    Unauthorized,
    Authorizing,
    Connecting,
    Unknown,
}

impl DeviceState {
    /// `DeviceState.from`: anything adam doesn't know is `UNKNOWN`.
    pub fn from(value: &str) -> DeviceState {
        match value {
            "offline" => DeviceState::Offline,
            "bootloader" => DeviceState::Bootloader,
            "device" => DeviceState::Device,
            "host" => DeviceState::Host,
            "recovery" => DeviceState::Recovery,
            "rescue" => DeviceState::Rescue,
            "sideload" => DeviceState::Sideload,
            "unauthorized" => DeviceState::Unauthorized,
            "authorizing" => DeviceState::Authorizing,
            "connecting" => DeviceState::Connecting,
            _ => DeviceState::Unknown,
        }
    }
}

/// The body of `host:devices` and of each `host:track-devices` message: Kotlin `lines()`, empty lines
/// dropped, each line `trim()`med and split at tabs into serial and state.
///
/// A line without a tab fails like Kotlin's `split[1]` on the one-element list `split` returns (a
/// `SingletonList`, hence its message), as a typed error instead of an exception (P6).
pub fn parse_devices(text: &str) -> Result<Vec<Device>, AdbError> {
    jstring::lines(&JavaString::from(text))
        .iter()
        .filter(|line| !line.0.is_empty())
        .map(|line| {
            let line = JavaString(jstring::trim(&line.0).to_vec()).to_string_lossy();
            let mut split = line.split('\t');
            let serial = split.next().unwrap_or_default();
            let state = split
                .next()
                .ok_or_else(|| AdbError::MalformedResponse("Index: 1, Size: 1".into()))?;
            Ok(Device {
                serial: serial.to_string(),
                state: DeviceState::from(state),
            })
        })
        .collect()
}

/// `host:devices`.
#[derive(Clone, Debug, Default)]
pub struct ListDevicesRequest;

impl Request for ListDevicesRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(&Target::Host, "devices")
    }
}

impl ComplexRequest for ListDevicesRequest {
    type Output = Vec<Device>;

    async fn read_element(&mut self, socket: &mut Socket) -> Result<Vec<Device>, AdbError> {
        parse_devices(&socket.read_protocol_string().await?)
    }
}

/// `host:track-devices`: the device list on every change, with no idle timeout to speak of.
///
/// When the server goes away, reading the next message fails with
/// `RequestRejectedException("Unexpected string length: ")`, so the stream ends in an error rather than
/// cleanly (known bug N16); it cancels marathon's provider job without restarting (WP 6.5 JVM probe).
#[derive(Clone, Debug, Default)]
pub struct AsyncDeviceMonitorRequest;

impl Request for AsyncDeviceMonitorRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(&Target::Host, "track-devices")
    }

    fn socket_idle_timeout(&self) -> Option<i64> {
        Some(i64::MAX)
    }
}

impl AsyncChannelRequest for AsyncDeviceMonitorRequest {
    type Item = Vec<Device>;
    type Input = ();

    async fn read_element(&mut self, socket: &mut Socket, send: &mut VecDeque<Vec<Device>>) -> Result<bool, AdbError> {
        let data = socket.read_protocol_string().await?;
        send.push_back(parse_devices(&data)?);
        Ok(false)
    }

    async fn write_element(_element: (), _socket: &SocketWriter) -> Result<(), AdbError> {
        Ok(())
    }
}

/// `host-serial:<serial>:features`, sent without a transport switch. Unknown names are dropped.
#[derive(Clone, Debug)]
pub struct FetchDeviceFeaturesRequest {
    target: Target,
}

impl FetchDeviceFeaturesRequest {
    pub fn new(serial: impl Into<String>) -> Self {
        Self {
            target: Target::Serial(serial.into()),
        }
    }
}

impl Request for FetchDeviceFeaturesRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(&self.target, "features")
    }
}

impl ComplexRequest for FetchDeviceFeaturesRequest {
    type Output = Vec<Feature>;

    async fn read_element(&mut self, socket: &mut Socket) -> Result<Vec<Feature>, AdbError> {
        Ok(socket
            .read_protocol_string()
            .await?
            .split(',')
            .filter_map(Feature::of)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_lines() {
        let devices = parse_devices("a\tdevice\r\n\n  b\toffline\textra \rc\twat\n").unwrap();
        assert_eq!(
            devices,
            vec![
                Device {
                    serial: "a".into(),
                    state: DeviceState::Device
                },
                Device {
                    serial: "b".into(),
                    state: DeviceState::Offline
                },
                Device {
                    serial: "c".into(),
                    state: DeviceState::Unknown
                },
            ]
        );
        assert_eq!(parse_devices("").unwrap(), vec![]);
        assert_eq!(
            parse_devices("a device\n"),
            Err(AdbError::MalformedResponse("Index: 1, Size: 1".into()))
        );
        // A blank line survives the emptiness filter, then has no tab once trimmed.
        assert!(parse_devices("  \n").is_err());
    }
}

#[cfg(test)]
mod list_devices_request_test {
    use super::*;
    use crate::client::stub;

    #[tokio::test]
    async fn test_returns_proper_content() {
        let server = stub::session(|mut s| async move {
            s.expect_cmd("host:devices").await;
            s.accept().await;
            let states = [
                ("emulator-5554", "offline"),
                ("emulator-5556", "bootloader"),
                ("emulator-5558", "device"),
                ("emulator-5560", "host"),
                ("emulator-5562", "recovery"),
                ("emulator-5564", "rescue"),
                ("emulator-5566", "sideload"),
                ("emulator-5568", "unauthorized"),
                ("emulator-5570", "authorizing"),
                ("emulator-5572", "connecting"),
                ("emulator-5574", "wtf"),
            ];
            let response: String = states.iter().map(|(s, st)| format!("{s}\t{st}\n")).collect();
            s.respond_string_v1(&response).await;
        })
        .await;
        let devices = server.client().execute(ListDevicesRequest, None).await.unwrap();
        let expected = [
            ("emulator-5554", DeviceState::Offline),
            ("emulator-5556", DeviceState::Bootloader),
            ("emulator-5558", DeviceState::Device),
            ("emulator-5560", DeviceState::Host),
            ("emulator-5562", DeviceState::Recovery),
            ("emulator-5564", DeviceState::Rescue),
            ("emulator-5566", DeviceState::Sideload),
            ("emulator-5568", DeviceState::Unauthorized),
            ("emulator-5570", DeviceState::Authorizing),
            ("emulator-5572", DeviceState::Connecting),
            ("emulator-5574", DeviceState::Unknown),
        ]
        .map(|(serial, state)| Device {
            serial: serial.into(),
            state,
        });
        assert_eq!(devices, expected);
        server.finish().await;
    }
}

#[cfg(test)]
mod async_device_monitor_request_test {
    use super::*;
    use crate::client::stub;

    #[tokio::test]
    async fn test_returns_proper_content() {
        let server = stub::session(|mut s| async move {
            s.expect_cmd("host:track-devices").await;
            s.accept().await;
            s.respond_string_v1("emulator-5554\toffline\n").await;
            s.respond_string_v1("emulator-5554\tdevice\n").await;
        })
        .await;
        let mut updates = server
            .client()
            .execute_channel(AsyncDeviceMonitorRequest, None)
            .unwrap();
        let device = |state| {
            vec![Device {
                serial: "emulator-5554".into(),
                state,
            }]
        };
        assert_eq!(updates.next().await.unwrap().unwrap(), device(DeviceState::Offline));
        assert_eq!(updates.next().await.unwrap().unwrap(), device(DeviceState::Device));
        drop(updates);
        server.finish().await;
    }
}

#[cfg(test)]
mod fetch_device_features_request_test {
    use super::*;
    use crate::client::stub;

    #[test]
    fn test_serialization() {
        let request = FetchDeviceFeaturesRequest::new("cafebabe");
        assert_eq!(
            String::from_utf8(request.serialize()).unwrap(),
            "001Dhost-serial:cafebabe:features"
        );
    }

    #[tokio::test]
    async fn test_response() {
        let socket = stub::StubSocket::new(
            b"0054fixed_push_symlink_timestamp,apex,fixed_push_mkdir,stat_v2,abb_exec,cmd,abb,shell_v2",
        )
        .await;
        let mut request = FetchDeviceFeaturesRequest::new("cafebabe");
        let features = request.read_element(&mut socket.connect().await).await.unwrap();
        assert_eq!(
            features,
            vec![
                Feature::FixedPushSymlinkTimestamp,
                Feature::Apex,
                Feature::FixedPushMkdir,
                Feature::StatV2,
                Feature::AbbExec,
                Feature::Cmd,
                Feature::Abb,
                Feature::ShellV2,
            ]
        );
    }
}
