//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/timeline/TimelineReporter.kt
//! and TimelineSummaryProvider.kt; report/execution-timeline/src/main/kotlin/com/malinskiy/marathon/report/timeline/
//! {Data,ExecutionStats,Measure,MetricType,TimelineExecutionResult}.kt.

use super::*;
use crate::{
    analytics::{ReporterRegistration, ReporterSlot},
    model::TestStatus,
    report::{ExecutionReport, Reporter},
    runtime::{ServiceFuture, VendorRuntime},
};
use jcompat::logging::{Logger, MarathonLogging};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MetricType {
    Failure,
    Passed,
    Ignored,
    Incomplete,
    AssumptionFailure,
    DeviceProviderInit,
    DevicePrepare,
}
#[derive(Clone, Debug)]
pub struct Data {
    pub test_name: String,
    pub metric_type: MetricType,
    pub start_date: i64,
    pub end_date: i64,
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DataJson<'a> {
    test_name: &'a str,
    metric_type: MetricType,
    start_date: i64,
    end_date: i64,
    expected_value: f64,
    variance: f64,
}
impl serde::Serialize for Data {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(
            &DataJson {
                test_name: &self.test_name,
                metric_type: self.metric_type,
                start_date: self.start_date,
                end_date: self.end_date,
                expected_value: 0.0,
                variance: 0.0,
            },
            serializer,
        )
    }
}
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionStats {
    pub idle_time_millis: i64,
    pub average_test_execution_time_millis: i64,
}
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Measure {
    pub measure: String,
    pub stats: ExecutionStats,
    pub data: Vec<Data>,
}
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineExecutionResult {
    pub passed_tests: i32,
    pub failed_tests: i32,
    pub ignored_tests: i32,
    pub execution_stats: ExecutionStats,
    pub measures: Vec<Measure>,
}
pub struct TimelineSummaryProvider {
    pub logger: Logger,
}
impl TimelineSummaryProvider {
    pub fn new(registry: &mut MarathonLogging) -> Self {
        Self {
            logger: registry.logger("TimelineSummaryProvider", None),
        }
    }
    pub fn generate(report: &ExecutionReport) -> VendorResult<TimelineExecutionResult> {
        // Instant.toEpochMilli overflows the same way whichever event Kotlin maps first.
        let millis = |start: &jcompat::jtime::Instant, finish: &jcompat::jtime::Instant| match (
            start.to_epoch_millis(),
            finish.to_epoch_millis(),
        ) {
            (Some(start), Some(finish)) => Ok((start, finish)),
            _ => Err(VendorError::Other(failure(
                "java.lang.ArithmeticException",
                "long overflow",
            ))),
        };
        let preparing = report
            .device_preparing_events()
            .iter()
            .map(|e| Ok((&e.serial_number, millis(&e.start, &e.finish)?)))
            .collect::<VendorResult<Vec<_>>>()?;
        let provider = report
            .device_provider_preparing_events()
            .iter()
            .map(|e| Ok((&e.serial_number, millis(&e.start, &e.finish)?)))
            .collect::<VendorResult<Vec<_>>>()?;
        let mut keys = Vec::new();
        for key in report
            .device_connected_events()
            .iter()
            .map(|e| &e.device.serial_number)
            .chain(report.device_preparing_events().iter().map(|e| &e.serial_number))
            .chain(
                report
                    .device_provider_preparing_events()
                    .iter()
                    .map(|e| &e.serial_number),
            )
            .chain(report.test_events().iter().map(|e| &e.device.serial_number))
        {
            if !keys.contains(key) {
                keys.push(key.clone())
            }
        }
        let measures: Vec<_> = keys
            .into_iter()
            .map(|serial| {
                let mut data: Vec<_> = report
                    .test_events()
                    .iter()
                    .filter(|e| e.device.serial_number == serial)
                    .map(|e| {
                        let r = &e.test_result;
                        let mut name = r.test.clazz.clone();
                        name.push('.');
                        name.push_str(&r.test.method.clone());
                        let metric_type = match r.status {
                            TestStatus::Failure => MetricType::Failure,
                            TestStatus::Passed => MetricType::Passed,
                            TestStatus::Ignored => MetricType::Ignored,
                            TestStatus::Incomplete => MetricType::Incomplete,
                            TestStatus::AssumptionFailure => MetricType::AssumptionFailure,
                        };
                        Data {
                            test_name: name,
                            metric_type,
                            start_date: r.start_time,
                            end_date: r.end_time,
                        }
                    })
                    .collect();
                data.extend(
                    preparing
                        .iter()
                        .filter(|(s, _)| **s == serial)
                        .map(|&(_, (start_date, end_date))| Data {
                            test_name: "DEVICE_PREPARE".into(),
                            metric_type: MetricType::DevicePrepare,
                            start_date,
                            end_date,
                        }),
                );
                data.extend(
                    provider
                        .iter()
                        .filter(|(s, _)| **s == serial)
                        .map(|&(_, (start_date, end_date))| Data {
                            test_name: "DEVICE_PROVIDER_INIT".into(),
                            metric_type: MetricType::DeviceProviderInit,
                            start_date,
                            end_date,
                        }),
                );
                data.sort_by_key(|d| d.start_date);
                let idle = data
                    .windows(2)
                    .fold(0i64, |a, w| a.wrapping_add(w[1].start_date.wrapping_sub(w[0].end_date)));
                let average = data
                    .iter()
                    .map(|d| d.end_date.wrapping_sub(d.start_date) as f64)
                    .sum::<f64>()
                    / data.len() as f64;
                Measure {
                    measure: serial,
                    stats: ExecutionStats {
                        idle_time_millis: idle,
                        average_test_execution_time_millis: average as i64,
                    },
                    data,
                }
            })
            .collect();
        let idle = measures
            .iter()
            .fold(0i64, |a, m| a.wrapping_add(m.stats.idle_time_millis));
        let avg = measures
            .iter()
            .map(|m| m.stats.average_test_execution_time_millis as f64)
            .sum::<f64>()
            / measures.len() as f64;
        let passed = report
            .test_events()
            .iter()
            .filter(|e| e.test_result.is_success())
            .count() as i32;
        Ok(TimelineExecutionResult {
            passed_tests: passed,
            failed_tests: (report.test_events().len() as i32).wrapping_sub(passed),
            ignored_tests: report
                .test_events()
                .iter()
                .filter(|e| e.test_result.is_ignored())
                .count() as i32,
            execution_stats: ExecutionStats {
                idle_time_millis: idle,
                average_test_execution_time_millis: avg as i64,
            },
            measures,
        })
    }
}
#[derive(Clone)]
pub struct TimelineReporter {
    io: Arc<dyn VendorRuntime>,
    root: PathBuf,
    pub logger: Logger,
    pub provider: Arc<TimelineSummaryProvider>,
}
impl TimelineReporter {
    pub fn new(io: Arc<dyn VendorRuntime>, root: PathBuf, registry: &mut MarathonLogging) -> Self {
        let provider = Arc::new(TimelineSummaryProvider::new(registry));
        Self {
            io,
            root,
            provider,
            logger: registry.logger("TimelineSummaryProvider", None),
        }
    }
    pub fn registration(self) -> ReporterRegistration {
        ReporterRegistration {
            slot: ReporterSlot::Timeline,
            reporter: Arc::new(self),
        }
    }
}
impl Reporter for TimelineReporter {
    fn generate<'a>(&'a self, report: &'a ExecutionReport) -> ServiceFuture<'a, ()> {
        let root = self.root.clone();
        let report = report.clone();
        Box::pin(async move {
            blocking(self.io.as_ref(), move || {
                let dir = child(&child(&root, "/html"), "/timeline");
                mkdir(&dir)?;
                for (name, bytes) in [
                    (
                        "chart.css",
                        include_bytes!("../../../../fixtures/reporters/timeline/chart.css").as_slice(),
                    ),
                    (
                        "chart.js",
                        include_bytes!("../../../../fixtures/reporters/timeline/chart.js").as_slice(),
                    ),
                    (
                        "iframeResizer.contentWindow.min.js",
                        include_bytes!("../../../../fixtures/reporters/timeline/iframeResizer.contentWindow.min.js")
                            .as_slice(),
                    ),
                ] {
                    write(&child(&dir, name), bytes)?;
                }
                let json = html_json(&TimelineSummaryProvider::generate(&report)?)?;
                let text = jcompat::report_text::replace(
                    &String::from(include_str!("../../../../fixtures/reporters/timeline/index.html")),
                    "${dataset}",
                    &json,
                );
                write(&child(&dir, "index.html"), text.as_bytes())
            })
            .await
            .map_err(operation_error)
        })
    }
}
