//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/graphite/BasicGraphiteClient.kt
//! and external/graphite/GraphiteTracker.kt, metrics/remote/graphite/QueryableGraphiteClient.kt.
//!
//! BasicGraphiteClient's Kotlin source is adapted from
//! https://github.com/mihkels/graphite-client-kotlin/blob/master/graphite-client/src/main/kotlin/com/mihkels/graphite/client/BasicGraphiteClient.kt.
use crate::http::form_encode;
use crate::{http::*, points};
use jcompat::{file_manager::escape, formatting::format_graphite, jtime::Instant};
use marathon_core::{
    analytics::{
        MetricsContext, TrackerInternalAdapter,
        remote::graphite::{GraphiteQuery, with_prefix},
    },
    error::*,
    executor::shared::failure,
    model::TestStatus,
    report::TestEvent,
    runtime::*,
};
use marathon_error::{AnalyticsKind, FailureKind};
use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpStream, lookup_host},
    time::{Instant as Monotonic, timeout},
};

pub struct GraphiteMetric {
    pub metric_name: String,
    pub metric_value: String,
    pub timestamp: i64,
}
pub trait DataSender: Send + Sync {
    fn send_data<'a>(
        &'a self,
        host: &'a str,
        port: u16,
        data: String,
        cancellation: Arc<dyn CancellationSignal>,
    ) -> ServiceFuture<'a, ()>;

    fn close(&self) {}
}
/// D51: Kotlin has no timeout on either connect or write.
#[derive(Clone, Copy)]
pub struct CarbonLimits {
    /// Name lookup and connect together.
    pub connect: Duration,
    /// Total time allowed for writing one payload, including a retry.
    pub write: Duration,
}
impl Default for CarbonLimits {
    /// 10 s outlasts one dead DNS server (resolv.conf's default is 5 s) and three lost SYNs
    /// (at 1, 3 and 7 s). A send is three short lines, which fit the socket's buffer, so a working
    /// carbon never comes near either limit.
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            write: Duration::from_secs(10),
        }
    }
}
#[derive(Default)]
pub struct SocketSender {
    limits: CarbonLimits,
    cached: Mutex<Option<(String, u16, TcpStream)>>,
}
fn graphite_error(message: String) -> OperationError {
    failure(FailureKind::Analytics(AnalyticsKind::Graphite), &message).into()
}
fn write_error(error: io::Error) -> OperationError {
    graphite_error(format!("Error while writing data to graphite: {error}"))
}
impl SocketSender {
    pub fn new(limits: CarbonLimits) -> Self {
        Self {
            limits,
            ..Self::default()
        }
    }

    fn cached(&self) -> std::sync::MutexGuard<'_, Option<(String, u16, TcpStream)>> {
        self.cached.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn connect(&self, host: &str, port: u16) -> OperationResult<TcpStream> {
        let connect = async {
            let addresses = lookup_host((host, port))
                .await
                .map_err(|_| graphite_error(format!("Unknown host: {host}")))?
                .collect::<Vec<_>>();
            TcpStream::connect(addresses.as_slice()).await.map_err(write_error)
        };
        timeout(self.limits.connect, connect).await.map_err(|_| {
            graphite_error(format!(
                "Error while writing data to graphite: connect timed out after {} ms",
                self.limits.connect.as_millis()
            ))
        })?
    }

