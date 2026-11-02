//! Port of the `exec:` session of adam 0.5.8
//! `adam/src/main/kotlin/com/malinskiy/adam/request/pkg/StreamingPackageInstallRequest.kt:76-110` (the `cmd`
//! branch) as a request of its own: `exec:<cmd>`, the input written raw, then the reply read to the end
//! and trimmed as `StringResponseTransformer` does. 6.2's install requests are built the same way.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.

use super::consts::POOLED_DEFAULT_BUFFER_SIZE;
use super::error::AdbError;
use super::request::{ComplexRequest, Request, create_base_request};
use super::target::Target;
use super::transform::{ResponseTransformer, StringResponseTransformer};
use super::transport::Socket;

#[derive(Debug)]
pub struct ExecRequest {
    cmd: String,
    stdin: Vec<u8>,
    transformer: StringResponseTransformer,
}

impl ExecRequest {
    pub fn new(cmd: impl Into<String>, stdin: Vec<u8>) -> Self {
        Self {
            cmd: cmd.into(),
            stdin,
            transformer: StringResponseTransformer::default(),
        }
    }
}

impl Request for ExecRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(&Target::NonSpecified, &format!("exec:{}", self.cmd))
    }
}

impl ComplexRequest for ExecRequest {
    type Output = String;

    async fn read_element(&mut self, socket: &mut Socket) -> Result<String, AdbError> {
        if !self.stdin.is_empty() {
            socket.write_fully(&self.stdin).await?;
        }
        socket
            .copy_to(&mut self.transformer, POOLED_DEFAULT_BUFFER_SIZE, None)
            .await;
        self.transformer.transform()
    }
}
