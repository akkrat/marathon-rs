//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/shell/v1/{ShellCommandRequest,
//! SyncShellCommandRequest,ShellCommandResult,ShellResultResponseTransformer,
//! ChanneledShellCommandRequest}.kt`.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.
//!
//! Shell v1 has no exit codes, so the synchronous request appends `;echo x$?` and parses the code back
//! out of the output. marathon's synchronous shell is always this one, even on shell_v2 devices.

use std::collections::VecDeque;

use super::Utf8Stream;
use crate::client::consts::MAX_FILE_PACKET_LENGTH;
use crate::client::error::AdbError;
use crate::client::request::{AsyncChannelRequest, ComplexRequest, Request, create_base_request, read_synchronous};
use crate::client::target::Target;
use crate::client::transform::{ResponseTransformer, kotlin_trim};
use crate::client::transport::{Socket, SocketWriter};

/// `SyncShellCommandRequest.EXIT_CODE_DELIMITER`.
pub const EXIT_CODE_DELIMITER: char = 'x';

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellCommandResult {
    pub stdout: Vec<u8>,
    pub exit_code: i32,
}

impl ShellCommandResult {
    /// `output`: stdout as text.
    pub fn output(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// `SyncShellCommandRequest.serialize()`: `shell:<cmd>;echo x$?`. Requests that convert the result (6.5's
/// `GetPropRequest`) reuse it with [`ShellResultResponseTransformer`].
pub fn serialize_sync_shell(target: &Target, cmd: &str) -> Vec<u8> {
    create_base_request(target, &format!("shell:{cmd};echo {EXIT_CODE_DELIMITER}$?"))
}

/// The output up to the last `x` is stdout; the rest, trimmed, is the exit code.
#[derive(Debug, Default)]
pub struct ShellResultResponseTransformer {
    bytes: Vec<u8>,
}

impl ResponseTransformer for ShellResultResponseTransformer {
    type Output = ShellCommandResult;

    fn process(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn transform(&mut self) -> Result<ShellCommandResult, AdbError> {
        let output = String::from_utf8_lossy(&self.bytes).into_owned();
        let Some(index) = output.rfind(EXIT_CODE_DELIMITER) else {
            return Err(AdbError::RequestRejected(format!(
                "No exit code delimiter found in {output}"
            )));
        };
        let stdout = &output[..index];
        let exit_code = kotlin_trim(&output[index + 1..]);
        // Kotlin toIntOrNull(), ASCII digits only (P7).
        let exit_code = exit_code
            .parse::<i32>()
            .map_err(|_| AdbError::RequestRejected(format!("Unexpected exit code value {exit_code}")))?;
        // stdout is re-encoded from the decoded text, as adam does, so it is not the raw bytes.
        Ok(ShellCommandResult {
            stdout: stdout.as_bytes().to_vec(),
            exit_code,
        })
    }
}

/// `ShellCommandRequest`: runs a command and waits for its output and exit code.
#[derive(Debug)]
pub struct ShellCommandRequest {
    cmd: String,
    target: Target,
    transformer: ShellResultResponseTransformer,
}

impl ShellCommandRequest {
    pub fn new(cmd: impl Into<String>) -> Self {
        Self {
            cmd: cmd.into(),
            target: Target::NonSpecified,
            transformer: ShellResultResponseTransformer::default(),
        }
    }
}

impl Request for ShellCommandRequest {
    fn serialize(&self) -> Vec<u8> {
        serialize_sync_shell(&self.target, &self.cmd)
    }
}

impl ComplexRequest for ShellCommandRequest {
    type Output = ShellCommandResult;

    async fn read_element(&mut self, socket: &mut Socket) -> Result<ShellCommandResult, AdbError> {
        read_synchronous(socket, &mut self.transformer).await
    }
}

/// One v1 read: at most 64 KiB, `None` at the end of the stream.
async fn read_chunk(socket: &mut Socket) -> Option<Vec<u8>> {
    let mut data = vec![0u8; MAX_FILE_PACKET_LENGTH];
    let count = socket.read_available(&mut data).await;
    if count < 0 {
        return None;
    }
    data.truncate(count as usize);
    Some(data)
}

/// `ChanneledShellCommandRequest` (v1): the command's output as text, one item per read. logcat uses it on
/// every device.
///
/// adam decodes each read on its own; here a character split across reads is kept whole (D11).
#[derive(Debug)]
pub struct ChanneledShellCommandRequest {
    cmd: String,
    target: Target,
    socket_idle_timeout: Option<i64>,
    decoder: Utf8Stream,
}

impl ChanneledShellCommandRequest {
    pub fn new(cmd: impl Into<String>) -> Self {
        Self::with(cmd, Target::NonSpecified, None)
    }

    pub fn with(cmd: impl Into<String>, target: Target, socket_idle_timeout: Option<i64>) -> Self {
        Self {
            cmd: cmd.into(),
            target,
            socket_idle_timeout,
            decoder: Utf8Stream::default(),
        }
    }
}

impl Request for ChanneledShellCommandRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(&self.target, &format!("shell:{}", self.cmd))
    }

    fn socket_idle_timeout(&self) -> Option<i64> {
        self.socket_idle_timeout
    }
}

impl AsyncChannelRequest for ChanneledShellCommandRequest {
    type Item = String;
    type Input = ();

    async fn read_element(&mut self, socket: &mut Socket, send: &mut VecDeque<String>) -> Result<bool, AdbError> {
        let Some(data) = read_chunk(socket).await else {
            return Ok(true);
        };
        let text = self.decoder.push(&data);
        if !text.is_empty() {
            send.push_back(text);
        }
        Ok(false)
    }

