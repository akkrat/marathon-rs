//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/influx/InfluxDbProvider.kt
//! and external/influx2/InfluxDb2Provider.kt (OkHttp transport settings).
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, header};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_timeout::TimeoutConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use jcompat::{
    logging::Level,
    writers::{GsonWriter, JavaString, WriterValue},
};
use marathon_core::{analytics::MetricsContext, error::*, executor::shared::failure};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
use tower_service::Service;
use url::Url;

pub(crate) fn text(value: &JavaString) -> OperationResult<String> {
    value
        .as_utf8()
        .map_err(|_| failure("java.lang.IllegalArgumentException", "Analytics text must be UTF-8").into())
}
pub(crate) fn quote(value: &str) -> String {
    match GsonWriter::default().write(&WriterValue::String(value.into())) {
        Ok(text) => text.to_string_lossy(),
        Err(_) => unreachable!("Gson string encoding is infallible"),
    }
}
pub(crate) fn vendor(error: OperationError) -> VendorError {
    match error {
        OperationError::Failed(f) => VendorError::Other(f),
        OperationError::Cancelled(c) => VendorError::Cancelled(c),
    }
}
pub(crate) fn log(context: &MetricsContext, logger: &str, level: Level, message: String, cause: Option<Failure>) {
    let (at_millis, wall_millis) = (context.stamp)();
    context.logs.push(marathon_core::logging::LogRecord {
        logger: logger.into(),
        at_millis,
        wall_millis,
        level,
        message: message.into(),
        cause,
    });
}
pub(crate) fn register(context: &MetricsContext, name: &str) -> OperationResult<()> {
    context
        .registry
        .lock()
        .map_err(|_| failure("java.lang.IllegalStateException", "Poisoned logging registry"))?
        .logger(name, None);
    Ok(())
}
#[derive(Clone)]
pub(crate) struct Http {
    connector: TimeoutConnector<HttpsConnector<HttpConnector>>,
    pub base: Url,
    auth: Option<String>,
    context: MetricsContext,
    debug: bool,
}
pub(crate) struct Reply {
    pub status: u16,
    pub body: String,
    pub location: Option<String>,
    pub retry_after: Option<String>,
}
impl Http {
    pub fn new(base: &JavaString, auth: Option<String>, context: MetricsContext, debug: bool) -> OperationResult<Self> {
        let raw = text(base)?;
        // P4: reject URL forms which WHATWG would silently repair.
        if !raw
            .split_once("://")
            .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
            || raw.trim() != raw
            || raw.chars().any(|c| c.is_control() || c == '\\')
        {
            return Err(failure(
                "java.lang.IllegalArgumentException",
                "Analytics URL must be an absolute HTTP(S) URL without controls or backslashes",
            )
            .into());
        }
        let mut base = Url::parse(&raw).map_err(|e| failure("java.lang.IllegalArgumentException", &e.to_string()))?;
        if base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(failure(
                "java.lang.IllegalArgumentException",
                "Analytics URL cannot contain credentials, query or fragment",
            )
            .into());
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let tls = HttpsConnectorBuilder::new()
            .with_tls_config(tls_config()?)
            .https_or_http()
            .enable_http1()
            .build();
        let mut connector = TimeoutConnector::new(tls);
        connector.set_connect_timeout(Some(Duration::from_secs(60)));
        connector.set_read_timeout(Some(Duration::from_secs(60)));
        connector.set_write_timeout(Some(Duration::from_secs(60)));
        Ok(Self {
            connector,
            base,
            auth,
            context,
            debug,
        })
    }
    pub fn with_cancellation(&self, cancellation: Arc<dyn marathon_core::runtime::CancellationSignal>) -> Self {
        let mut http = self.clone();
        http.context.cancellation = cancellation;
        http
    }
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        accept: Option<&str>,
        body: String,
    ) -> OperationResult<Reply> {
        let mut url = self
            .base
            .join(path)
            .map_err(|e| failure("java.lang.IllegalArgumentException", &e.to_string()))?;
        let mut method = method.to_owned();
        let mut body = body;
        let origin = self.base.origin();
        let mut previous_status = None;
        for _ in 0..21 {
            if let Some(reason) = self.context.cancellation.reason() {
                return Err(reason.into());
            }
            let request = async {
                let uri = url
                    .as_str()
                    .parse::<hyper::Uri>()
                    .map_err(|e| failure("java.lang.IllegalArgumentException", &e.to_string()))?;
                let mut connector = self.connector.clone();
                let stream = connector
                    .call(uri.clone())
                    .await
                    .map_err(|e| network_error(e.as_ref()))?;
                let (mut sender, connection) = hyper::client::conn::http1::handshake(stream)
                    .await
                    .map_err(|e| network_error(&e))?;
                let authority = uri.authority().map(|a| a.as_str()).unwrap_or("");
                let target = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
                let mut builder = Request::builder()
                    .method(method.as_str())
                    .uri(target)
                    .header(header::HOST, authority);
                if url.origin() == origin
                    && let Some(auth) = &self.auth
                {
                    builder = builder.header(header::AUTHORIZATION, auth);
                }
                if let Some(ct) = content_type.filter(|_| method != "GET") {
                    builder = builder.header(header::CONTENT_TYPE, ct);
                }
                if let Some(accept) = accept {
                    builder = builder.header(header::ACCEPT, accept);
                }
                let request = builder
                    .body(Full::new(Bytes::from(body.clone())))
                    .map_err(|e| failure("java.lang.IllegalArgumentException", &e.to_string()))?;
                let exchange = async {
                    let response = sender.send_request(request).await.map_err(|e| network_error(&e))?;
                    let status = response.status().as_u16();
                    let location = response
                        .headers()
                        .get(header::LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    let retry_after = response
                        .headers()
                        .get(header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    let bytes = response
                        .into_body()
                        .collect()
                        .await
                        .map_err(|e| network_error(&e))?
                        .to_bytes();
                    drop(sender);
                    Ok::<_, Failure>(Reply {
                        status,
                        location,
                        retry_after,
                        body: String::from_utf8_lossy(&bytes).into_owned(),
                    })
                };
                // The connection driver is scoped to this request; no detached pool task.
                let (reply, _driver) = futures_util::future::join(exchange, connection).await;
                reply
            };
            if self.debug {
                log(
                    &self.context,
                    "okhttp3.OkHttpClient",
                    Level::Info,
                    format!("--> {method} {url}"),
                    None,
                );
            }
            let response = tokio::select! {
                biased;
                reason = self.context.cancellation.cancelled() => return Err(reason.into()),
                result = request => result?,
            };
            if self.debug {
                log(
                    &self.context,
                    "okhttp3.OkHttpClient",
                    Level::Info,
                    format!("<-- {} {url}", response.status),
                    None,
                );
            }
            let retry_delay = response
                .retry_after
                .as_deref()
                .map(|value| value.parse::<u64>().unwrap_or(u64::MAX));
            if (response.status == 408 && previous_status != Some(408) && retry_delay.unwrap_or(0) == 0)
                || (response.status == 503 && previous_status != Some(503) && retry_delay == Some(0))
            {
                previous_status = Some(response.status);
                continue;
            }
            previous_status = Some(response.status);
            if matches!(response.status, 300 | 301 | 302 | 303 | 307 | 308)
                && let Some(location) = &response.location
            {
                url = url
                    .join(location)
                    .map_err(|e| failure("java.lang.IllegalArgumentException", &e.to_string()))?;
                if !matches!(url.scheme(), "http" | "https") {
                    return Ok(response);
                }
                // OkHttp keeps the method and body only for 307/308.
                if method != "GET" && method != "HEAD" && !matches!(response.status, 307 | 308) {
                    method = "GET".into();
                    body.clear();
                }
                continue;
            }
            return Ok(response);
        }
        Err(failure("java.net.ProtocolException", "Too many follow-up requests: 21").into())
    }
}
pub(crate) fn json(body: &str, class: &str) -> OperationResult<serde_json::Value> {
    serde_json::from_str(body).map_err(|e| failure(class, &e.to_string()).into())
}
pub(crate) fn checked(reply: Reply, v2: bool) -> OperationResult<String> {
    if (200..300).contains(&reply.status) {
        return Ok(reply.body);
    }
    let error = serde_json::from_str::<serde_json::Value>(&reply.body).ok();
    let message = error
        .as_ref()
        .and_then(|v| v[if v2 { "message" } else { "error" }].as_str())
        .unwrap_or(&reply.body);
    let class = if v2 {
        match reply.status {
            400 => "com.influxdb.exceptions.BadRequestException",
            401 => "com.influxdb.exceptions.UnauthorizedException",
            403 => "com.influxdb.exceptions.ForbiddenException",
            404 => "com.influxdb.exceptions.NotFoundException",
            422 => "com.influxdb.exceptions.UnprocessableEntityException",
            500 => "com.influxdb.exceptions.InternalServerErrorException",
            503 => "com.influxdb.exceptions.ServiceUnavailableException",
            _ => "com.influxdb.exceptions.InfluxException",
        }
    } else {
        v1_error_class(message)
    };
    let message = if v2 {
        format!("HTTP status code: {}; Message: {message}", reply.status)
    } else {
        message.into()
    };
    Err(failure(class, &message).into())
}
/// influxdb-java InfluxDBException.buildExceptionFromErrorMessage, first match wins.
pub(crate) fn v1_error_class(message: &str) -> &'static str {
    const CLASSES: [(&str, &str); 10] = [
        (
            "database not found",
            "org.influxdb.InfluxDBException$DatabaseNotFoundException",
        ),
        (
            "points beyond retention policy",
            "org.influxdb.InfluxDBException$PointsBeyondRetentionPolicyException",
        ),
        (
            "field type conflict",
            "org.influxdb.InfluxDBException$FieldTypeConflictException",
        ),
        (
            "unable to parse",
            "org.influxdb.InfluxDBException$UnableToParseException",
        ),
        (
            "hinted handoff queue not empty",
            "org.influxdb.InfluxDBException$HintedHandOffQueueNotEmptyException",
        ),
        (
            "cache-max-memory-size exceeded",
            "org.influxdb.InfluxDBException$CacheMaxMemorySizeExceededException",
        ),
        (
            "user is required to write to database",
            "org.influxdb.InfluxDBException$AuthorizationFailedException",
        ),
        (
            "user is not authorized to write to database",
            "org.influxdb.InfluxDBException$AuthorizationFailedException",
        ),
        (
            "authorization failed",
            "org.influxdb.InfluxDBException$AuthorizationFailedException",
        ),
        (
            "username required",
            "org.influxdb.InfluxDBException$AuthorizationFailedException",
        ),
    ];
    CLASSES
        .iter()
        .find(|(text, _)| message.contains(text))
        .map_or("org.influxdb.InfluxDBException", |(_, class)| class)
}
pub(crate) fn closed() -> OperationError {
    failure("java.lang.IllegalStateException", "Client is closed").into()
}
pub(crate) type SharedHttp = Arc<Http>;

