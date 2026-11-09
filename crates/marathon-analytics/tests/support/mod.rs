//! Port of core/src/integrationTest/kotlin/com/malinskiy/marathon/analytics/metrics/remote/TestEvents.kt
//! (fixed anchor and deterministic loopback infrastructure).
#![allow(dead_code)]
use jcompat::jtime::{duration_of, instant_of};
use marathon_analytics::AnalyticsClients;
use marathon_config::*;
use marathon_core::{
    analytics::*,
    error::*,
    executor::{CoreExecutor, Signal, bridge::call_vendor, shared::failure},
    logging::LogRecord,
    model::*,
    report::TestEvent,
    runtime::*,
    time::*,
};
use marathon_error::{FailureKind, InjectedKind};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
};

pub fn io<T>(value: std::io::Result<T>) -> OperationResult<T> {
    value.map_err(|e| failure(FailureKind::Io, &e.to_string()).into())
}
pub struct Mono;
impl MonotonicClock for Mono {
    fn nanos(&self, _: &'static str) -> ClockResult<i64> {
        Ok(0)
    }
}
#[derive(Clone)]
pub struct Env {
    pub context: MetricsContext,
    pub io: Arc<dyn VendorRuntime>,
}
impl Env {
    pub fn clients(&self) -> AnalyticsClients {
        AnalyticsClients::new(self.context.clone(), self.io.clone())
    }
}
pub fn run(body: impl FnOnce(Env) -> ServiceFuture<'static, ()> + Send + 'static) -> Vec<LogRecord> {
    let result = CoreExecutor::run(move |core| {
        Box::pin(async move {
            let context = MetricsContext {
                clock: core.vendor_clock(),
                vendor: core.vendor(),
                monotonic: Arc::new(Mono),
                cancellation: Signal::new(),
                registry: core.logging(),
                logs: core.log_queue().sender(),
                stamp: Arc::new(|| (0, 0)),
            };
            let env = Env {
                context,
                io: core.vendor(),
            };
            call_vendor(core.vendor().as_ref(), Signal::new(), move |_| {
                Box::pin(async move {
                    match tokio::time::timeout(std::time::Duration::from_secs(60), body(env)).await {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(OperationError::Failed(f))) => Err(VendorError::Other(f)),
                        Ok(Err(OperationError::Cancelled(c))) => Err(VendorError::Cancelled(c)),
                        Err(_) => Err(VendorError::Other(failure(
                            FailureKind::Injected(InjectedKind::TestWatchdog),
                            "analytics test stalled",
                        ))),
                    }
                })
            })
            .await
            .map_err(|e| match e {
                VendorError::Cancelled(c) => OperationError::Cancelled(c),
                e => OperationError::Failed(e.failure().clone()),
            })?;
            Ok(core.log_queue().records())
        })
    });
    match result {
        Ok(logs) => logs,
        Err(error) => panic!("analytics test: {error:?}"),
    }
}
pub fn test() -> Test {
    Test {
        pkg: "pkg".into(),
        clazz: "Clazz".into(),
        method: "method".into(),
        meta_properties: vec![],
    }
}
pub fn event(status: TestStatus, escaped: bool) -> TestEvent {
    let device = DeviceInfo {
        operating_system: OperatingSystem {
            version: "Fake OS".into(),
        },
        serial_number: "serial: /x".into(),
        model: "model".into(),
        manufacturer: "maker".into(),
        network_state: NetworkState::Connected,
        device_features: vec![],
        healthy: true,
    };
    let mut test = test();
    if escaped {
        test.pkg = "p a,=\\".into();
    }
    TestEvent {
        instant: instant_of(-2, 999_000_000).unwrap(),
        pool_id: DevicePoolId { name: "omni".into() },
        device: device.clone(),
        test_result: Arc::new(TestResult {
            test,
            device,
            test_batch_id: "batch".into(),
            status,
            start_time: 1000,
            end_time: 6000,
            stacktrace: None,
            attachments: vec![],
        }),
        final_result: false,
    }
}
pub fn events() -> Vec<TestEvent> {
    [1, 10, 20, 30, 40, 60, 70, 80, 90, 100]
        .into_iter()
        .enumerate()
        .map(|(i, minutes)| {
            let mut e = event(if i < 5 { TestStatus::Passed } else { TestStatus::Failure }, false);
            let millis = ANCHOR - minutes * 60_000;
            e.instant = instant_of(millis / 1000, 0).unwrap();
            let r = Arc::make_mut(&mut e.test_result);
            r.start_time = millis - (i as i64 + 1) * 1000;
            r.end_time = millis;
            e.final_result = true;
            e
        })
        .collect()
}
pub const ANCHOR: i64 = 1_767_225_600_000;
pub fn defaults() -> Defaults {
    Defaults {
        success_rate: 0.0,
        duration: duration_of(300, 0).unwrap(),
        observed_duration_min_tests: None,
    }
}
pub fn v1(url: &str) -> InfluxDbConfiguration {
    InfluxDbConfiguration {
        url: url.into(),
        user: "".into(),
        password: "".into(),
        db_name: "db".into(),
        retention_policy_configuration: RetentionPolicyConfiguration::default_policy(),
        defaults: defaults(),
        read_only: false,
    }
}
pub fn v2(url: &str) -> InfluxDb2Configuration {
    InfluxDb2Configuration {
        url: url.into(),
        token: "dummy-token".into(),
        organization: "org".into(),
        bucket: "bucket".into(),
        retention_policy_configuration: InfluxDb2RetentionPolicyConfiguration::default_policy(),
        defaults: defaults(),
        read_only: false,
    }
}
pub fn statuses() -> [TestStatus; 5] {
    [
        TestStatus::Failure,
        TestStatus::Passed,
        TestStatus::Ignored,
        TestStatus::Incomplete,
        TestStatus::AssumptionFailure,
    ]
}
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}
#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub headers: Vec<(String, String)>,
}
impl Reply {
    pub fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: body.into(),
            headers: vec![],
        }
    }
    /// The connection ends with no response at all: a transport failure, not an HTTP one.
    pub fn dropped() -> Self {
        Self {
            status: NO_RESPONSE,
            body: String::new(),
            headers: vec![],
        }
    }
}
const NO_RESPONSE: u16 = 0;
pub struct Peer {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Request>>>,
    pub changed: Arc<tokio::sync::Notify>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<OperationResult<()>>,
}
impl Peer {
    pub async fn new(mut respond: impl FnMut(&Request) -> Reply + Send + 'static) -> OperationResult<Self> {
        let listener = io(TcpListener::bind("127.0.0.1:0").await)?;
        let url = format!("http://{}", io(listener.local_addr())?);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let saved = requests.clone();
        let changed = Arc::new(tokio::sync::Notify::new());
        let notify = changed.clone();
        let (stop, mut stopping) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) =
                    tokio::select! {result=listener.accept()=>io(result)?,_= &mut stopping=>return Ok(())};
                let mut bytes = Vec::new();
                let mut buffer = [0; 8192];
                let header_end = loop {
                    let count = io(socket.read(&mut buffer).await)?;
                    if count == 0 {
                        return Err(
                            failure(FailureKind::Injected(InjectedKind::TestPeer), "EOF before HTTP headers").into(),
                        );
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(index) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        break index + 4;
                    }
                };
                let header = String::from_utf8_lossy(&bytes[..header_end]);
                let mut lines = header.split("\r\n");
                let mut first = lines.next().unwrap_or("").split(' ');
                let (method, path) = (
                    first.next().unwrap_or("").to_owned(),
                    first.next().unwrap_or("").to_owned(),
                );
                let headers: BTreeMap<_, _> = lines
                    .filter_map(|line| {
                        line.split_once(':')
                            .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
                    })
                    .collect();
                let length = headers
                    .get("content-length")
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0);
                while bytes.len() < header_end + length {
                    let count = io(socket.read(&mut buffer).await)?;
                    if count == 0 {
                        return Err(
                            failure(FailureKind::Injected(InjectedKind::TestPeer), "EOF before HTTP body").into(),
                        );
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                }
                let request = Request {
                    method,
                    path,
                    headers,
                    body: String::from_utf8_lossy(&bytes[header_end..header_end + length]).into_owned(),
                };
                let reply = respond(&request);
                saved.lock().unwrap().push(request);
                notify.notify_waiters();
                if reply.status == NO_RESPONSE {
                    continue;
                }
                let mut header = format!(
                    "HTTP/1.1 {} Reply\r\nContent-Length: {}\r\nConnection: close\r\n",
                    reply.status,
                    reply.body.len()
                );
                for (k, v) in reply.headers {
                    header.push_str(&format!("{k}: {v}\r\n"));
                }
                header.push_str("\r\n");
                io(socket.write_all(header.as_bytes()).await)?;
                io(socket.write_all(reply.body.as_bytes()).await)?;
                io(socket.shutdown().await)?;
            }
        });
        Ok(Self {
            url,
            requests,
            changed,
            stop,
            task,
        })
    }
    pub async fn wait_requests(&self, count: usize) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.requests.lock().unwrap().len() >= count {
                return;
            }
            changed.await;
        }
    }
    pub async fn shutdown(self) -> OperationResult<Vec<Request>> {
        let _ = self.stop.send(());
        self.task
            .await
            .map_err(|e| failure(FailureKind::Injected(InjectedKind::TestPeer), &e.to_string()))??;
        Ok(self.requests.lock().unwrap().clone())
    }
}
fn native_request_json(mut row: serde_json::Value) -> serde_json::Result<serde_json::Value> {
    if row["headers"]["content-type"]
        .as_str()
        .is_some_and(|t| t.starts_with("application/json"))
    {
        // Fixtures retain their original escaping; JSON bodies compare as data,
        // with declaration order retained by preserve_order.
        let body: serde_json::Value = serde_json::from_str(row["body"].as_str().unwrap_or(""))?;
        row["body"] = serde_json::to_string(&body)?.into();
    }
    Ok(row)
}
pub fn exchanges(scenario: &str) -> Vec<serde_json::Value> {
    include_str!("../../../../fixtures/oracle/analytics-clients/exchanges.jsonl")
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .and_then(native_request_json)
                .unwrap()
        })
        .filter(|row| row["scenario"] == scenario)
        .collect()
}
pub fn observations(scenario: &str) -> Vec<serde_json::Value> {
    include_str!("../../../../fixtures/oracle/analytics-clients/observations.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|row| row["scenario"] == scenario)
        .collect()
}
pub async fn replay(scenario: &str) -> OperationResult<(Peer, Arc<Mutex<usize>>)> {
    let mut rows = exchanges(scenario)
        .into_iter()
        .filter(|r| r["method"].is_string())
        .collect::<Vec<_>>();
    if scenario == "v1-config-escaping" {
        for row in &mut rows {
            if row["headers"]["authorization"].is_string() {
                // D57: base64 of the UTF-8 bytes of `u é😀:päss`.
                row["headers"]["authorization"] = "Basic dSDDqfCfmIA6cMOkc3M=".into();
            }
        }
    }
    let seen = Arc::new(Mutex::new(0));
    let count = seen.clone();
    let peer = Peer::new(move |request| {
        let mut seen = count.lock().unwrap();
        let expected = &rows[*seen];
        *seen += 1;
        assert_eq!(request.method, expected["method"]);
        assert_eq!(request.path, expected["path"]);
        assert_eq!(request.body, expected["body"]);
        for (name, value) in expected["headers"].as_object().unwrap() {
            assert_eq!(request.headers.get(name).map(String::as_str), value.as_str(), "{name}");
        }
        Reply {
            status: expected["status"].as_u64().unwrap() as u16,
            body: expected["response"].as_str().unwrap().into(),
            headers: expected["response_headers"]
                .as_object()
                .map(|headers| {
                    headers
                        .iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
                        .collect()
                })
                .unwrap_or_default(),
        }
    })
    .await?;
    Ok((peer, seen))
}
