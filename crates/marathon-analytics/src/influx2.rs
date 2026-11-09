//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/influx2/InfluxDb2Provider.kt
//! and external/influx2/InfluxDb2Tracker.kt, external/influx2/Influxdb2Exception.kt.
use marathon_logging::Level;

use crate::http::query_encode;
use crate::{http::*, points};
use jcompat::{jnum, };
use marathon_config::InfluxDb2Configuration;
use marathon_core::{
    analytics::{
        MetricsContext, TrackerInternalAdapter,
        remote::{influx::Value, influx2::*},
    },
    error::*,
    executor::shared::failure,
    report::TestEvent,
    runtime::*,
};
use marathon_error::{AnalyticsKind, FailureKind};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub struct Influx2Client {
    http: SharedHttp,
    organization: String,
    bucket: String,
    context: MetricsContext,
    closed: AtomicBool,
    operations: tokio::sync::Mutex<()>,
}
impl Influx2Client {
    pub async fn connect(
        config: &InfluxDb2Configuration,
        debug: bool,
        context: MetricsContext,
    ) -> OperationResult<Arc<Self>> {
        register(
            &context,
            "com.malinskiy.marathon.analytics.external.influx2.InfluxDb2Provider",
        )?;
        let token = text(&config.token)?;
        let http = Arc::new(Http::new(
            &config.url,
            // OkHttp trims header values, so an empty token still sends `Token`.
            Some(format!("Token {token}").trim().to_owned()),
            context.clone(),
            debug,
        )?);
        let (organization, bucket) = (text(&config.organization)?, text(&config.bucket)?);
        let organizations = json(
            &api(&http, "GET", "api/v2/orgs", None, None, String::new()).await?,
            FailureKind::Analytics(AnalyticsKind::InfluxV2),
        )?;
        let candidates: Vec<_> = organizations["orgs"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|org| org["name"].as_str() == Some(&organization))
            .collect();
        let Some(first) = candidates.first() else {
            return Err(failure(
                FailureKind::Analytics(AnalyticsKind::InfluxV2Provider),
                &format!("No organization with name = {organization} found"),
            )
            .into());
        };
        let id = first["id"].as_str().unwrap_or("null");
        if candidates.len() > 1 {
            log(
                &context,
                "com.malinskiy.marathon.analytics.external.influx2.InfluxDb2Provider",
                Level::WARN,
                format!("Multiple organizations with name = {organization} found. Using first one, id = {id}"),
                None,
            );
        }
        if bucket.is_empty() {
            return Err(failure(
                FailureKind::IllegalArgument,
                "Expecting a non-empty string for Bucket Name",
            )
            .into());
        }
        let buckets = json(
            &api(
                &http,
                "GET",
                &format!("api/v2/buckets?name={}", query_encode(&bucket)),
                None,
                None,
                String::new(),
            )
            .await?,
            FailureKind::Analytics(AnalyticsKind::InfluxV2),
        )?;
        if buckets["buckets"].as_array().is_none_or(Vec::is_empty) {
            let rp = &config.retention_policy_configuration;
            log(
                &context,
                "com.malinskiy.marathon.analytics.external.influx2.InfluxDb2Provider",
                Level::INFO,
                format!(
                    "Creating InfluxDB 2 bucket: orgId = {id}, bucket = {bucket}, retention: RetentionPolicyConfiguration(everySeconds={}, shardGroupDurationSeconds={})",
                    rp.every_seconds, rp.shard_group_duration_seconds
                ),
                None,
            );
            let body = json_body(&BucketRequest {
                org_id: id,
                name: &bucket,
                retention_rules: [RetentionRule {
                    kind: "expire",
                    every_seconds: rp.every_seconds,
                    shard_group_duration_seconds: rp.shard_group_duration_seconds,
                }],
            })?;
            api(&http, "POST", "api/v2/buckets", Some("application/json"), None, body).await?;
        }
        Ok(Arc::new(Self {
            http,
            organization,
            bucket,
            context,
            closed: AtomicBool::new(false),
            operations: tokio::sync::Mutex::new(()),
        }))
    }
    pub async fn write(&self, point: String) -> OperationResult<()> {
        let _operation = self.operations.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(closed());
        }
        let path = format!(
            "api/v2/write?org={}&bucket={}&precision=ms",
            query_encode(&self.organization),
            query_encode(&self.bucket)
        );
        api(
            &self.http,
            "POST",
            &path,
            Some("text/plain; charset=utf-8"),
            Some("application/json"),
            point,
        )
        .await
        .map(|_| ())
    }
    pub async fn shutdown(&self) -> OperationResult<()> {
        let _operation = self.operations.lock().await;
        self.closed.store(true, Ordering::Release);
        Ok(())
    }
}
pub(crate) async fn api(
    http: &Http,
    method: &str,
    path: &str,
    ct: Option<&str>,
    accept: Option<&str>,
    body: String,
) -> OperationResult<String> {
    match http.request(method, path, ct, accept, body).await {
        Ok(reply) => checked(reply, true),
        Err(OperationError::Failed(cause)) if cause.kind.wraps_as_analytics_io() => Err(Failure {
            kind: FailureKind::Analytics(AnalyticsKind::InfluxV2),
            message: cause.message.to_owned(),
            cause: Some(Box::new(cause)),
            stacktrace: None,
        }
        .into()),
        Err(error) => Err(error),
    }
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BucketRequest<'a> {
    #[serde(rename = "orgID")]
    org_id: &'a str,
    name: &'a str,
    retention_rules: [RetentionRule; 1],
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RetentionRule {
    #[serde(rename = "type")]
    kind: &'static str,
    every_seconds: i32,
    shard_group_duration_seconds: i64,
}
#[derive(serde::Serialize)]
struct FluxRequest<'a> {
    query: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    params: Empty,
    dialect: FluxDialect,
}
#[derive(serde::Serialize)]
struct Empty {}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FluxDialect {
    header: bool,
    delimiter: &'static str,
    annotations: [&'static str; 3],
    comment_prefix: &'static str,
    date_time_format: &'static str,
}
impl FluxQuery for Influx2Client {
    fn query<'a>(&'a self, flux: &'a str) -> ServiceFuture<'a, Vec<Record>> {
        Box::pin(async move {
            let _operation = self.operations.lock().await;
            if self.closed.load(Ordering::Acquire) {
                return Err(closed());
            }
            let body = json_body(&FluxRequest {
                query: flux,
                kind: "flux",
                params: Empty {},
                dialect: FluxDialect {
                    header: true,
                    delimiter: ",",
                    annotations: ["datatype", "group", "default"],
                    comment_prefix: "#",
                    date_time_format: "RFC3339",
                },
            })?;
            decode(
                &api(
                    &self.http,
                    "POST",
                    &format!("api/v2/query?org={}", query_encode(&self.organization)),
                    Some("application/json; charset=UTF-8"),
                    None,
                    body,
                )
                .await?,
            )
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(self.shutdown())
    }
}
fn parse_error(message: &str) -> OperationError {
    failure(FailureKind::Analytics(AnalyticsKind::FluxCsvParser), message).into()
}
/// The query API wraps a cell's NumberFormatException in InfluxException.
fn number_error(message: &str) -> OperationError {
    Failure {
        kind: FailureKind::Analytics(AnalyticsKind::InfluxV2),
        message: Some(message.into()),
        cause: Some(Box::new(failure(FailureKind::NumberFormat, message))),
        stacktrace: None,
    }
    .into()
}
/// Long.parseUnsignedLong's three message forms.
fn parse_unsigned_long(raw: &str) -> OperationResult<u64> {
    if raw.starts_with('-') {
        return Err(number_error(&format!(
            "Illegal leading minus sign on unsigned string {raw}."
        )));
    }
    raw.parse::<u64>().map_err(|e| {
        number_error(&if *e.kind() == std::num::IntErrorKind::PosOverflow {
            format!("String value {raw} exceeds range of unsigned long.")
        } else {
            format!("For input string: \"{raw}\"")
        })
    })
}
/// Annotated CSV is table-oriented: annotation/header blocks can repeat after
/// a blank line, and defaults apply before datatype conversion.
pub fn decode(body: &str) -> OperationResult<Vec<Record>> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(body.as_bytes());
    let (mut types, mut defaults, mut columns) = (Vec::<String>::new(), Vec::<String>::new(), Vec::<String>::new());
    let mut need_header = false;
    let mut out = Vec::new();
    for row in reader.records() {
        let row = row.map_err(|e| parse_error(&e.to_string()))?;
        let first = row.get(0).unwrap_or("");
        if matches!(first, "#datatype" | "#default" | "#group") {
            if !need_header {
                types.clear();
                defaults.clear();
                columns.clear();
                need_header = true;
            }
            if first == "#datatype" {
                types = row.iter().map(str::to_owned).collect();
            }
            if first == "#default" {
                defaults = row.iter().map(str::to_owned).collect();
            }
            continue;
        }
        if first.starts_with('#') {
            continue;
        }
        if row.iter().all(str::is_empty) {
            columns.clear();
            continue;
        }
        if row.get(1) == Some("error") {
            columns = row.iter().map(str::to_owned).collect();
            continue;
        }
        if columns.get(1).is_some_and(|s| s == "error") {
            return Err(failure(
                FailureKind::Analytics(AnalyticsKind::FluxQuery),
                row.get(1).unwrap_or(""),
            )
            .into());
        }
        if types.is_empty() {
            return Err(parse_error(
                "Unable to parse CSV response. FluxTable definition was not found.",
            ));
        }
        if columns.is_empty() || need_header {
            columns = row.iter().map(str::to_owned).collect();
            need_header = false;
            continue;
        }
        if columns.len() != row.len() || columns.len() != types.len() {
            return Err(parse_error("CSV row does not match table definition"));
        }
        let mut record = BTreeMap::new();
        for i in 1..columns.len() {
            let raw = &row[i];
            let raw = if raw.is_empty() {
                defaults.get(i).map(String::as_str).unwrap_or("")
            } else {
                raw
            };
            let value = if raw.is_empty() {
                if types[i] == "string" && defaults.get(i).is_some() {
                    Value::Text("".into())
                } else {
                    Value::Null
                }
            } else {
                match types[i].as_str() {
                    "double" => Value::Number(match raw {
                        "+Inf" => f64::INFINITY,
                        "-Inf" => f64::NEG_INFINITY,
                        _ => jnum::parse_double_with_error(raw).map_err(|m| number_error(&m))?,
                    }),
                    "long" => Value::Number(
                        raw.parse::<i64>()
                            .map_err(|_| number_error(&format!("For input string: \"{raw}\"")))?
                            as f64,
                    ),
                    "unsignedLong" => Value::Number(parse_unsigned_long(raw)? as i64 as f64),
                    _ => Value::Text(raw.into()),
                }
            };
            record.insert(columns[i].as_str().into(), value);
        }
        out.push(record);
    }
    Ok(out)
}
pub struct Influx2Tracker {
    pub client: Arc<Influx2Client>,
    pub read_only: bool,
}
impl Influx2Tracker {
    pub fn new(client: Arc<Influx2Client>, read_only: bool) -> OperationResult<Self> {
        register(
            &client.context,
            "com.malinskiy.marathon.analytics.external.influx2.InfluxDb2Tracker",
        )?;
        Ok(Self { client, read_only })
    }
    pub async fn track_test(&self, event: &TestEvent) -> OperationResult<()> {
        if self.read_only {
            return Ok(());
        }
        // D51: Kotlin logs a failed write per test here. `BackendGuard` logs it once per kind,
        // and only it can see that a write took a minute to fail.
        self.client.write(points::line(event, true)?).await
    }
}
impl TrackerInternalAdapter for Influx2Tracker {
    fn test<'a>(&'a self, event: &'a TestEvent) -> VendorFuture<'a, ()> {
        Box::pin(async move { self.track_test(event).await.map_err(vendor) })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(self.client.shutdown())
    }
}
