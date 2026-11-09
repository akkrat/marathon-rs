//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/metrics/remote/influx2/InfluxDB2DataSource.kt
use super::influx::Value;
use super::*;
use crate::{error::OperationResult, executor::shared::failure};
use jcompat::jnum::double_to_string;
use marathon_error::FailureKind;
use std::{collections::BTreeMap, sync::Arc};

pub type Record = BTreeMap<String, Value>;
pub trait FluxQuery: Send + Sync {
    fn query<'a>(&'a self, flux: &'a str) -> ServiceFuture<'a, Vec<Record>>;
    fn close(&self) -> ServiceFuture<'_, ()>;
}
pub struct InfluxDB2DataSource {
    pub client: Arc<dyn FluxQuery>,
    pub bucket: String,
}
impl InfluxDB2DataSource {
    pub fn command(&self, percentile: Option<f64>, limit: Instant) -> String {
        let (indent, field, aggregate) = match percentile {
            Some(p) => (
                "  ",
                "duration",
                format!(
                    "quantile(q: {}, method: \"exact_selector\")",
                    double_to_string(p / 100.0)
                ),
            ),
            None => ("              ", "success", "mean()".to_owned()),
        };
        let mut command: String = "from(bucket: \"".into();
        command.push_str(&self.bucket.clone());
        command.push_str(&format!("\")\n{indent}|> range(start: {})\n{indent}|> filter(fn: (r) => r[\"_measurement\"] == \"tests\" and r[\"_field\"] == \"{field}\")\n{indent}|> keep(columns: [\"testname\", \"_value\"])\n{indent}|> {aggregate}\n{indent}|> group(columns: [\"testname\"])", limit.seconds()));
        command
    }
}
pub fn map_records(records: Vec<Record>) -> OperationResult<Vec<(String, f64)>> {
    let mut values = Vec::new();
    for record in records {
        let name = record.get(&String::from("testname"));
        let value = record.get(&String::from("_value"));
        match (name, value) {
            (None | Some(Value::Null), _) | (_, None | Some(Value::Null)) => {}
            (Some(Value::Text(name)), Some(Value::Number(value))) => values.push((name.clone(), *value)),
            _ => {
                return Err(failure(FailureKind::ClassCast, "Expected testname and numeric measurement").into());
            }
        }
    }
    Ok(values)
}
impl RemoteDataSource for InfluxDB2DataSource {
    fn request_all_success_rates(&self, limit: Instant) -> ServiceFuture<'_, Vec<SuccessRate>> {
        Box::pin(async move {
            Ok(map_records(self.client.query(&self.command(None, limit)).await?)?
                .into_iter()
                .map(|(test_name, mean)| SuccessRate { test_name, mean })
                .collect())
        })
    }
    fn request_all_execution_times(&self, percentile: f64, limit: Instant) -> ServiceFuture<'_, Vec<ExecutionTime>> {
        Box::pin(async move {
            Ok(
                map_records(self.client.query(&self.command(Some(percentile), limit)).await?)?
                    .into_iter()
                    .map(|(test_name, percentile)| ExecutionTime { test_name, percentile })
                    .collect(),
            )
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        self.client.close()
    }
}
