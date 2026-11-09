//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/transport/{Socket,SocketFactory,
//! SuspendCloseable,TransportResponse}.kt` and `.../transport/vertx/{VertxSocket,VertxSocketFactory}.kt`,
//! as semantics only: tokio replaces vert.x, with no buffer pools, record parser or verticle per socket
//! (D11). `BufferFactory.kt` and `VariableSizeRecordParser.kt` are not ported.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.
//!
//! What is kept, because requests observe it:
//! - a read never fails: EOF, a reset, an idle close and [`SocketFactory::close`] all read as end of
//!   stream (vert.x closes the read channel, and `receiveCatching().getOrNull()` turns an error into -1);
//! - `readFully` stops short at EOF without failing, so callers see a short count;
//! - once the connection is closed, by the peer, the idle timer or `close()`, a write fails with
//!   `IllegalStateException` (vert.x closes the channel on EOF, as half-closure is off);
//! - the idle timer runs during a blocked write too, and fails it with netty's
//!   `StacklessClosedChannelException`;
//! - the idle timer and `close()` close the connection itself, so the server sees it go at once;
//! - the idle timeout counts the milliseconds it was given (D21; adam's counted seconds, known bug N12).
//!
//! The connect and idle timers are transport timers (netty's), not the scheduler clock, so they use
//! `tokio::time` directly (WP 6.1 orchestrator decision 4).

use std::io;
use std::net::{Shutdown, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, Weak};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{Mutex, watch};
use tokio::time::Instant;

use super::error::{AdbError, ConnectError};
use super::log::AdamLogging;

/// `AndroidDebugBridgeClientFactory.kt:39-40`: what the client uses when nothing is configured.
pub const DEFAULT_CONNECT_TIMEOUT_MS: i64 = 10_000;
pub const DEFAULT_IDLE_TIMEOUT_MS: i64 = 30_000;

/// `VertxSocketFactory.toTimeoutInt` (`VertxSocketFactory.kt:53-57`): a Java `Long.toInt()`, which wraps,
/// with a negative result replaced by `Int.MAX_VALUE`.
pub fn to_timeout_int(value: i64) -> i32 {
    let int = value as i32;
    if int < 0 { i32::MAX } else { int }
}

/// `Request::socket_idle_timeout` for a request with no idle limit: silent by nature, or bounded by a
/// watchdog of its own. Kotlin's `Long.MAX_VALUE`.
pub const NO_IDLE_LIMIT: i64 = i64::MAX;

/// D21: the idle timeout in the milliseconds it is given. adam passes milliseconds to vert.x 4.4.6
/// `setIdleTimeout(int)`, which counts seconds, so Kotlin's default 30 s lasts 30,000 s (known bug N12, whose
/// silent `screenrecord` now runs on a client without a limit: `AndroidDebugBridgeClient::without_idle_limit`).
/// Zero disables it, as in vert.x; so do [`NO_IDLE_LIMIT`] and a negative value, which Kotlin turned into
/// 68 years.
pub fn idle_timeout(millis: i64) -> Option<Duration> {
    if millis <= 0 || millis == NO_IDLE_LIMIT {
        return None;
    }
    Some(Duration::from_millis(millis as u64))
}

/// netty's `CONNECT_TIMEOUT_MILLIS`, which really is milliseconds; zero means no timeout.
pub fn connect_timeout(millis: i64) -> Option<Duration> {
    match to_timeout_int(millis) {
        0 => None,
        ms => Some(Duration::from_millis(ms as u64)),
    }
}

/// `VertxSocketFactory`: opens one connection per request and closes them all on [`SocketFactory::close`].
/// Dropping it without `close()` leaves its live connections open, as an unclosed vert.x does.
#[derive(Debug)]
pub struct SocketFactory {
    connect_timeout_ms: i64,
    idle_timeout_ms: i64,
    closed: Arc<watch::Sender<bool>>,
    /// adam creates its `vertx` lazily, on the first request, and `close()` before that closes nothing
    /// (`VertxSocketFactory.kt:32-35,47-50`).
    initialized: AtomicBool,
    /// The connections `close()` shuts down. Registration and `close()` share the lock, so a connection
    /// is either shut down or refused.
    live: std::sync::Mutex<Vec<Weak<Shared>>>,
}

