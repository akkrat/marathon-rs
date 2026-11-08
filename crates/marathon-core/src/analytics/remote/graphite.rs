//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/metrics/remote/graphite/GraphiteDataSource.kt
//! and core/src/main/kotlin/com/malinskiy/marathon/extension/StringExtensions.kt (withPrefix).
use super::*;
use crate::{analytics::MetricsContext, error::OperationResult, executor::shared::failure};
use jcompat::{
    jnum::{double_to_string, parse_double_with_error},
    logging::Level,
};
use std::sync::Arc;

/// WP 3.4b implements QueryableGraphiteClient: render GET on port 80, Java form
/// encoding, format_graphite(from), 60 s timeouts, nonempty raw response lines.
pub trait GraphiteQuery: Send + Sync {
    fn query<'a>(&'a self, target: &'a str, limit: Instant) -> ServiceFuture<'a, Vec<String>>;
}
pub struct GraphiteDataSource {
    pub client: Arc<dyn GraphiteQuery>,
    pub prefix: Option<String>,
    /// Where the debug line for a skipped `None` goes (D51). `None` logs nothing.
    pub context: Option<MetricsContext>,
}
pub fn with_prefix(path: &str, prefix: Option<&str>) -> String {
    match prefix.filter(|s| !s.is_empty()) {
        Some(prefix) => format!("{prefix}.{path}"),
        None => path.to_owned(),
    }
}
impl GraphiteDataSource {
    pub fn target(&self, percentile: Option<f64>) -> String {
        let prefix = self.prefix.as_deref().filter(|p| !p.is_empty());
        let node = 1 + prefix.map_or(0, |p| p.bytes().filter(|b| *b == b'.').count() + 1);
        let field = if percentile.is_some() { "duration" } else { "success" };
        let path = with_prefix(&format!("tests.*.*.*.*.*.{field}"), prefix);
        match percentile {
            Some(p) => format!("aliasByNode(summarize(nPercentile({path}, {}), '10years', 'average', true), {node})", double_to_string(p)),
            None => format!("aliasByNode(asPercent(summarize({path},'10years','sum',true),summarize({path},'10years','count',true)),{node})")
                .chars().filter(|c| !matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')).collect(),
        }
    }
}
pub fn parse_line(line: &str) -> OperationResult<(String, f64)> {
    let name = line.split_once(',').map_or(line, |p| p.0).replace("--", ".");
    let value = line.split_once('|').map_or(line, |p| p.1);
    let value =
        parse_double_with_error(value).map_err(|message| failure("java.lang.NumberFormatException", &message))?;
    Ok((name, value))
}
/// Whether a raw line is one series with no value at all: the query summarises over 10 years, and
/// `format=raw` prints `None` for a series with no points in the window. Kotlin's `toDouble` throws
/// here and loses the whole query (C38). `50,None` is still malformed, and fails as it did.
pub fn has_no_datapoint(line: &str) -> bool {
    line.split_once('|').map_or(line, |p| p.1) == "None"
}
impl GraphiteDataSource {
    /// D51: keeps every line that has a value, so one empty series can't discard the query.
    fn with_datapoints<'a>(&'a self, lines: &'a [String]) -> impl Iterator<Item = &'a str> {
        lines.iter().map(String::as_str).filter(|line| {
            let skip = has_no_datapoint(line);
            if let (true, Some(context)) = (skip, &self.context) {
                let text = format!("Skipping a Graphite series with no datapoints: {line}");
                context.log("MetricsProviderImpl", Level::Debug, text, None);
            }
            !skip
        })
    }
}
impl RemoteDataSource for GraphiteDataSource {
    fn request_all_success_rates(&self, limit: Instant) -> ServiceFuture<'_, Vec<SuccessRate>> {
        Box::pin(async move {
            let lines = self.client.query(&self.target(None), limit).await?;
            self.with_datapoints(&lines)
                .map(|line| {
                    let (test_name, value) = parse_line(line)?;
                    Ok(SuccessRate {
                        test_name,
                        mean: value / 100.0,
                    })
                })
                .collect()
        })
    }
    fn request_all_execution_times(&self, percentile: f64, limit: Instant) -> ServiceFuture<'_, Vec<ExecutionTime>> {
        Box::pin(async move {
            let lines = self.client.query(&self.target(Some(percentile)), limit).await?;
            self.with_datapoints(&lines)
                .map(|line| {
                    let (test_name, percentile) = parse_line(line)?;
                    Ok(ExecutionTime { test_name, percentile })
                })
                .collect()
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
