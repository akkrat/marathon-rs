//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/shell/v2/ChanneledShellCommandRequest.kt`
//! and `.../request/shell/v2/MessageType.kt`.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.
//!
//! The shell protocol v2 stream: packets of a one-byte id and a little-endian length, carrying stdout,
//! stderr and the exit code apart. The synchronous v2 request is not ported: marathon never uses it
//! (WP 6.1 P1).

use std::collections::VecDeque;

use tokio::sync::mpsc;

use crate::client::consts::MAX_PACKET_LENGTH;
use crate::client::error::AdbError;
use crate::client::request::{AsyncChannelRequest, Request, create_base_request};
use crate::client::target::Target;
use crate::client::transport::{Socket, SocketWriter};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageType {
    Stdin,
    Stdout,
    Stderr,
    Exit,
    /// Close the subprocess's stdin, if possible.
    CloseStdin,
    /// A window size change (an ASCII version of `struct winsize`).
    WindowSizeChange,
    /// An invalid or unknown packet.
    Invalid,
}

impl MessageType {
    pub fn to_value(self) -> u8 {
        match self {
            MessageType::Stdin => 0,
            MessageType::Stdout => 1,
            MessageType::Stderr => 2,
            MessageType::Exit => 3,
            MessageType::CloseStdin => 4,
            MessageType::WindowSizeChange => 5,
            MessageType::Invalid => 255,
        }
    }

    pub fn of(value: u8) -> MessageType {
        match value {
            0 => MessageType::Stdin,
            1 => MessageType::Stdout,
            2 => MessageType::Stderr,
            3 => MessageType::Exit,
            4 => MessageType::CloseStdin,
            5 => MessageType::WindowSizeChange,
            _ => MessageType::Invalid,
        }
    }
}

/// One piece of output: stdout, stderr or the exit code.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellCommandResultChunk {
    pub stdout: Option<Vec<u8>>,
    pub stderr: Option<Vec<u8>>,
    pub exit_code: Option<i32>,
}

/// stdin for the command, and whether to close it afterwards.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellCommandInputChunk {
    pub stdin: Option<Vec<u8>>,
    pub close: bool,
}

/// `shell,v2,raw:<cmd>`.
///
/// The input channel is written from the first pull of the stream on, so a sender that waits for room
/// before the stream is pulled needs a channel large enough or a task of its own. Once the channel is
/// closed and drained, the stream stops after its current read (known bug N17).
#[derive(Debug)]
pub struct ChanneledShellCommandRequest {
    cmd: String,
    channel: Option<mpsc::Receiver<ShellCommandInputChunk>>,
    target: Target,
    socket_idle_timeout: Option<i64>,
    /// The stdout or stderr packet being read and the bytes still to come: each 16 KiB piece is an item
    /// of its own, handed on as soon as it is read, as adam sends each piece to its channel.
    packet: Option<(MessageType, i32)>,
}

impl ChanneledShellCommandRequest {
    pub fn new(cmd: impl Into<String>, channel: Option<mpsc::Receiver<ShellCommandInputChunk>>) -> Self {
        Self::with(cmd, channel, Target::NonSpecified, None)
    }

    pub fn with(
        cmd: impl Into<String>,
        channel: Option<mpsc::Receiver<ShellCommandInputChunk>>,
        target: Target,
        socket_idle_timeout: Option<i64>,
    ) -> Self {
        Self {
            cmd: cmd.into(),
            channel,
            target,
            socket_idle_timeout,
            packet: None,
        }
    }

    /// One piece of a stdout or stderr packet with `length` bytes left.
    async fn read_piece(
        &mut self,
        socket: &mut Socket,
        send: &mut VecDeque<ShellCommandResultChunk>,
        kind: MessageType,
        length: i32,
    ) -> Result<bool, AdbError> {
        let to_read = MAX_PACKET_LENGTH.min(length as usize);
        let mut data = vec![0u8; to_read];
        let read = socket.read_fully(&mut data).await;
        data.truncate(read);
        if read > 0 {
            send.push_back(if kind == MessageType::Stdout {
                ShellCommandResultChunk {
                    stdout: Some(data),
                    ..Default::default()
                }
            } else {
                ShellCommandResultChunk {
                    stderr: Some(data),
                    ..Default::default()
                }
            });
        }
        if read < to_read {
            return Ok(true);
        }
        let rest = length - to_read as i32;
        if rest > 0 {
            self.packet = Some((kind, rest));
        }
        Ok(false)
    }
}

impl Request for ChanneledShellCommandRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(&self.target, &format!("shell,v2,raw:{}", self.cmd))
    }

    fn socket_idle_timeout(&self) -> Option<i64> {
        self.socket_idle_timeout
    }
}