impl SocketFactory {
    pub fn new(connect_timeout_ms: i64, idle_timeout_ms: i64) -> Self {
        Self {
            connect_timeout_ms,
            idle_timeout_ms,
            closed: Arc::new(watch::Sender::new(false)),
            initialized: AtomicBool::new(false),
            live: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Kotlin's first use of `vertx`: from now on, `close()` closes the factory.
    pub(crate) fn initialize(&self) {
        self.initialized.store(true, Ordering::Release);
    }

    /// `tcp(socketAddress, connectTimeout, idleTimeout)`: the per-request timeouts, in milliseconds,
    /// override the factory's. `host_name` is the name an `InetSocketAddress` keeps beside the address,
    /// for error messages only.
    pub async fn tcp(
        &self,
        addr: SocketAddr,
        host_name: Option<&str>,
        connect_timeout_ms: Option<i64>,
        idle_timeout_ms: Option<i64>,
    ) -> Result<Socket, AdbError> {
        self.initialize();
        self.check_open()?;
        let connect = TcpStream::connect(addr);
        let result = match connect_timeout(connect_timeout_ms.unwrap_or(self.connect_timeout_ms)) {
            Some(limit) => match tokio::time::timeout(limit, connect).await {
                Ok(r) => r,
                Err(_) => {
                    return Err(AdbError::Connect(ConnectError {
                        timed_out: true,
                        message: format!("connection timed out: {}", socket_address(addr, host_name)),
                    }));
                }
            },
            None => connect.await,
        };
        let stream = result.map_err(|e| connect_error(e, addr, host_name))?;
        let _ = stream.set_nodelay(true);
        let socket = Socket::new(
            stream,
            idle_timeout(idle_timeout_ms.unwrap_or(self.idle_timeout_ms)),
            self.closed.clone(),
        )
        .map_err(|e| AdbError::Io(e.to_string()))?;
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        // A close() that raced the connect wins, as vert.x would refuse the deployment.
        self.check_open()?;
        live.retain(|shared| shared.strong_count() > 0);
        live.push(Arc::downgrade(&socket.writer.0));
        Ok(socket)
    }

    /// `vertx.close()` (`VertxSocketFactory.kt:47-51`), once a request has been made: every live
    /// connection is closed, so its server sees the end at once, its reads end and its writes fail; new
    /// connections fail.
    pub fn close(&self) {
        if !self.initialized.load(Ordering::Acquire) {
            return;
        }
        let live = {
            let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
            self.closed.send_replace(true);
            std::mem::take(&mut *live)
        };
        for shared in live.iter().filter_map(Weak::upgrade) {
            shared.shut_down();
        }
    }

    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    fn check_open(&self) -> Result<(), AdbError> {
        if self.is_closed() {
            // vert.x refuses the socket's deployment once closed (JVM probe `close-new-request`).
            return Err(AdbError::VertxClosed);
        }
        Ok(())
    }
}

/// Connection diagnostics include the configured name when one was resolved.
fn socket_address(addr: SocketAddr, host_name: Option<&str>) -> String {
    host_name.map_or_else(|| addr.to_string(), |name| format!("{name}/{addr}"))
}

fn connect_error(e: io::Error, addr: SocketAddr, host_name: Option<&str>) -> AdbError {
    match e.kind() {
        io::ErrorKind::ConnectionRefused => AdbError::Connect(ConnectError {
            timed_out: false,
            message: format!("Connection refused: {}", socket_address(addr, host_name)),
        }),
        io::ErrorKind::TimedOut => AdbError::Connect(ConnectError {
            timed_out: true,
            message: format!("connection timed out: {}", socket_address(addr, host_name)),
        }),
        _ => AdbError::Io(format!("{e}: {}", socket_address(addr, host_name))),
    }
}

/// The state both halves share: closure and the idle clock.
#[derive(Debug)]
struct Shared {
    wr: Mutex<OwnedWriteHalf>,
    /// A second handle on the connection, to shut it down while a read or write holds the halves.
    handle: std::net::TcpStream,
    closed: AtomicBool,
    idle: Option<Duration>,
    start: Instant,
    /// Nanoseconds after `start` of the last read or write.
    last_activity: AtomicU64,
    factory_closed: watch::Receiver<bool>,
}

impl Shared {
    fn touch(&self) {
        let nanos = u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.last_activity.store(nanos, Ordering::Relaxed);
    }

    fn idle_deadline(&self) -> Option<Instant> {
        let idle = self.idle?;
        let last = Duration::from_nanos(self.last_activity.load(Ordering::Relaxed));
        self.start.checked_add(last)?.checked_add(idle)
    }

    /// Closed by the peer, the idle timer or the factory. The idle timer is checked lazily, by pending
    /// reads and writes and before each write: netty closes the channel when it fires, so anything later
    /// sees it closed.
    fn is_closed(&self) -> bool {
        if self.closed.load(Ordering::Relaxed) {
            return true;
        }
        let expired = self.idle_deadline().is_some_and(|d| Instant::now() >= d);
        if expired || *self.factory_closed.borrow() {
            self.shut_down();
            return true;
        }
        false
    }

    /// netty's channel close: the server sees the end now, not when the request drops the socket.
    fn shut_down(&self) {
        self.closed.store(true, Ordering::Relaxed);
        // Fails only if the connection is already gone.
        let _ = self.handle.shutdown(Shutdown::Both);
    }
}

/// One adb connection (`transport/Socket.kt`). Dropping it closes the connection, which stands in for the
/// Kotlin `use {}` close.
#[derive(Debug)]
pub struct Socket {
    rd: OwnedReadHalf,
    writer: SocketWriter,
    factory_closed: watch::Receiver<bool>,
    eof: bool,
    logging: Option<AdamLogging>,
}

/// The write side of a [`Socket`], shared with a request's input writer (shell v2 stdin).
#[derive(Clone, Debug)]
pub struct SocketWriter(Arc<Shared>);

impl Socket {
    fn new(stream: TcpStream, idle: Option<Duration>, factory: Arc<watch::Sender<bool>>) -> io::Result<Self> {
        // Taken out of tokio and back to duplicate the handle portably; `into_std` keeps it non-blocking.
        let stream = stream.into_std()?;
        let handle = stream.try_clone()?;
        let (rd, wr) = TcpStream::from_std(stream)?.into_split();
        let shared = Shared {
            wr: Mutex::new(wr),
            handle,
            closed: AtomicBool::new(false),
            idle,
            start: Instant::now(),
            last_activity: AtomicU64::new(0),
            factory_closed: factory.subscribe(),
        };
        Ok(Socket {
            rd,
            writer: SocketWriter(Arc::new(shared)),
            factory_closed: factory.subscribe(),
            eof: false,
            logging: None,
        })
    }

    /// adam's loggers are static; here the client hands its logging to each socket, for the requests.
    pub(crate) fn set_logging(&mut self, logging: Option<AdamLogging>) {
        self.logging = logging;
    }

    pub(crate) fn logging(&self) -> Option<&AdamLogging> {
        self.logging.as_ref()
    }

    /// The Kotlin `use {}` close at the end of a request that otherwise succeeded: once the factory is
    /// closed, undeploying the socket's verticle fails with `IllegalStateException: Unknown deployment`
    /// (JVM probe `close-v1-stream`). A request that already failed keeps its own error.
    pub fn close_result(&self) -> Result<(), AdbError> {
        if *self.factory_closed.borrow() {
            return Err(AdbError::IllegalState("Unknown deployment".into()));
        }
        Ok(())
    }

    pub fn writer(&self) -> SocketWriter {
        self.writer.clone()
    }

    pub fn is_closed_for_write(&self) -> bool {
        self.writer.is_closed_for_write()
    }

    /// `isClosedForRead`: a read has met the end of the stream (or a closed connection).
    ///
    /// vert.x also reports the end once it has merely arrived, so a stream whose server closes right after
    /// a message may end cleanly in Kotlin. Here only what a read has seen counts: pulls are not continuous
    /// like Kotlin's producer, and a `track-devices` stream whose server dies or whose client is closed
    /// while it waits must end with an error (known bug N16), as 6.5 relies on.
    pub fn is_closed_for_read(&self) -> bool {
        self.eof
    }

    fn mark_eof(&mut self) {
        self.eof = true;
        // vert.x closes the whole channel at EOF (no half-closure), so writes stop too.
        self.writer.0.closed.store(true, Ordering::Relaxed);
    }

    /// One read of at most `buf.len()` bytes; 0 means the end of the stream.
    async fn read_some(&mut self, buf: &mut [u8]) -> usize {
        if buf.is_empty() {
            return 0;
        }
        loop {
            if self.eof || self.writer.0.is_closed() {
                self.mark_eof();
                return 0;
            }
            let deadline = self.writer.0.idle_deadline();
            let idle = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            };
            let (rd, watch) = (&mut self.rd, &mut self.factory_closed);
            let factory_closed = async {
                // A factory dropped without close() closes nothing, as an unclosed vert.x lives on; its
                // dropped sender must not wake this select! forever after (R1-wonder-1).
                if watch.wait_for(|closed| *closed).await.is_err() {
                    std::future::pending::<()>().await;
                }
            };
            let read = tokio::select! {
                r = rd.read(buf) => r,
                // The deadline may have moved (a write happened); is_closed() at the top decides.
                _ = idle => continue,
                _ = factory_closed => continue,
            };
            match read {
                Ok(0) | Err(_) => {
                    self.mark_eof();
                    return 0;
                }
                Ok(n) => {
                    self.writer.0.touch();
                    return n;
                }
            }
        }
    }

    /// `readAvailable`: the number of bytes read, or -1 at the end of the stream. Never 0 for a non-empty
    /// buffer.
    pub async fn read_available(&mut self, buf: &mut [u8]) -> isize {
        match self.read_some(buf).await {
            0 if !buf.is_empty() => -1,
            n => n as isize,
        }
    }

    /// `readFully`: fills `buf` unless the stream ends first. Returns the number of bytes read; unlike
    /// a Java `readFully`, a short count at EOF is not an error, as in `VertxSocket.kt:125-139`.
    pub async fn read_fully(&mut self, buf: &mut [u8]) -> usize {
        let mut total = 0;
        while total < buf.len() {
            let n = self.read_some(&mut buf[total..]).await;
            if n == 0 {
                break;
            }
            total += n;
        }
        total
    }

    /// `readByte`, or `None` at the end of the stream (where Kotlin returns a stale pooled byte, WP 6.1 P4).
    pub async fn read_byte(&mut self) -> Option<u8> {
        let mut b = [0u8; 1];
        (self.read_fully(&mut b).await == 1).then_some(b[0])
    }

    /// `readIntLittleEndian`, or `None` if the stream ends first (P4, as for [`Socket::read_byte`]).
    pub async fn read_int_little_endian(&mut self) -> Option<i32> {
        let mut b = [0u8; 4];
        (self.read_fully(&mut b).await == 4).then_some(i32::from_le_bytes(b))
    }

    pub async fn write_fully(&self, bytes: &[u8]) -> Result<(), AdbError> {
        self.writer.write_fully(bytes).await
    }

    pub async fn write_byte(&self, value: u8) -> Result<(), AdbError> {
        self.writer.write_fully(&[value]).await
    }

    pub async fn write_int_little_endian(&self, value: i32) -> Result<(), AdbError> {
        self.writer.write_fully(&value.to_le_bytes()).await
    }
}

impl SocketWriter {
    pub fn is_closed_for_write(&self) -> bool {
        self.0.is_closed()
    }

