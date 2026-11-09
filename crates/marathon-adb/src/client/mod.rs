//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/AndroidDebugBridgeClient.kt` and
//! `.../AndroidDebugBridgeClientFactory.kt`: the adb client marathon talks to every adb server with.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.
//!
//! One TCP connection per request. A request with a serial first switches the connection to that device
//! (`host:transport:<serial>`). Requests that read one result go through
//! [`AndroidDebugBridgeClient::execute`]; streaming ones through
//! [`AndroidDebugBridgeClient::execute_channel`], which returns a pull-based stream that connects on the
//! first pull and closes its connection when dropped (WP 6.1 orchestrator decision 3). The client
//! implements no marathon-level timeouts; 6.5 and 6.6a wrap its futures in the scheduler clock.

pub mod abb;
pub mod bytes;
pub mod consts;
pub mod device;
pub mod error;
pub mod exec;
pub mod feature;
pub mod file_io;
pub mod framebuffer;
pub mod log;
pub mod misc;
pub mod pkg;
pub mod request;
pub mod reverse;
pub mod shell;
pub mod socket;
pub mod stream;
pub mod sync;
pub mod target;
pub mod transform;
pub mod transport;
pub mod validation;

#[cfg(test)]
mod oracle_tests;
#[cfg(test)]
pub(crate) mod stub;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

pub use error::{AdbError, AdbFailure, ConnectError};
pub use feature::Feature;
pub use log::AdamLogging;
pub use request::{AsyncChannelRequest, ComplexRequest, Request};
pub use shell::compat::{AsyncCompatShellCommandRequest, CompatShellStream};
pub use stream::ChannelStream;
pub use target::Target;
pub use transport::{Socket, SocketFactory, SocketWriter};
pub use validation::ValidationResponse;

use consts::DEFAULT_ADB_PORT;
use misc::SetDeviceRequest;
use transport::{DEFAULT_CONNECT_TIMEOUT_MS, DEFAULT_IDLE_TIMEOUT_MS};

#[derive(Clone, Debug)]
pub struct AndroidDebugBridgeClient {
    host: IpAddr,
    host_name: Option<String>,
    port: u16,
    socket_factory: Arc<SocketFactory>,
    logging: Option<AdamLogging>,
    /// Replaces each request's own `socket_idle_timeout`; see [`Self::without_idle_limit`].
    idle_override: Option<i64>,
}

impl AndroidDebugBridgeClient {
    /// Clients may share one factory, as marathon's do (`AdamDeviceProvider.kt:63,91-96`); closing one
    /// client then closes them all.
    pub fn new(host: IpAddr, port: u16, socket_factory: Arc<SocketFactory>, logging: Option<AdamLogging>) -> Self {
        Self {
            host,
            host_name: None,
            port,
            socket_factory,
            logging,
            idle_override: None,
        }
    }

    /// D21: the same server and factory, with no idle limit on any request. For requests whose caller
    /// bounds them with a configured timeout: their silence is legitimate up to that timeout (a
    /// `screenrecord`, `pm install` compiling an APK), so the idle limit must not fire before it does.
    pub fn without_idle_limit(&self) -> Self {
        Self {
            idle_override: Some(transport::NO_IDLE_LIMIT),
            ..self.clone()
        }
    }

    /// The configured host as `InetAddress.getByName(host)` keeps it beside the address: a DNS name is
    /// kept, an IP literal is not (Java's `hostName` stays unset). Connect errors show it as
    /// `name/ip:port`, as Java's `InetSocketAddress` does.
    pub fn with_host_name(mut self, host: &str) -> Self {
        self.host_name = java_host_name(host);
        self
    }

    pub fn host(&self) -> IpAddr {
        self.host
    }

