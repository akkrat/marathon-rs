//! A byte-exact scripted stub for the ported adam tests, standing in for adam's `server-stub`
//! (`AdbServerRule`, `StubSocket`). `FakeAdbServer` is a stateful world and deliberately cannot send
//! malformed replies (WP 6.1 orchestrator decision 5). Test-only.
//!
//! One session serves one connection: the script runs, then the connection closes.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use super::shell::v2::MessageType;
use super::transport::{DEFAULT_CONNECT_TIMEOUT_MS, DEFAULT_IDLE_TIMEOUT_MS, Socket, SocketFactory};
use super::{AdamLogging, AndroidDebugBridgeClient};

pub(crate) struct StubServer {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

/// Serves one connection with `script`.
pub(crate) async fn session<F, Fut>(script: F) -> StubServer
where
    F: FnOnce(StubSession) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the stub");
    let addr = listener.local_addr().expect("stub address");
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        script(StubSession { stream }).await;
    });
    StubServer { addr, task }
}

pub(crate) async fn sessions<F, Fut>(count: usize, script: F) -> StubServer
where
    F: Fn(usize, StubSession) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
    let addr = listener.local_addr().expect("stub address");
    let task = tokio::spawn(async move {
        for index in 0..count {
            let (stream, _) = listener.accept().await.expect("accept");
            script(index, StubSession { stream }).await;
        }
    });
    StubServer { addr, task }
}

impl StubServer {
    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub(crate) fn client(&self) -> AndroidDebugBridgeClient {
        self.client_with(None)
    }

    pub(crate) fn client_with(&self, logging: Option<AdamLogging>) -> AndroidDebugBridgeClient {
        let factory = SocketFactory::new(DEFAULT_CONNECT_TIMEOUT_MS, DEFAULT_IDLE_TIMEOUT_MS);
        AndroidDebugBridgeClient::new(self.addr.ip(), self.addr.port(), Arc::new(factory), logging)
    }

    /// Waits for the script, failing the test if one of its expectations failed.
    pub(crate) async fn finish(self) {
        if let Err(e) = self.task.await
            && e.is_panic()
        {
            std::panic::resume_unwind(e.into_panic());
        }
    }
}

pub(crate) struct StubSession {
    stream: TcpStream,
}

impl StubSession {
    pub(crate) async fn read_exact(&mut self, n: usize) -> Vec<u8> {
        let mut bytes = vec![0; n];
        self.stream.read_exact(&mut bytes).await.expect("stub bytes");
        bytes
    }

    pub(crate) async fn expect_bytes(&mut self, expected: &[u8]) {
        assert_eq!(self.read_exact(expected.len()).await, expected);
    }

    pub(crate) async fn expect_sync(&mut self, tag: &[u8; 4], path: &str) {
        self.expect_bytes(tag).await;
        self.expect_bytes(&(path.len() as u32).to_le_bytes()).await;
        self.expect_bytes(path.as_bytes()).await;
    }

    /// `expectCmd`: the next request is exactly `expected`.
    pub(crate) async fn expect_cmd(&mut self, expected: &str) {
        let mut len = [0u8; 4];
        self.stream.read_exact(&mut len).await.expect("request length");
        let len = usize::from_str_radix(std::str::from_utf8(&len).expect("hex"), 16).expect("hex");
        let mut request = vec![0u8; len];
        self.stream.read_exact(&mut request).await.expect("request");
        assert_eq!(String::from_utf8_lossy(&request), expected);
    }

    pub(crate) async fn accept(&mut self) {
        self.respond(b"OKAY").await;
    }

    /// `reject`: FAIL and the message as a protocol string.
    pub(crate) async fn reject(&mut self, message: &str) {
        self.respond(b"FAIL").await;
        self.respond_string_v1(message).await;
    }

    pub(crate) async fn respond(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.expect("write");
    }

    /// For a client that may already have hung up.
    pub(crate) async fn try_respond(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(bytes).await
    }

    /// `respondStringV1`: the UTF-16 length in four lowercase hex digits, then the text.
    pub(crate) async fn respond_string_v1(&mut self, message: &str) {
        let header = format!("{:04x}", message.encode_utf16().count());
        self.respond(header.as_bytes()).await;
        self.respond(message.as_bytes()).await;
    }

    async fn respond_shell_v2(&mut self, kind: MessageType, text: &str) {
        let mut packet = vec![kind.to_value()];
        packet.extend_from_slice(&(text.len() as i32).to_le_bytes());
        packet.extend_from_slice(text.as_bytes());
        self.respond(&packet).await;
    }

    pub(crate) async fn respond_shell_v2_stdout(&mut self, text: &str) {
        self.respond_shell_v2(MessageType::Stdout, text).await;
    }

    pub(crate) async fn respond_shell_v2_stderr(&mut self, text: &str) {
        self.respond_shell_v2(MessageType::Stderr, text).await;
    }

    /// The stub writes the length big-endian (`writeInt(1)`); the client ignores it.
    pub(crate) async fn respond_shell_v2_exit(&mut self, code: u8) {
        self.respond(&[MessageType::Exit.to_value(), 0, 0, 0, 1, code]).await;
    }

    /// A bare id byte, as the stub sends it.
    pub(crate) async fn respond_shell_v2_window_size_change(&mut self) {
        self.respond(&[MessageType::WindowSizeChange.to_value()]).await;
    }

    pub(crate) async fn respond_shell_v2_invalid(&mut self) {
        self.respond(&[MessageType::Invalid.to_value()]).await;
    }

    pub(crate) async fn expect_shell_v2_stdin(&mut self, expected: &str) {
        assert_eq!(self.stream.read_u8().await.expect("id"), MessageType::Stdin.to_value());
        let size = self.stream.read_i32_le().await.expect("size");
        let mut data = vec![0u8; size as usize];
        self.stream.read_exact(&mut data).await.expect("stdin");
        assert_eq!(String::from_utf8_lossy(&data), expected);
    }

    pub(crate) async fn expect_shell_v2_stdin_close(&mut self) {
        assert_eq!(
            self.stream.read_u8().await.expect("id"),
            MessageType::CloseStdin.to_value()
        );
        assert_eq!(self.stream.read_i32_le().await.expect("size"), 0);
    }

    /// `input.discard()`: reads until the client closes.
    pub(crate) async fn discard(&mut self) {
        let mut sink = Vec::new();
        let _ = self.stream.read_to_end(&mut sink).await;
    }

    /// Reads everything the client sends until it closes, for scripts that check what arrived.
    pub(crate) async fn read_to_end(&mut self) -> Vec<u8> {
        let mut data = Vec::new();
        let _ = self.stream.read_to_end(&mut data).await;
        data
    }
}

/// adam's `StubSocket`: a connection whose input is `content`, for calling a request's `read_element`
/// directly.
pub(crate) struct StubSocket {
    server: StubServer,
}

impl StubSocket {
    pub(crate) async fn new(content: &'static [u8]) -> StubSocket {
        let server = session(move |mut s| async move {
            s.respond(content).await;
        })
        .await;
        StubSocket { server }
    }

    pub(crate) async fn connect(&self) -> Socket {
        SocketFactory::new(DEFAULT_CONNECT_TIMEOUT_MS, DEFAULT_IDLE_TIMEOUT_MS)
            .tcp(self.server.addr, None, None, None)
            .await
            .expect("connect to the stub")
    }
}