    /// `writeFully`: `IllegalStateException` once the connection is closed (`VertxSocket.kt` `writeFully`).
    /// A write the server does not take in fails when the idle timer fires, as netty closes the channel
    /// under it. Each part written counts as activity; netty counts only the completed write, which
    /// differs only for a write slower than the idle timeout.
    pub async fn write_fully(&self, bytes: &[u8]) -> Result<(), AdbError> {
        let not_connected = || AdbError::IllegalState("Socket write error: socket is not connected CLOSED".into());
        if self.0.is_closed() {
            return Err(not_connected());
        }
        // Whoever holds the lock is bounded by the same idle timer.
        let mut wr = self.0.wr.lock().await;
        if self.0.is_closed() {
            return Err(not_connected());
        }
        let mut written = 0;
        while written < bytes.len() {
            let deadline = self.0.idle_deadline();
            let idle = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                r = wr.write(&bytes[written..]) => match r {
                    Ok(0) => return Err(AdbError::Io(io::Error::from(io::ErrorKind::WriteZero).to_string())),
                    Ok(n) => {
                        written += n;
                        self.0.touch();
                    }
                    Err(e) => return Err(AdbError::Io(e.to_string())),
                },
                // The deadline may have moved (a read happened).
                _ = idle => {
                    if self.0.is_closed() {
                        return Err(AdbError::ClosedChannel);
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn write_byte(&self, value: u8) -> Result<(), AdbError> {
        self.write_fully(&[value]).await
    }

    pub async fn write_int_little_endian(&self, value: i32) -> Result<(), AdbError> {
        self.write_fully(&value.to_le_bytes()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_timeout_is_in_milliseconds_d21() {
        assert_eq!(idle_timeout(30_000), Some(Duration::from_secs(30)));
        assert_eq!(idle_timeout(1), Some(Duration::from_millis(1)));
        assert_eq!(idle_timeout(0), None);
        assert_eq!(idle_timeout(-1), None);
        assert_eq!(idle_timeout(NO_IDLE_LIMIT), None);
        // Long.toInt() no longer wraps: 2^32 + 5 ms is not 5.
        assert_eq!(idle_timeout((1 << 32) + 5), Some(Duration::from_millis((1 << 32) + 5)));
    }

    #[test]
    fn connect_timeout_conversion_keeps_the_int_wrap() {
        assert_eq!(to_timeout_int(30_000), 30_000);
        assert_eq!(to_timeout_int(-1), i32::MAX);
        assert_eq!(to_timeout_int(i64::MAX), i32::MAX);
        // Long.toInt() wraps: 2^32 + 5 becomes 5.
        assert_eq!(to_timeout_int((1 << 32) + 5), 5);
        assert_eq!(connect_timeout(10_000), Some(Duration::from_millis(10_000)));
        assert_eq!(connect_timeout(0), None);
    }

    /// Counts wake-ups.
    struct Wakes(AtomicU64);

    impl std::task::Wake for Wakes {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// R1-wonder-1: a socket whose factory was dropped without `close()` still waits for its peer. The
    /// dropped watch sender used to make every read spin: `wait_for` failed at once, and its coop budget
    /// was all that made the read yield, waking itself to run again.
    #[tokio::test]
    async fn dropping_the_factory_leaves_reads_waiting() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let factory = SocketFactory::new(DEFAULT_CONNECT_TIMEOUT_MS, DEFAULT_IDLE_TIMEOUT_MS);
        let mut socket = factory.tcp(addr, None, None, None).await.unwrap();
        let (_peer, _) = listener.accept().await.unwrap();
        drop(factory);
        let wakes = Arc::new(Wakes(AtomicU64::new(0)));
        let waker = std::task::Waker::from(wakes.clone());
        let mut buf = [0u8; 16];
        {
            let mut read = std::pin::pin!(socket.read_available(&mut buf));
            let poll = read.as_mut().poll(&mut std::task::Context::from_waker(&waker));
            assert!(poll.is_pending());
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(
                wakes.0.load(Ordering::Relaxed),
                0,
                "the read woke itself instead of waiting"
            );
        }
        assert!(!socket.is_closed_for_write());
    }

    #[test]
    fn socket_address_keeps_the_host_name() {
        let addr: SocketAddr = "127.0.0.1:5037".parse().unwrap();
        assert_eq!(socket_address(addr, None), "127.0.0.1:5037");
        assert_eq!(socket_address(addr, Some("localhost")), "localhost/127.0.0.1:5037");
    }
    #[test]
    fn socket_addresses_use_native_display_d57() {
        for (ip, expected) in [
            ("127.0.0.1", "127.0.0.1:5037"),
            ("::1", "[::1]:5037"),
            ("2001:db8::a:0:1", "[2001:db8::a:0:1]:5037"),
            ("fe80::1", "[fe80::1]:5037"),
            ("::ffff:10.0.0.1", "[::ffff:10.0.0.1]:5037"),
            ("::", "[::]:5037"),
        ] {
            let address = SocketAddr::new(ip.parse().unwrap(), 5037);
            assert_eq!(socket_address(address, None), expected);
        }
    }
}