    /// The configured host name, if it is not an IP literal; for logs that name the server.
    pub fn host_name(&self) -> Option<&str> {
        self.host_name.as_deref()
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn socket_factory(&self) -> &Arc<SocketFactory> {
        &self.socket_factory
    }

    fn address(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }

    /// `execute(ComplexRequest, serial)`. Validation fails before anything connects.
    pub async fn execute<R: ComplexRequest>(
        &self,
        mut request: R,
        serial: Option<&str>,
    ) -> Result<R::Output, AdbError> {
        validate(&request)?;
        let mut socket = connect(
            &self.socket_factory,
            self.address(),
            self.host_name.as_deref(),
            self.idle_override.or(request.socket_idle_timeout()),
            self.logging.clone(),
        )
        .await?;
        if let Some(serial) = serial {
            SetDeviceRequest::new(serial).handshake(&mut socket).await?;
        }
        let output = request.process(&mut socket).await?;
        socket.close_result()?;
        Ok(output)
    }

    /// `execute(AsyncChannelRequest, scope, serial)`. Validation fails here; connecting, the transport
    /// switch and the handshake happen on the first pull, and their errors end the stream.
    ///
    /// A request's input channel (shell v2 stdin) is also only written from the first pull on: a sender
    /// that waits for room before the stream is pulled needs a large enough channel or its own task.
    pub fn execute_channel<R: AsyncChannelRequest>(
        &self,
        request: R,
        serial: Option<&str>,
    ) -> Result<ChannelStream<R>, AdbError> {
        validate(&request)?;
        Ok(self.open_channel(request, serial))
    }

    pub(crate) fn open_channel<R: AsyncChannelRequest>(&self, request: R, serial: Option<&str>) -> ChannelStream<R> {
        // adam's producer starts at once and creates the factory's vert.x, so a close() from now on
        // closes the factory even before the first pull connects.
        self.socket_factory.initialize();
        ChannelStream::start(
            self.socket_factory.clone(),
            self.address(),
            self.host_name.clone(),
            self.logging.clone(),
            self.idle_override,
            request,
            serial.map(str::to_string),
        )
    }

    /// `execute(MultiRequest)` for [`AsyncCompatShellCommandRequest`]: shell v2 if the features include
    /// `SHELL_V2`, else shell v1 (PLAN §3.9).
    pub fn execute_compat<R: AsyncCompatShellCommandRequest>(
        &self,
        request: R,
        serial: Option<&str>,
    ) -> CompatShellStream<R> {
        CompatShellStream::start(self, request, serial)
    }

    /// `close()`: closes the socket factory, which every client sharing it notices. Open connections are
    /// closed and read EOF, and those that then complete without an error fail with
    /// `IllegalStateException: Unknown deployment`; later requests fail with [`AdbError::VertxClosed`].
    /// Before the factory's first request it does nothing, as adam's vert.x does not exist yet.
    pub fn close(&self) {
        self.socket_factory.close();
    }
}

fn validate<R: Request + ?Sized>(request: &R) -> Result<(), AdbError> {
    let response = request.validate();
    if response.success {
        return Ok(());
    }
    Err(AdbError::RequestValidation(format!(
        "Request {} did not pass validation: {}",
        request.simple_name(),
        response.message.as_deref().unwrap_or("null")
    )))
}

pub(crate) async fn connect(
    factory: &SocketFactory,
    addr: SocketAddr,
    host_name: Option<&str>,
    idle_timeout_ms: Option<i64>,
    logging: Option<AdamLogging>,
) -> Result<Socket, AdbError> {
    let mut socket = factory.tcp(addr, host_name, None, idle_timeout_ms).await?;
    socket.set_logging(logging);
    Ok(socket)
}

/// `AndroidDebugBridgeClientFactory`.
#[derive(Debug, Default)]
pub struct AndroidDebugBridgeClientFactory {
    pub port: Option<u16>,
    pub host: Option<IpAddr>,
    /// The host as configured, before resolution; see [`AndroidDebugBridgeClient::with_host_name`].
    pub host_name: Option<String>,
    pub socket_factory: Option<Arc<SocketFactory>>,
    pub idle_timeout_ms: Option<i64>,
    pub connect_timeout_ms: Option<i64>,
    pub logging: Option<AdamLogging>,
}

impl AndroidDebugBridgeClientFactory {
    /// Without a port adam asks `DiscoverAdbSocketInteractor`, which is 6.5's; marathon always sets one
    /// (`AdamDeviceProvider.kt:94`), so this falls back to the default port.
    pub fn build(self) -> AndroidDebugBridgeClient {
        let socket_factory = self.socket_factory.unwrap_or_else(|| {
            Arc::new(SocketFactory::new(
                self.connect_timeout_ms.unwrap_or(DEFAULT_CONNECT_TIMEOUT_MS),
                self.idle_timeout_ms.unwrap_or(DEFAULT_IDLE_TIMEOUT_MS),
            ))
        });
        let client = AndroidDebugBridgeClient::new(
            self.host.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            self.port.unwrap_or(DEFAULT_ADB_PORT),
            socket_factory,
            self.logging,
        );
        match &self.host_name {
            Some(name) => client.with_host_name(name),
            None => client,
        }
    }
}

/// `InetAddress.getByName(host).hostName` before any reverse lookup: `None` for an IP literal (IPv6 in
/// brackets or not), else the name as given.
fn java_host_name(host: &str) -> Option<String> {
    let literal = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    literal.parse::<IpAddr>().is_err().then(|| host.to_string())
}

#[cfg(test)]
mod android_debug_bridge_client_test {
    use super::shell::v1::ShellCommandRequest;
    use super::*;
    use crate::client::log::capture;
    use jcompat::logging::Level;