/// The bundled Mozilla roots (close to the JVM's cacerts) plus the platform
/// store, so internally issued CAs installed on the host are trusted.
/// Loaded once per process on the vendor side; unreadable entries are skipped.
fn roots() -> Arc<rustls::RootCertStore> {
    static ROOTS: OnceLock<Arc<rustls::RootCertStore>> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let mut roots = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
            Arc::new(roots)
        })
        .clone()
}
fn tls_config() -> OperationResult<rustls::ClientConfig> {
    Ok(
        rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| failure("java.security.GeneralSecurityException", &e.to_string()))?
            .with_root_certificates(roots())
            .with_no_client_auth(),
    )
}
pub(crate) fn network_error(error: &(dyn std::error::Error + 'static)) -> Failure {
    let mut source = Some(error);
    let mut class = "java.io.IOException";
    while let Some(current) = source {
        if let Some(io) = current.downcast_ref::<std::io::Error>() {
            class = match io.kind() {
                std::io::ErrorKind::ConnectionRefused => "java.net.ConnectException",
                std::io::ErrorKind::TimedOut => "java.net.SocketTimeoutException",
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe => "java.net.SocketException",
                _ => class,
            };
        }
        source = current.source();
    }
    failure(class, &error.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use marathon_core::{executor::Signal, logging::LogQueue, runtime::*, time::*};
    use tokio::{io::AsyncReadExt, net::TcpListener, sync::Notify};

    impl Http {
        /// Shortened socket-idle limits for loopback fault tests.
        pub(crate) fn set_idle_timeouts(&mut self, timeout: Duration) {
            self.connector.set_connect_timeout(Some(timeout));
            self.connector.set_read_timeout(Some(timeout));
            self.connector.set_write_timeout(Some(timeout));
        }
    }
    struct Clock;
    impl MonotonicClock for Clock {
        fn nanos(&self, _: &'static str) -> ClockResult<i64> {
            Ok(0)
        }
    }
    impl VendorClock for Clock {
        fn delay(&self, _: i64, _: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
            Box::pin(std::future::pending())
        }
        fn with_timeout<'a>(
            &'a self,
            p: Arc<dyn CancellationSignal>,
            _: i64,
            b: VendorTimeoutBody<'a>,
        ) -> VendorFuture<'a, ()> {
            b(p)
        }
        fn with_timeout_or_null<'a>(
            &'a self,
            p: Arc<dyn CancellationSignal>,
            _: i64,
            b: VendorTimeoutBody<'a>,
        ) -> VendorFuture<'a, Option<()>> {
            Box::pin(async move { b(p).await.map(Some) })
        }
    }
    pub(crate) fn context() -> MetricsContext {
        MetricsContext {
            clock: Arc::new(Clock),
            monotonic: Arc::new(Clock),
            cancellation: Signal::new(),
            registry: Arc::default(),
            logs: LogQueue::default().sender(),
            stamp: Arc::new(|| (0, 0)),
        }
    }
    async fn timeout(write: bool) -> OperationResult<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|e| network_error(&e))?;
        let url = format!("http://{}", listener.local_addr().map_err(|e| network_error(&e))?);
        let mut http = Http::new(&url.into(), None, context(), false)?;
        // Shortened socket-idle timeout, retaining the independent other scopes.
        if write {
            http.connector.set_write_timeout(Some(Duration::from_millis(10)));
        } else {
            http.connector.set_read_timeout(Some(Duration::from_millis(10)));
        }
        let done = Notify::new();
        let server = async {
            let (mut stream, _) = listener.accept().await.map_err(|e| network_error(&e))?;
            let mut bytes = [0; 1];
            let _ = stream.read(&mut bytes).await.map_err(|e| network_error(&e))?;
            done.notified().await;
            Ok::<_, Failure>(())
        };
        let request = async {
            let body = if write {
                "x".repeat(32 * 1024 * 1024)
            } else {
                String::new()
            };
            let result = http.request("POST", "slow", Some("text/plain"), None, body).await;
            done.notify_one();
            assert!(
                matches!(result,Err(OperationError::Failed(f)) if f.class==JavaString::from("java.net.SocketTimeoutException"))
            );
        };
        let (_, server) = futures_util::future::join(request, server).await;
        server?;
        Ok(())
    }
    #[test]
    fn tls_trusts_bundled_and_platform_roots() {
        let mut expected = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        expected.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
        assert_eq!(roots().len(), expected.len());
        assert!(roots().len() >= webpki_roots::TLS_SERVER_ROOTS.len());
        assert!(tls_config().is_ok());
        assert!(Http::new(&"https://127.0.0.1:1".into(), None, context(), false).is_ok());
    }
    #[test]
    fn v1_error_bodies_map_to_influxdb_java_subclasses() {
        for (message, class) in [
            (
                "database not found",
                "org.influxdb.InfluxDBException$DatabaseNotFoundException",
            ),
            (
                "partial write: points beyond retention policy dropped=1",
                "org.influxdb.InfluxDBException$PointsBeyondRetentionPolicyException",
            ),
            (
                "field type conflict",
                "org.influxdb.InfluxDBException$FieldTypeConflictException",
            ),
            (
                "unable to parse 'x'",
                "org.influxdb.InfluxDBException$UnableToParseException",
            ),
            (
                "hinted handoff queue not empty",
                "org.influxdb.InfluxDBException$HintedHandOffQueueNotEmptyException",
            ),
            (
                "engine: cache-max-memory-size exceeded",
                "org.influxdb.InfluxDBException$CacheMaxMemorySizeExceededException",
            ),
            (
                "user is required to write to database \"db\"",
                "org.influxdb.InfluxDBException$AuthorizationFailedException",
            ),
            (
                "user is not authorized to write to database \"db\"",
                "org.influxdb.InfluxDBException$AuthorizationFailedException",
            ),
            (
                "authorization failed",
                "org.influxdb.InfluxDBException$AuthorizationFailedException",
            ),
            (
                "username required",
                "org.influxdb.InfluxDBException$AuthorizationFailedException",
            ),
            (
                "database not found; field type conflict",
                "org.influxdb.InfluxDBException$DatabaseNotFoundException",
            ),
            ("bad", "org.influxdb.InfluxDBException"),
        ] {
            assert_eq!(v1_error_class(message), class, "{message}");
        }
    }
    #[tokio::test]
    async fn read_and_write_have_independent_idle_timeouts() {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            timeout(false).await?;
            timeout(true).await
        })
        .await;
        assert!(matches!(result, Ok(Ok(()))));
    }
}