impl AsyncChannelRequest for ChanneledShellCommandRequest {
    type Item = ShellCommandResultChunk;
    type Input = ShellCommandInputChunk;

    fn take_channel(&mut self) -> Option<mpsc::Receiver<ShellCommandInputChunk>> {
        self.channel.take()
    }

    /// At the end of the stream inside a packet, adam reads stale pooled-buffer bytes; here the bytes
    /// that did arrive are handed on and the stream ends cleanly, with no exit chunk (WP 6.1 P4).
    async fn read_element(
        &mut self,
        socket: &mut Socket,
        send: &mut VecDeque<ShellCommandResultChunk>,
    ) -> Result<bool, AdbError> {
        if let Some((kind, length)) = self.packet.take() {
            return self.read_piece(socket, send, kind, length).await;
        }
        let Some(id) = socket.read_byte().await else {
            return Ok(true);
        };
        match MessageType::of(id) {
            kind @ (MessageType::Stdout | MessageType::Stderr) => {
                let Some(length) = socket.read_int_little_endian().await else {
                    return Ok(true);
                };
                // A signed length: zero or less carries nothing.
                if length > 0 {
                    return self.read_piece(socket, send, kind, length).await;
                }
            }
            MessageType::Exit => {
                // The length is read and ignored, then one byte, sign-extended like Kotlin's
                // `readByte().toInt()`: 128-255 come back as -128..-1 (known bug N13).
                if socket.read_int_little_endian().await.is_none() {
                    return Ok(true);
                }
                let Some(code) = socket.read_byte().await else {
                    return Ok(true);
                };
                send.push_back(ShellCommandResultChunk {
                    exit_code: Some(i32::from(code as i8)),
                    ..Default::default()
                });
                return Ok(true);
            }
            // Any other id is skipped as that one byte, without a length (known bug N14).
            _ => {}
        }
        Ok(false)
    }

    async fn write_element(element: ShellCommandInputChunk, socket: &SocketWriter) -> Result<(), AdbError> {
        if let Some(bytes) = element.stdin {
            socket.write_byte(MessageType::Stdin.to_value()).await?;
            socket.write_int_little_endian(bytes.len() as i32).await?;
            socket.write_fully(&bytes).await?;
        }
        if element.close {
            socket.write_byte(MessageType::CloseStdin.to_value()).await?;
            socket.write_int_little_endian(0).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod channeled_shell_command_request_test {
    use super::*;
    use crate::client::stub;

    #[tokio::test]
    async fn test_returns_proper_content() {
        let server = stub::session(|mut s| async move {
            s.expect_cmd("host:transport:serial").await;
            s.accept().await;
            s.expect_cmd("shell,v2,raw:echo foo; echo bar >&2; exit 17").await;
            s.accept().await;
            s.expect_shell_v2_stdin("cafebabe").await;
            s.expect_shell_v2_stdin_close().await;
            s.respond_shell_v2_stdout("fo").await;
            s.respond_shell_v2_stderr("ba").await;
            s.respond_shell_v2_stdout("o\n").await;
            s.respond_shell_v2_window_size_change().await;
            s.respond_shell_v2_invalid().await;
            s.respond_shell_v2_stderr("r\n").await;
            s.respond_shell_v2_exit(17).await;
            s.discard().await;
        })
        .await;
        let (stdio, channel) = mpsc::channel(1);
        let mut updates = server
            .client()
            .execute_channel(
                ChanneledShellCommandRequest::new("echo foo; echo bar >&2; exit 17", Some(channel)),
                Some("serial"),
            )
            .unwrap();
        // Kotlin sends from Dispatchers.IO while the producer already runs; the writer here starts on the
        // first pull, so the sends get a task of their own.
        let sender = tokio::spawn(async move {
            let _ = stdio
                .send(ShellCommandInputChunk {
                    stdin: Some(b"cafebabe".to_vec()),
                    close: false,
                })
                .await;
            let _ = stdio
                .send(ShellCommandInputChunk {
                    stdin: None,
                    close: true,
                })
                .await;
            // Kotlin's test never closes the channel either.
            stdio
        });
        let (mut stdout, mut stderr, mut exit_code) = (Vec::new(), Vec::new(), -1);
        while let Some(msg) = updates.next().await {
            let msg = msg.unwrap();
            stdout.extend(msg.stdout.unwrap_or_default());
            stderr.extend(msg.stderr.unwrap_or_default());
            if let Some(code) = msg.exit_code {
                exit_code = code;
            }
        }
        assert_eq!(stdout, b"foo\n");
        assert_eq!(stderr, b"bar\n");
        assert_eq!(exit_code, 17);
        drop(sender.await);
        server.finish().await;
    }
}