    #[tokio::test]
    async fn test_request_rejection() {
        let server = stub::session(|mut s| async move {
            s.expect_cmd("host:transport:serial").await;
            s.reject("something-something").await;
        })
        .await;
        let error = server
            .client()
            .execute(ShellCommandRequest::new("xx"), Some("serial"))
            .await
            .unwrap_err();
        assert_eq!(error, AdbError::RequestRejected("something-something".into()));
        server.finish().await;
    }

    #[tokio::test]
    async fn test_request_rejection_during_handshake() {
        let server = stub::session(|mut s| async move {
            s.expect_cmd("host:transport:serial").await;
            s.accept().await;
            s.expect_cmd("shell:xx;echo x$?").await;
            s.reject("something-somethingx0").await;
        })
        .await;
        let (logging, events) = capture::logging(true);
        let error = server
            .client_with(Some(logging))
            .execute(ShellCommandRequest::new("xx"), Some("serial"))
            .await
            .unwrap_err();
        assert_eq!(error, AdbError::RequestRejected("something-somethingx0".into()));
        // The framed request, hex length included.
        assert_eq!(
            *events.lock().unwrap(),
            vec![(
                "com.malinskiy.adam.request.Request".to_string(),
                Level::Warn,
                "adb server rejected command 0011shell:xx;echo x$?".to_string()
            )]
        );
        server.finish().await;
    }

    #[tokio::test]
    async fn test_request_rejection_during_handshake_with_no_message() {
        let server = stub::session(|mut s| async move {
            s.expect_cmd("host:transport:serial").await;
            s.accept().await;
            s.expect_cmd("shell:xx;echo x$?").await;
            s.respond(b"FAIL").await;
            s.respond(b"XXXXx").await;
        })
        .await;
        let error = server
            .client()
            .execute(ShellCommandRequest::new("xx"), Some("serial"))
            .await
            .unwrap_err();
        // XXXX is no length, so the message is the trimmed rest of the stream.
        assert_eq!(error, AdbError::RequestRejected("x".into()));
        server.finish().await;
    }

    #[tokio::test]
    async fn test_request_rejection_and_unexpected_message_length() {
        let server = stub::session(|mut s| async move {
            s.expect_cmd("host:transport:serial").await;
            s.respond(b"FAIL").await;
            s.respond(b"XXXXsomething-something").await;
        })
        .await;
        let error = server
            .client()
            .execute(ShellCommandRequest::new("xx"), Some("serial"))
            .await
            .unwrap_err();
        assert_eq!(error, AdbError::RequestRejected("something-something".into()));
        server.finish().await;
    }

    struct Invalid;

    impl Request for Invalid {
        fn serialize(&self) -> Vec<u8> {
            unreachable!("not sent")
        }

        fn validate(&self) -> ValidationResponse {
            ValidationResponse::new(false, Some("Fake".into()))
        }
    }

    impl ComplexRequest for Invalid {
        type Output = String;

        async fn read_element(&mut self, _socket: &mut Socket) -> Result<String, AdbError> {
            unreachable!("not connected")
        }
    }

    #[tokio::test]
    async fn test_request_validation() {
        // Nothing listens on the port: validation fails before connecting.
        let client = AndroidDebugBridgeClientFactory {
            port: Some(1),
            ..Default::default()
        }
        .build();
        let error = client.execute(Invalid, Some("serial")).await.unwrap_err();
        assert_eq!(
            error,
            AdbError::RequestValidation("Request Invalid did not pass validation: Fake".into())
        );
    }