    async fn write_element(_element: (), _socket: &SocketWriter) -> Result<(), AdbError> {
        Ok(())
    }

    async fn close(&mut self, send: &mut VecDeque<String>) -> Result<(), AdbError> {
        let rest = self.decoder.finish();
        if !rest.is_empty() {
            send.push_back(rest);
        }
        Ok(())
    }
}

/// The v1 stream as raw reads, for the compat stream, which hands the bytes on instead of decoding and
/// re-encoding them (D11).
#[derive(Debug)]
pub(crate) struct RawChanneledShellCommandRequest {
    cmd: String,
    target: Target,
    socket_idle_timeout: Option<i64>,
}

impl RawChanneledShellCommandRequest {
    pub(crate) fn new(cmd: String, target: Target, socket_idle_timeout: Option<i64>) -> Self {
        Self {
            cmd,
            target,
            socket_idle_timeout,
        }
    }
}

impl Request for RawChanneledShellCommandRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(&self.target, &format!("shell:{}", self.cmd))
    }

    fn socket_idle_timeout(&self) -> Option<i64> {
        self.socket_idle_timeout
    }

    fn simple_name(&self) -> &str {
        "ChanneledShellCommandRequest"
    }
}

impl AsyncChannelRequest for RawChanneledShellCommandRequest {
    type Item = Vec<u8>;
    type Input = ();

    async fn read_element(&mut self, socket: &mut Socket, send: &mut VecDeque<Vec<u8>>) -> Result<bool, AdbError> {
        match read_chunk(socket).await {
            Some(data) => {
                send.push_back(data);
                Ok(false)
            }
            None => Ok(true),
        }
    }

    async fn write_element(_element: (), _socket: &SocketWriter) -> Result<(), AdbError> {
        Ok(())
    }
}

#[cfg(test)]
mod shell_command_request_test {
    use super::*;
    use crate::client::stub;

    async fn run(response: &'static str) -> ShellCommandResult {
        let server = stub::session(move |mut s| async move {
            s.expect_cmd("host:transport:serial").await;
            s.accept().await;
            s.expect_cmd("shell:xx;echo x$?").await;
            s.accept().await;
            s.respond(response.as_bytes()).await;
        })
        .await;
        let output = server
            .client()
            .execute(ShellCommandRequest::new("xx"), Some("serial"))
            .await
            .unwrap();
        server.finish().await;
        output
    }

    #[tokio::test]
    async fn test_returns_proper_content() {
        let output = run("something-somethingx1").await;
        assert_eq!(output.output(), "something-something");
        assert_eq!(output.exit_code, 1);
    }

    #[tokio::test]
    async fn test_returns_non_stripped_stdout() {
        let output = run("something-something\nx1").await;
        assert_eq!(output.output(), "something-something\n");
        assert_eq!(output.exit_code, 1);
    }
}

#[cfg(test)]
mod shell_result_response_transformer_test {
    use super::*;

    fn transform(bytes: &[u8]) -> Result<ShellCommandResult, AdbError> {
        let mut t = ShellResultResponseTransformer::default();
        t.process(bytes);
        t.transform()
    }

    #[test]
    fn test_no_exit_code_delimiter() {
        assert_eq!(
            transform(b"nothing"),
            Err(AdbError::RequestRejected(
                "No exit code delimiter found in nothing".into()
            ))
        );
    }

    #[test]
    fn test_invalid_exit_code() {
        assert_eq!(
            transform(b"nothingxtest"),
            Err(AdbError::RequestRejected("Unexpected exit code value test".into()))
        );
    }

    #[test]
    fn last_delimiter_and_trimmed_code() {
        let r = transform(b"a x bxyz x 255\n").unwrap();
        assert_eq!((r.output(), r.exit_code), ("a x bxyz ".to_string(), 255));
        let r = transform(b"box\nx 255 \n").unwrap();
        assert_eq!((r.output(), r.exit_code), ("box\n".to_string(), 255));
        // Invalid UTF-8 in stdout comes back re-encoded, as U+FFFD.
        let r = transform(b"\xffx0").unwrap();
        assert_eq!(r.stdout, "\u{fffd}".as_bytes());
        // Fullwidth digits, which Kotlin's toIntOrNull would take, are refused (P7).
        assert!(transform("x\u{FF11}".as_bytes()).is_err());
    }
}

#[cfg(test)]
mod channeled_shell_command_request_test {
    use super::*;
    use crate::client::stub;

    #[tokio::test]
    async fn test_returns_proper_content() {
        let server = stub::session(|mut s| async move {
            s.expect_cmd("host:transport:emulator-5554").await;
            s.accept().await;
            s.expect_cmd("shell:logcat -v").await;
            s.accept().await;
            s.respond(b"something-something").await;
            s.respond(b"something2-something2").await;
        })
        .await;
        let mut updates = server
            .client()
            .execute_channel(ChanneledShellCommandRequest::new("logcat -v"), Some("emulator-5554"))
            .unwrap();
        let mut buffer = String::new();
        while let Some(update) = updates.next().await {
            buffer.push_str(&update.unwrap());
        }
        assert_eq!(buffer, "something-somethingsomething2-something2");
        server.finish().await;
    }
}

#[cfg(test)]
mod async_shell_command_request_test {
    use super::*;

    #[test]
    fn test_simple_command() {
        assert_eq!(
            String::from_utf8(ChanneledShellCommandRequest::new("test").serialize()).unwrap(),
            "000Ashell:test"
        );
    }
}
