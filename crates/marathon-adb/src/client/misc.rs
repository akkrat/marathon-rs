//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/misc/GetAdbServerVersionRequest.kt`
//! and `.../request/misc/SetDeviceRequest.kt`.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.

use super::error::AdbError;
use super::request::{ComplexRequest, Request, create_base_request};
use super::target::Target;
use super::transport::Socket;

/// `host:version`: the server's protocol version (`adb.h` `ADB_SERVER_VERSION`).
#[derive(Clone, Debug, Default)]
pub struct GetAdbServerVersionRequest;

impl Request for GetAdbServerVersionRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(&Target::Host, "version")
    }
}

impl ComplexRequest for GetAdbServerVersionRequest {
    type Output = i32;

    async fn read_element(&mut self, socket: &mut Socket) -> Result<i32, AdbError> {
        let version = socket.read_optional_protocol_string().await;
        // Kotlin toIntOrNull(16), ASCII digits only (P7).
        i32::from_str_radix(&version, 16).map_err(|_| AdbError::RequestRejected("Empty/corrupt response".into()))
    }
}

/// `host:transport:<serial>`: switches the connection to a device. The client sends it before a request
/// that has a serial.
#[derive(Clone, Debug)]
pub struct SetDeviceRequest {
    serial: String,
}

impl SetDeviceRequest {
    pub fn new(serial: impl Into<String>) -> Self {
        Self { serial: serial.into() }
    }
}

impl Request for SetDeviceRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(&Target::Host, &format!("transport:{}", self.serial))
    }
}

#[cfg(test)]
mod get_adb_server_version_request_test {
    use super::*;
    use crate::client::stub;

    #[tokio::test]
    async fn test_returns_proper_version() {
        let server = stub::session(|mut s| async move {
            s.expect_cmd("host:version").await;
            s.accept().await;
            s.respond_string_v1(&format!("{:x}", 41)).await;
        })
        .await;
        let version = server.client().execute(GetAdbServerVersionRequest, None).await.unwrap();
        assert_eq!(version, 41);
        server.finish().await;
    }
}
