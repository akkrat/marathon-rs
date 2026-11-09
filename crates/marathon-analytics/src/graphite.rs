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
use std::{sync::Arc, time::Duration};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpStream, lookup_host},
    time::timeout,
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
}
/// D51: Kotlin has no timeout on either, and each send holds up the queue actor.
#[derive(Clone, Copy)]
pub struct CarbonLimits {
    /// Name lookup and connect together.
    pub connect: Duration,
    /// Write and shutdown together.
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
#[derive(Clone, Copy, Default)]
pub struct SocketSender {
    pub limits: CarbonLimits,
}
fn graphite_error(message: String) -> OperationError {
    failure(
        "com.malinskiy.marathon.analytics.external.graphite.GraphiteException",
        &message,
    )
    .into()
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
            let limits = self.limits;
            let send = async {
                let connect = async {
                    let addresses = lookup_host((host, port))
                        .await
                        .map_err(|_| graphite_error(format!("Unknown host: {host}")))?
                        .collect::<Vec<_>>();
                    TcpStream::connect(addresses.as_slice())
                        .await
                        .map_err(|e| graphite_error(format!("Error while writing data to graphite: {e}")))
                };
                let mut socket = timeout(limits.connect, connect).await.map_err(|_| {
                    graphite_error(format!(
                        "Error while writing data to graphite: connect timed out after {} ms",
                        limits.connect.as_millis()
                    ))
                })??;
                let write = async {
                    socket.write_all(data.as_bytes()).await?;
                    socket.shutdown().await
                };
                timeout(limits.write, write)
                    .await
                    .map_err(|_| {
                        graphite_error(format!(
                            "Error while writing data to graphite: write timed out after {} ms",
                            limits.write.as_millis()
                        ))
                    })?
                    .map_err(|e| graphite_error(format!("Error while writing data to graphite: {e}")))?;
                Ok(())
            };
            // Approved P3: typed cancellation interrupts a stuck carbon write. D51 adds the limits.
            tokio::select! {biased; reason=cancellation.cancelled()=>Err(reason.into()),result=send=>result}
        })
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
            let from =
                format_graphite(limit).ok_or_else(|| failure("java.lang.ArithmeticException", "long overflow"))?;
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
