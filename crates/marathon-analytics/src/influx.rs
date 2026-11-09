//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/influx/InfluxDbProvider.kt
//! and external/influx/InfluxDbTracker.kt.
use crate::{http::*, points};
use base64::{Engine, engine::general_purpose::STANDARD};
use marathon_config::InfluxDbConfiguration;
use marathon_core::{
    analytics::{MetricsContext, TrackerInternalAdapter, remote::influx::*},
    error::*,
    executor::{
        Signal,
        shared::{failure, requested},
    },
    model::TestStatus,
    report::TestEvent,
    runtime::*,
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
use tokio::sync::{Mutex, mpsc, oneshot};

pub const BATCH_ACTIONS: usize = 1000;
pub const BATCH_INTERVAL_MILLIS: i64 = 1000;
pub const RETRY_BUFFER: usize = 10000;
enum Message {
    Point(String),
    Flush(oneshot::Sender<OperationResult<()>>),
    Close(oneshot::Sender<OperationResult<()>>),
}
pub struct InfluxClient {
    http: SharedHttp,
    sender: mpsc::Sender<Message>,
    task: Arc<dyn VendorTask>,
    /// The worker's own cancellation, used only when a shutdown is abandoned.
    worker: Arc<Signal>,
    closed: Mutex<bool>,
}
/// D51: a caller that stops waiting for `shutdown`, as `BackendGuard` does at its limit, drops
/// the future. The worker would otherwise go on with a request that a trickling server never ends.
struct AbandonOnDrop<'a>(Option<&'a Signal>);
impl Drop for AbandonOnDrop<'_> {
    fn drop(&mut self) {
        if let Some(worker) = self.0 {
            worker.cancel(requested("InfluxDB shutdown abandoned"));
        }
    }
}
impl InfluxClient {
    /// Poll on the vendor runtime during dependency construction; history and
    /// tracking each call this constructor, owning independent workers.
    pub async fn connect(
        config: &InfluxDbConfiguration,
        debug: bool,
        context: MetricsContext,
        io: Arc<dyn VendorRuntime>,
    ) -> OperationResult<Arc<Self>> {
        let (user, password) = (text(&config.user)?, text(&config.password)?);
        let auth = (!user.is_empty() && !password.is_empty())
            .then(|| format!("Basic {}", STANDARD.encode(format!("{user}:{password}").as_bytes())));
        let http = Arc::new(Http::new(&config.url, auth, context.clone(), debug)?);
        let database = text(&config.db_name)?;
        let rp = &config.retention_policy_configuration;
        let policy = text(&rp.name)?;
        let databases = setup_query(&http, "SHOW DATABASES").await?;
        let rows = databases
            .results
            .first()
            .and_then(|s| s.series.first())
            .ok_or_else(|| failure("java.lang.NullPointerException", "Missing database series"))?;
        let exists = rows.values.iter().any(|row| matches!(row.first(), Some(Value::Text(name)) if name.to_owned().trim_matches(|c| c <= '\u{20}') == database));
        if !exists {
            // influxdb-java Preconditions, in createDatabase/createRetentionPolicy order.
            let empty = |name: &str| {
                failure(
                    "java.lang.IllegalArgumentException",
                    &format!("Expecting a non-empty string for {name}"),
                )
            };
            if database.is_empty() {
                return Err(empty("name").into());
            }
            setup_query(&http, &format!("CREATE DATABASE \"{database}\"")).await?;
            let (duration, shard) = (text(&rp.duration)?, text(&rp.shard_duration)?);
            for (value, name) in [
                (&policy, "retentionPolicyName"),
                (&database, "database"),
                (&duration, "retentionDuration"),
            ] {
                if value.is_empty() {
                    return Err(empty(name).into());
                }
            }
            for (value, name) in [(&duration, "retentionDuration"), (&shard, "shardDuration")] {
                if !value.is_empty() && !valid_duration(value) {
                    return Err(failure(
                        "java.lang.IllegalArgumentException",
                        &format!("Invalid InfluxDB duration: {value} for {name}"),
                    )
                    .into());
                }
            }
            if rp.replication_factor <= 0 {
                return Err(failure(
                    "java.lang.IllegalArgumentException",
                    "Expecting a positive number for replicationFactor",
                )
                .into());
            }
            let mut query = format!(
                "CREATE RETENTION POLICY \"{policy}\" ON \"{database}\" DURATION {duration} REPLICATION {}",
                rp.replication_factor
            );
            if !shard.is_empty() {
                query.push_str(&format!(" SHARD DURATION {shard}"));
            }
            if rp.is_default {
                query.push_str(" DEFAULT");
            }
            setup_query(&http, &query).await?;
        }
        let (sender, receiver) = mpsc::channel(BATCH_ACTIONS);
        // Java's scheduled batch pool is independent of the caller's coroutine.
        // onFinish must still be able to drain it after the run is cancelled.
        let worker = Signal::new();
        let worker_http = Arc::new(http.with_cancellation(worker.clone()));
        let task = io.spawn(Box::pin(async move {
            run_batches(worker_http, context, database, policy, receiver)
                .await
                .map_err(vendor)
        }));
        Ok(Arc::new(Self {
            http,
            sender,
            task,
            worker,
            closed: Mutex::new(false),
        }))
    }
    pub async fn write(&self, point: String) -> OperationResult<()> {
        let closed = self.closed.lock().await;
        if *closed {
            return Err(crate::http::closed());
        }
        self.sender
            .send(Message::Point(point))
            .await
            .map_err(|_| crate::http::closed())
    }
    pub async fn flush(&self) -> OperationResult<()> {
        let closed = self.closed.lock().await;
        if *closed {
            return Err(crate::http::closed());
        }
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(Message::Flush(reply))
            .await
            .map_err(|_| crate::http::closed())?;
        receiver.await.map_err(|_| crate::http::closed())?
    }
    pub async fn shutdown(&self) -> OperationResult<()> {
        let mut abandon = AbandonOnDrop(Some(&self.worker));
        let mut closed = self.closed.lock().await;
        let result = if !*closed {
            *closed = true;
            let (reply, receiver) = oneshot::channel();
            match self.sender.send(Message::Close(reply)).await {
                Ok(()) => receiver
                    .await
                    .map_err(|_| crate::http::closed())
                    .and_then(|result| result),
                Err(_) => Err(crate::http::closed()),
            }
        } else {
            Ok(())
        };
        // Hold the close gate until the worker and its current socket are gone.
        let joined = self.task.join().await;
        abandon.0 = None;
        match joined {
            // An earlier shutdown was abandoned and stopped the worker; whoever abandoned it reported that.
            Err(VendorError::Cancelled(_)) if self.worker.reason().is_some() => Ok(()),
            Err(VendorError::Cancelled(c)) => Err(OperationError::Cancelled(c)),
            Err(e) => Err(OperationError::Failed(e.failure().clone())),
            Ok(()) => result,
        }
    }
}
pub(crate) async fn request(
    http: &Http,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: String,
) -> OperationResult<String> {
    let reply = http
        .request(method, path, content_type, None, body)
        .await
        .map_err(|error| match error {
            OperationError::Failed(cause)
                if cause.class.to_owned().starts_with("java.io.")
                    || cause.class.to_owned().starts_with("java.net.") =>
            {
                OperationError::Failed(Failure {
                    class: "org.influxdb.InfluxDBIOException".into(),
                    message: Some(cause.to_java_string()),
                    cause: Some(Box::new(cause)),
                    stacktrace: None,
                })
            }
            other => other,
        })?;
    checked(reply, false)
}
async fn setup_query(http: &Http, command: &str) -> OperationResult<QueryResult> {
    let body = request(
        http,
        "POST",
        "query",
        Some("application/x-www-form-urlencoded"),
        format!("q={}", form_encode(command)),
    )
    .await?;
    decode(&body)
}
impl InfluxQuery for InfluxClient {
    fn query<'a>(&'a self, command: &'a str, database: &'a str) -> ServiceFuture<'a, QueryResult> {
        Box::pin(async move {
            let guard = self.closed.lock().await;
            if *guard {
                return Err(closed());
            }
            let path = format!(
                "query?db={}&q={}",
                query_encode(&text(database)?),
                form_encode(&text(command)?)
            );
            decode(&request(&self.http, "GET", &path, None, String::new()).await?)
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(self.shutdown())
    }
}
pub fn decode(body: &str) -> OperationResult<QueryResult> {
    let value = json(body, "com.squareup.moshi.JsonEncodingException")?;
    fn string(v: &serde_json::Value) -> Option<String> {
        v.as_str().map(Into::into)
    }
    fn values(v: &serde_json::Value) -> &[serde_json::Value] {
        v.as_array().map(Vec::as_slice).unwrap_or_default()
    }
    Ok(QueryResult {
        error: string(&value["error"]),
        results: values(&value["results"])
            .iter()
            .map(|result| StatementResult {
                error: string(&result["error"]),
                series: values(&result["series"])
                    .iter()
                    .map(|series| Series {
                        name: string(&series["name"]).unwrap_or_default(),
                        tags: series["tags"]
                            .as_object()
                            .map(|tags| {
                                tags.iter()
                                    .filter_map(|(k, v)| string(v).map(|v| (k.as_str().into(), v)))
                                    .collect::<BTreeMap<_, _>>()
                            })
                            .unwrap_or_default(),
                        columns: values(&series["columns"]).iter().filter_map(string).collect(),
                        values: values(&series["values"])
                            .iter()
                            .map(|row| {
                                values(row)
                                    .iter()
                                    .map(|v| match v {
                                        serde_json::Value::Number(n) => Value::Number(n.as_f64().unwrap_or(f64::NAN)),
                                        serde_json::Value::String(s) => Value::Text(s.as_str().into()),
                                        _ => Value::Null,
                                    })
                                    .collect()
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect(),
    })
}
struct BatchWriter {
    http: SharedHttp,
    path: String,
    retries: VecDeque<Vec<String>>,
}
impl BatchWriter {
    async fn send(&self, lines: &[String]) -> OperationResult<()> {
        request(
            &self.http,
            "POST",
            &self.path,
            Some("text/plain; charset=utf-8"),
            format!("{}\n", lines.join("\n")),
        )
        .await
        .map(|_| ())
    }
    fn retryable(error: &OperationError) -> bool {
        let OperationError::Failed(failure) = error else {
            return false;
        };
        let message = failure.message.to_owned().unwrap_or_default();
        ![
            "database not found",
            "points beyond retention policy",
            "field type conflict",
            "unable to parse",
            "hinted handoff queue not empty",
            "user is required to write to database",
            "user is not authorized to write to database",
            "authorization failed",
            "username required",
        ]
        .iter()
        .any(|s| message.contains(s))
    }
    fn buffer(&mut self, points: Vec<String>) {
        if let Some(last) = self
            .retries
            .back_mut()
            .filter(|last| last.len() + points.len() <= BATCH_ACTIONS)
        {
            last.extend(points);
        } else {
            self.retries.push_back(points);
        }
        while self.retries.iter().map(Vec::len).sum::<usize>() > RETRY_BUFFER {
            self.retries.pop_front();
        }
    }
    async fn flush(&mut self, points: &mut Vec<String>) -> OperationResult<()> {
        while let Some(batch) = self.retries.pop_front() {
            match self.send(&batch).await {
                Err(error @ OperationError::Cancelled(_)) => return Err(error),
                Err(error) if Self::retryable(&error) => {
                    self.retries.push_front(batch);
                    if !points.is_empty() {
                        let points = std::mem::take(points);
                        self.buffer(points);
                    }
                    return Ok(());
                }
                _ => {}
            }
        }
        if !points.is_empty() {
            let points = std::mem::take(points);
            match self.send(&points).await {
                Err(error @ OperationError::Cancelled(_)) => return Err(error),
                Err(error) if Self::retryable(&error) => self.buffer(points),
                _ => {}
            }
        }
        Ok(())
    }
    async fn close(&mut self, points: &mut Vec<String>) -> OperationResult<()> {
        self.flush(points).await?;
        // RetryCapableBatchWriter.close makes one last attempt per buffered batch.
        while let Some(batch) = self.retries.pop_front() {
            if let Err(e @ OperationError::Cancelled(_)) = self.send(&batch).await {
                return Err(e);
            }
        }
        Ok(())
    }
}
async fn run_batches(
    http: SharedHttp,
    context: MetricsContext,
    database: String,
    policy: String,
    mut receiver: mpsc::Receiver<Message>,
) -> OperationResult<()> {
    let mut writer = BatchWriter {
        http,
        path: format!(
            "write?db={}&rp={}&precision=n&consistency=one",
            query_encode(&database),
            query_encode(&policy)
        ),
        retries: VecDeque::new(),
    };
    let mut points = Vec::new();
    let timer_signal = Signal::new();
    let timer = context.clock.delay(BATCH_INTERVAL_MILLIS, timer_signal.clone());
    tokio::pin!(timer);
    loop {
        tokio::select! {
            biased;
            message=receiver.recv()=>match message {
                Some(Message::Point(point))=>{points.push(point);if points.len()>=BATCH_ACTIONS {writer.flush(&mut points).await?;}},
                Some(Message::Flush(reply))=>{let result=writer.flush(&mut points).await;let _=reply.send(result.clone());result?;},
                Some(Message::Close(reply))=>{let result=writer.close(&mut points).await;let _=reply.send(result.clone());return result;},
                None=>return Ok(()), // Drop never flushes or generates observable cleanup.
            },
            result=&mut timer=>{
                result.map_err(|e| match e { VendorError::Cancelled(c)=>OperationError::Cancelled(c),e=>OperationError::Failed(e.failure().clone()) })?;
                writer.flush(&mut points).await?;
                timer.set(context.clock.delay(BATCH_INTERVAL_MILLIS,timer_signal.clone()));
            },
        }
    }
}
pub struct InfluxTracker {
    pub client: Arc<InfluxClient>,
    pub read_only: bool,
}
impl InfluxTracker {
    pub async fn track_test(&self, event: &TestEvent) -> OperationResult<()> {
        if self.read_only || !matches!(event.test_result.status, TestStatus::Passed | TestStatus::Failure) {
            return Ok(());
        }
        self.client.write(points::line(event, false)?).await
    }
}
impl TrackerInternalAdapter for InfluxTracker {
    fn test<'a>(&'a self, event: &'a TestEvent) -> VendorFuture<'a, ()> {
        Box::pin(async move { self.track_test(event).await.map_err(vendor) })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(self.client.shutdown())
    }
}

fn valid_duration(value: &str) -> bool {
    if value == "inf" {
        return true;
    }
    let bytes = value.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
        if at == start || at == bytes.len() || !b"wdmhs".contains(&bytes[at]) {
            return false;
        }
        at += 1;
    }
    !bytes.is_empty()
}