    async fn write(
        &self,
        socket: &mut TcpStream,
        remaining: &mut &[u8],
        budget: Duration,
    ) -> OperationResult<io::Result<()>> {
        timeout(budget, socket.write_all_buf(remaining)).await.map_err(|_| {
            graphite_error(format!(
                "Error while writing data to graphite: write timed out after {} ms",
                self.limits.write.as_millis()
            ))
        })
    }
}
impl DataSender for SocketSender {
    fn send_data<'a>(
        &'a self,
        host: &'a str,
        port: u16,
        data: String,
        cancellation: Arc<dyn CancellationSignal>,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            let send = async {
                // The remote delivery task is the single writer. Move the socket out so no lock
                // is held during I/O, and cancellation or a partial write cannot put it back.
                let mut cached = self.cached().take().filter(|(h, p, _)| h == host && *p == port);
                let mut budget = self.limits.write;
                let mut remaining = data.as_bytes();
                if let Some((_, _, socket)) = cached.as_mut() {
                    // Tokio's read readiness may lag a FIN already visible to the kernel.
                    // Carbon sends no reply, so only an empty nonblocking receive queue is safe
                    // to reuse. A FIN arriving after this probe can still race the write.
                    let usable = matches!(
                        socket2::SockRef::from(&*socket).peek(&mut [std::mem::MaybeUninit::uninit()]),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock
                    );
                    if usable {
                        let started = Monotonic::now();
                        match self.write(socket, &mut remaining, budget).await? {
                            Ok(()) => {
                                *self.cached() = cached;
                                return Ok(());
                            }
                            // Replay only if none of this payload reached the socket.
                            Err(error) if remaining.len() != data.len() => return Err(write_error(error)),
                            Err(_) => budget = budget.saturating_sub(started.elapsed()),
                        }
                    }
                }
                drop(cached);
                let mut socket = self.connect(host, port).await?;
                self.write(&mut socket, &mut remaining, budget)
                    .await?
                    .map_err(write_error)?;
                *self.cached() = Some((host.to_owned(), port, socket));
                Ok(())
            };
            // Approved P3: typed cancellation interrupts a stuck carbon write. D51 adds the limits.
            tokio::select! {biased; reason=cancellation.cancelled()=>Err(reason.into()),result=send=>result}
        })
    }
    fn close(&self) {
        self.cached().take();
    }
}
pub struct BasicGraphiteClient {
    pub host: String,
    pub port: u16,
    pub prefix: Option<String>,
    pub sender: Arc<dyn DataSender>,
    pub cancellation: Arc<dyn CancellationSignal>,
}
impl BasicGraphiteClient {
    pub async fn send(&self, metric: GraphiteMetric) -> OperationResult<()> {
        self.send_all(&[metric]).await
    }
    pub async fn send_all(&self, metrics: &[GraphiteMetric]) -> OperationResult<()> {
        if metrics.is_empty() {
            return Ok(());
        }
        let data = metrics
            .iter()
            .map(|m| {
                format!(
                    "{} {} {}\n",
                    with_prefix(&m.metric_name, self.prefix.as_deref()),
                    m.metric_value,
                    m.timestamp / 1000
                )
            })
            .collect::<String>();
        self.sender
            .send_data(&self.host, self.port, data, self.cancellation.clone())
            .await
    }
}
pub struct GraphiteTracker {
    pub client: BasicGraphiteClient,
    pub read_only: bool,
}
impl GraphiteTracker {
    pub async fn track_test(&self, event: &TestEvent) -> OperationResult<()> {
        if self.read_only || !matches!(event.test_result.status, TestStatus::Passed | TestStatus::Failure) {
            return Ok(());
        }
        let result = &event.test_result;
        let test = &result.test;
        let serial = text(&escape(&event.device.serial_number))?;
        let base = format!(
            "tests.{}.{}.{}.{}.{serial}",
            text(&test.to_safe_test_name())?.replace('.', "--"),
            text(&test.pkg)?.replace('.', "--"),
            text(&test.clazz)?,
            text(&test.method)?
        );
        let timestamp = points::millis(event)?;
        let metrics = [
            ("ignored", if result.is_ignored() { "1" } else { "0" }.into()),
            (
                "success",
                if result.status == TestStatus::Passed { "1" } else { "0" }.into(),
            ),
            ("duration", result.end_time.wrapping_sub(result.start_time).to_string()),
        ]
        .map(|(name, value)| GraphiteMetric {
            metric_name: format!("{base}.{name}"),
            metric_value: value,
            timestamp,
        });
        self.client.send_all(&metrics).await
    }
}
impl TrackerInternalAdapter for GraphiteTracker {
    fn test<'a>(&'a self, event: &'a TestEvent) -> VendorFuture<'a, ()> {
        Box::pin(async move { self.track_test(event).await.map_err(vendor) })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async move {
            self.client.sender.close();
            Ok(())
        })
    }
}
pub struct QueryableGraphiteClient {
    // OkHttp parses the URL per call, so an invalid host fails each query
    // (where 3.4a's retries and defaults apply), never construction.
    http: OperationResult<Http>,
}
impl QueryableGraphiteClient {
    pub fn new(host: &str, context: MetricsContext) -> Self {
        Self::with_endpoint(&format!("http://{host}:80"), context)
    }
    /// Internal endpoint seam, retaining production's host-only/port-80 setup.
    pub fn with_endpoint(endpoint: &str, context: MetricsContext) -> Self {
        Self {
            http: Http::new(endpoint, None, context, false),
        }
    }
    pub fn port(&self) -> Option<u16> {
        self.http.as_ref().ok()?.base.port_or_known_default()
    }
}
impl GraphiteQuery for QueryableGraphiteClient {
    fn query<'a>(&'a self, target: &'a str, limit: Instant) -> ServiceFuture<'a, Vec<String>> {
        Box::pin(async move {
            let from = format_graphite(limit).ok_or_else(|| failure(FailureKind::Arithmetic, "long overflow"))?;
            let path = format!("render?target={}&format=raw&from={from}", form_encode(target));
            let http = self.http.as_ref().map_err(Clone::clone)?;
            let reply = http.request("GET", &path, None, None, String::new()).await?;
            Ok(reply
                .body
                .split('\n')
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marathon_core::executor::Signal;
    use tokio::{io::AsyncReadExt, net::TcpListener};

    #[tokio::test(flavor = "current_thread")]
    async fn a_closed_carbon_connection_delivers_the_next_payload_once_u284() -> OperationResult<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(write_error)?;
        let port = listener.local_addr().map_err(write_error)?.port();
        let sender = SocketSender::default();
        let signal = Signal::new();
        timeout(Duration::from_secs(60), async {
            sender
                .send_data("127.0.0.1", port, "before 1 0\n".into(), signal.clone())
                .await?;
            let (mut first, _) = listener.accept().await.map_err(write_error)?;
            let mut before = [0; b"before 1 0\n".len()];
            first.read_exact(&mut before).await.map_err(write_error)?;
            assert_eq!(&before, b"before 1 0\n");
            drop(first);

            // Wait for kernel EOF without polling Tokio's reactor, so its cached read
            // readiness still predates the FIN when the next send starts.
            {
                let cached = sender.cached();
                let (_, _, client) = cached.as_ref().expect("first send retains its connection");
                let socket = socket2::SockRef::from(client);
                let deadline = Monotonic::now() + Duration::from_secs(60);
                loop {
                    match socket.peek(&mut [std::mem::MaybeUninit::uninit()]) {
                        Ok(0) => break,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            assert!(Monotonic::now() < deadline, "kernel did not report Carbon EOF");
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        result => panic!("expected Carbon EOF, got {result:?}"),
                    }
                }
            }

            sender
                .send_data("127.0.0.1", port, "after 2 0\n".into(), signal)
                .await?;
            sender.close();
            let listener = listener.into_std().map_err(write_error)?;
            let (second, _) = listener.accept().map_err(|error| {
                graphite_error(format!(
                    "no replacement Carbon connection after successful second send: {error}"
                ))
            })?;
            second.set_nonblocking(true).map_err(write_error)?;
            let mut second = TcpStream::from_std(second).map_err(write_error)?;
            let mut after = String::new();
            second.read_to_string(&mut after).await.map_err(write_error)?;
            assert_eq!(after, "after 2 0\n");
            assert!(matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
            Ok(())
        })
        .await
        .map_err(|_| graphite_error("Carbon graceful-close witness timed out".into()))?
    }

    #[tokio::test]
    async fn a_reset_carbon_connection_is_reopened_without_losing_the_next_payload_u284() -> OperationResult<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(write_error)?;
        let addr = listener.local_addr().map_err(write_error)?;
        let sender = SocketSender::default();
        let signal = Signal::new();
        timeout(Duration::from_secs(60), async {
            sender
                .send_data("127.0.0.1", addr.port(), "before 1 0\n".into(), signal.clone())
                .await?;
            let (mut first, _) = listener.accept().await.map_err(write_error)?;
            let mut before = [0; b"before 1 0\n".len()];
            first.read_exact(&mut before).await.map_err(write_error)?;
            assert_eq!(&before, b"before 1 0\n");
            first.set_zero_linger().map_err(write_error)?;
            drop(first);
            // Wait for the kernel to report the reset, rather than racing its notification.
            let cached = sender.cached().take().expect("first send retains its connection");
            cached.2.readable().await.map_err(write_error)?;
            *sender.cached() = Some(cached);

            sender
                .send_data("127.0.0.1", addr.port(), "after 2 0\n".into(), signal)
                .await?;
            sender.close();
            let (mut second, _) = listener.accept().await.map_err(write_error)?;
            let mut after = String::new();
            second.read_to_string(&mut after).await.map_err(write_error)?;
            assert_eq!(after, "after 2 0\n");
            Ok(())
        })
        .await
        .map_err(|_| graphite_error("Carbon reset witness timed out".into()))?
    }
}
