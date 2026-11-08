//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/metrics/remote/influx/InfluxDBDataSource.kt
use super::*;
use crate::{error::OperationResult, executor::shared::failure};
use jcompat::jnum::double_to_string;
use std::{collections::BTreeMap, sync::Arc};

/// Narrow DTO boundary for the WP 3.4b HTTP transport (P1: numeric coercion is f64).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Number(f64),
    Text(String),
}
#[derive(Clone, Debug, Default)]
pub struct Series {
    pub name: String,
    pub tags: BTreeMap<String, String>,
    pub columns: Vec<String>,
    pub values: Vec<Vec<Value>>,
}
#[derive(Clone, Debug, Default)]
pub struct QueryResult {
    pub error: Option<String>,
    pub results: Vec<StatementResult>,
}
#[derive(Clone, Debug, Default)]
pub struct StatementResult {
    pub error: Option<String>,
    pub series: Vec<Series>,
}
pub trait InfluxQuery: Send + Sync {
    fn query<'a>(&'a self, command: &'a String, database: &'a String) -> ServiceFuture<'a, QueryResult>;
    fn close(&self) -> ServiceFuture<'_, ()>;
}
pub struct InfluxDBDataSource {
    pub client: Arc<dyn InfluxQuery>,
    pub database: String,
    pub retention_policy: String,
}
impl InfluxDBDataSource {
    pub fn command(&self, percentile: Option<f64>, limit: Instant) -> String {
        let select = percentile.map_or_else(
            || "MEAN(\"success\")".to_owned(),
            |p| format!("PERCENTILE(\"duration\",{})", double_to_string(p)),
        );
        let mut command: String = format!("SELECT {select}\nFROM \"");
        command.push_str(&self.retention_policy.clone());
        command.push_str(&format!("\".\"tests\"\nWHERE time >= '{limit}'\nGROUP BY \"testname\""));
        command
    }
}
fn mapper_error(prefix: &str, error: String) -> crate::error::Failure {
    let mut message = String::from(prefix);
    message.push_str(&error);
    crate::error::Failure {
        class: "org.influxdb.InfluxDBMapperException".into(),
        message: Some(message),
        cause: None,
        stacktrace: None,
    }
}
pub fn map_result(result: QueryResult, column: &str) -> OperationResult<Vec<(String, f64)>> {
    let mut values = Vec::new();
    if let Some(error) = result.error {
        return Err(mapper_error("InfluxDB returned an error: ", error).into());
    }
    for statement in result.results {
        if let Some(error) = statement.error {
            return Err(mapper_error("InfluxDB returned an error with Series: ", error).into());
        }
        for series in statement.series.into_iter().filter(|s| s.name == "tests") {
            let index = series.columns.iter().position(|c| c == &String::from(column));
            for row in series.values {
                let value = index.and_then(|i| row.get(i));
                if let Some(Value::Text(_)) = value {
                    return Err(failure("java.lang.ClassCastException", "Expected numeric measurement").into());
                }
                if let (Some(name), Some(Value::Number(value))) = (series.tags.get(&String::from("testname")), value) {
                    values.push((name.clone(), *value));
                }
            }
        }
    }
    Ok(values)
}
impl RemoteDataSource for InfluxDBDataSource {
    fn request_all_success_rates(&self, limit: Instant) -> ServiceFuture<'_, Vec<SuccessRate>> {
        Box::pin(async move {
            Ok(map_result(
                self.client.query(&self.command(None, limit), &self.database).await?,
                "mean",
            )?
            .into_iter()
            .map(|(test_name, mean)| SuccessRate { test_name, mean })
            .collect())
        })
    }
    fn request_all_execution_times(&self, percentile: f64, limit: Instant) -> ServiceFuture<'_, Vec<ExecutionTime>> {
        Box::pin(async move {
            Ok(map_result(
                self.client
                    .query(&self.command(Some(percentile), limit), &self.database)
                    .await?,
                "percentile",
            )?
            .into_iter()
            .map(|(test_name, percentile)| ExecutionTime { test_name, percentile })
            .collect())
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        self.client.close()
    }
}