    #[tokio::test]
    async fn connection_closed_during_handshake_is_an_empty_rejection() {
        // EOF instead of OKAY: the short read is "not OKAY", and the message read after it is empty
        // (known bug N15), not "no message received".
        let server = stub::session(|mut s| async move {
            s.expect_cmd("host:version").await;
            s.respond(b"OK").await;
        })
        .await;
        let error = server
            .client()
            .execute(super::misc::GetAdbServerVersionRequest, None)
            .await
            .unwrap_err();
        assert_eq!(error, AdbError::RequestRejected(String::new()));
        server.finish().await;
    }

    #[tokio::test]
    async fn oversized_request_is_refused_locally() {
        let server = stub::session(|mut s| async move {
            s.expect_cmd("host:transport:serial").await;
            s.accept().await;
            // Nothing else arrives: the client refuses before writing (P5).
            assert!(s.read_to_end().await.is_empty());
        })
        .await;
        let error = server
            .client()
            .execute(ShellCommandRequest::new("y".repeat(0x10000)), Some("serial"))
            .await
            .unwrap_err();
        assert!(matches!(error, AdbError::RequestRejected(_)), "{error}");
        server.finish().await;
    }

    /// The client's futures and streams can move between threads (`lifecycle.md`).
    #[test]
    fn ip_literals_keep_no_host_name() {
        assert_eq!(java_host_name("127.0.0.1"), None);
        assert_eq!(java_host_name("::1"), None);
        assert_eq!(java_host_name("[::1]"), None);
        assert_eq!(java_host_name("localhost").as_deref(), Some("localhost"));
        assert_eq!(java_host_name("adb.example").as_deref(), Some("adb.example"));
        let client = AndroidDebugBridgeClientFactory {
            host_name: Some("localhost".into()),
            ..Default::default()
        }
        .build();
        assert_eq!(client.host_name(), Some("localhost"));
    }

    #[test]
    fn futures_and_streams_are_send() {
        fn send<T: Send>(_: &T) {}
        let client = AndroidDebugBridgeClientFactory::default().build();
        send(&client.execute(ShellCommandRequest::new("x"), Some("s")));
        let mut stream = client
            .execute_channel(super::shell::v2::ChanneledShellCommandRequest::new("x", None), None)
            .unwrap();
        send(&stream.next());
        send(&stream);
        let mut compat = client.execute_compat(super::shell::compat::CompatShellCommandRequest::new("x", vec![]), None);
        send(&compat.next());
    }

    #[tokio::test]
    async fn requests_after_close_fail_with_vertx_closed() {
        let client = AndroidDebugBridgeClientFactory {
            port: Some(1),
            ..Default::default()
        }
        .build();
        // The first request creates adam's vert.x, refused or not.
        assert!(
            client
                .execute(super::misc::GetAdbServerVersionRequest, None)
                .await
                .is_err()
        );
        client.close();
        let error = client
            .execute(super::misc::GetAdbServerVersionRequest, None)
            .await
            .unwrap_err();
        assert_eq!(error, AdbError::VertxClosed);
        assert!(!error.is_exception());
        let mut stream = client
            .execute_channel(super::device::AsyncDeviceMonitorRequest, None)
            .unwrap();
        assert_eq!(stream.next().await, Some(Err(AdbError::VertxClosed)));
        assert!(stream.next().await.is_none());
    }

    /// R1-sol-5: closing a factory that never made a request closes nothing (JVM probe
    /// `close-before-first-request`).
    #[tokio::test]
    async fn closing_an_unused_factory_does_nothing() {
        let server = stub::session(|mut s| async move {
            s.expect_cmd("host:version").await;
            s.accept().await;
            s.respond(b"00040029").await;
        })
        .await;
        let client = server.client();
        client.close();
        assert!(!client.socket_factory().is_closed());
        assert_eq!(
            client.execute(super::misc::GetAdbServerVersionRequest, None).await,
            Ok(41)
        );
        server.finish().await;
        // Now it was used, so closing it counts.
        client.close();
        assert!(client.socket_factory().is_closed());
    }

    /// A stream not yet pulled has already used the factory, as adam's producer starts at once.
    #[tokio::test]
    async fn an_unpulled_stream_has_used_the_factory() {
        let client = AndroidDebugBridgeClientFactory {
            port: Some(1),
            ..Default::default()
        }
        .build();
        let mut stream = client
            .execute_channel(super::device::AsyncDeviceMonitorRequest, None)
            .unwrap();
        client.close();
        assert_eq!(stream.next().await, Some(Err(AdbError::VertxClosed)));
    }
}
