//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/device/DeviceInfoJsonReporter.kt,
//! report/bill/BillingReporter.kt, report/raw/RawJsonReporter.kt,
//! report/test/TestJsonReporter.kt and report/stdout/StdoutReporter.kt.

use super::{json as schema, projection as p, *};
use crate::{
    analytics::{ReporterRegistration, ReporterSlot},
    artifact_io::*,
    error::*,
    logging::OutputRecord,
    output::*,
    time::{Timer, WallClock},
};
use jcompat::{
    formatting::format_stdout_duration,
    jtime::{instant_duration_millis, instant_to_epoch_milli},
    logging::{Level, Logger},
};
use std::path::PathBuf;
use std::{collections::BTreeMap, io::Write};

#[derive(Clone)]
pub struct ReporterFiles {
    pub io: Arc<dyn crate::runtime::VendorRuntime>,
    pub files: Arc<dyn OutputFiles>,
    /// The process working directory, for FileSerializer's absolute paths.
    pub cwd: PathBuf,
}
impl ReporterFiles {
    pub(super) async fn write(&self, path: PathBuf, text: String) -> OperationResult<()> {
        blocking(self.io.as_ref(), move || {
            validate_path(&path)?;
            let mut file = std::fs::File::create(&path).map_err(|e| file_open_error(e, &path))?;
            file.write_all(text.as_bytes()).map_err(io_error)
        })
        .await
        .map_err(operation_error)
    }
    fn target(&self, kind: FileType, pool: &DevicePoolId, device: &DeviceInfo, test: Option<&Test>) -> FileTarget {
        FileTarget {
            kind,
            pool: pool.clone(),
            device: device.clone(),
            test: test.cloned(),
            test_batch_id: None,
            id: None,
            chunk: None,
        }
    }
}
fn json(value: impl serde::Serialize) -> OperationResult<String> {
    serde_json::to_string(&value)
        .map_err(|e| crate::executor::shared::failure("java.lang.IllegalArgumentException", &e.to_string()).into())
}
fn arithmetic(value: Option<i64>) -> OperationResult<i64> {
    value.ok_or_else(|| crate::executor::shared::failure("java.lang.ArithmeticException", "long overflow").into())
}

pub struct DeviceInfoJsonReporter {
    pub output: ReporterFiles,
}
impl DeviceInfoJsonReporter {
    pub fn new(output: ReporterFiles) -> Self {
        Self { output }
    }
}
impl Reporter for DeviceInfoJsonReporter {
    fn generate<'a>(&'a self, report: &'a ExecutionReport) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            for e in report.device_connected_events() {
                let text = json(schema::Device::from(&e.device))?;
                let target = self.output.target(FileType::DeviceInfo, &e.pool_id, &e.device, None);
                let path = self.output.files.create_file(&target).await?;
                self.output.write(path, text).await?;
            }
            Ok(())
        })
    }
}
pub struct TestJsonReporter {
    pub output: ReporterFiles,
}
impl TestJsonReporter {
    pub fn new(output: ReporterFiles) -> Self {
        Self { output }
    }
}
impl Reporter for TestJsonReporter {
    fn generate<'a>(&'a self, report: &'a ExecutionReport) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            for e in report.test_events().iter().filter(|e| e.final_result) {
                let target = self
                    .output
                    .target(FileType::TestResult, &e.pool_id, &e.device, Some(&e.test_result.test));
                let path = self.output.files.create_file(&target).await?;
                let text = json(schema::TestResultJson::new(&e.test_result, &self.output.cwd))?;
                self.output.write(path, text).await?;
            }
            Ok(())
        })
    }
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RawTest<'a> {
    package: &'a str,
    class: &'a str,
    method: &'a str,
    meta_properties: Vec<RawProperty<'a>>,
    device_serial: &'a str,
    status: &'static str,
    ignored: bool,
    success: bool,
    timestamp: i64,
    duration: i64,
}
#[derive(serde::Serialize)]
struct RawProperty<'a> {
    name: &'a str,
    #[serde(serialize_with = "string_map")]
    values: Vec<(&'a str, String)>,
}
fn string_map<S: serde::Serializer>(entries: &[(&str, String)], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_map(entries.iter().map(|(key, value)| (key, value)))
}
pub struct RawJsonReporter {
    pub output: ReporterFiles,
}
impl RawJsonReporter {
    pub fn new(output: ReporterFiles) -> Self {
        Self { output }
    }
}
impl Reporter for RawJsonReporter {
    fn generate<'a>(&'a self, report: &'a ExecutionReport) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            let mut values = Vec::new();
            for e in report.test_events() {
                let r = &e.test_result;
                let mut properties = Vec::new();
                for prop in &r.test.meta_properties {
                    let mut entries = Vec::new();
                    for (key, value) in prop.values.entries() {
                        let text = if matches!(value, MetaValue::Null) {
                            String::default()
                        } else {
                            p::kotlin_meta(value).render()
                        };
                        entries.push((key.as_str(), text));
                    }
                    properties.push(RawProperty {
                        name: &prop.name,
                        values: entries,
                    });
                }
                values.push(RawTest {
                    package: &r.test.pkg,
                    class: &r.test.clazz,
                    method: &r.test.method,
                    meta_properties: properties,
                    device_serial: &e.device.serial_number,
                    status: p::status(r.status),
                    ignored: r.is_ignored(),
                    success: r.is_success(),
                    timestamp: r.start_time,
                    duration: r.end_time.wrapping_sub(r.start_time),
                });
            }
            let path = self.output.files.create_test_result_file("raw.json").await?;
            self.output.write(path, json(values)?).await
        })
    }
}
#[derive(serde::Serialize)]
struct Bill<'a> {
    device: schema::Device<'a>,
    pool: schema::Pool<'a>,
    start: i64,
    end: i64,
    duration: i64,
}
pub struct BillingReporter {
    pub output: ReporterFiles,
    default_start: Instant,
    logger: Logger,
    log: ServiceLog,
}
impl BillingReporter {
    pub fn new(output: ReporterFiles, clock: &dyn WallClock, logger: Logger, log: ServiceLog) -> OperationResult<Self> {
        Ok(Self {
            output,
            default_start: clock
                .instant("core.BillingReporter.defaultStart")
                .map_err(clock_error)
                .map_err(operation_error)?,
            logger,
            log,
        })
    }
}
impl Reporter for BillingReporter {
    fn generate<'a>(&'a self, report: &'a ExecutionReport) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            let mut starts: Vec<(String, Instant)> = Vec::new();
            let mut ends: Vec<(String, Instant)> = Vec::new();
            let key = |i: Instant| (i.seconds(), i.nanos());
            for e in report.device_preparing_events() {
                if let Some((_, start)) = starts.iter_mut().find(|(s, _)| *s == e.serial_number) {
                    if key(e.finish) < key(*start) {
                        *start = e.finish;
                    }
                } else {
                    starts.push((e.serial_number.clone(), e.finish));
                }
            }
            for e in report.test_events() {
                if let Some((_, end)) = ends.iter_mut().find(|(s, _)| *s == e.device.serial_number) {
                    if key(e.instant) > key(*end) {
                        *end = e.instant;
                    }
                } else {
                    ends.push((e.device.serial_number.clone(), e.instant));
                }
            }
            let mut serials: Vec<_> = starts.iter().map(|(s, _)| s.clone()).collect();
            for (s, _) in &ends {
                if !serials.contains(s) {
                    serials.push(s.clone());
                }
            }
            let mut bills = Vec::new();
            for serial in serials {
                let Some((_, end)) = ends.iter().find(|(s, _)| *s == serial) else {
                    continue;
                };
                let start = starts
                    .iter()
                    .find(|(s, _)| *s == serial)
                    .map_or(self.default_start, |(_, s)| *s);
                let Some(e) = report
                    .device_connected_events()
                    .iter()
                    .rev()
                    .find(|e| e.device.serial_number == serial)
                else {
                    let (logger, log) = (self.logger.clone(), self.log.clone());
                    blocking(self.output.io.as_ref(), move || {
                        log(
                            &logger,
                            Level::Warn,
                            "Failure to process device bill: missing timeline event",
                            None,
                        );
                        Ok(())
                    })
                    .await
                    .map_err(operation_error)?;
                    continue;
                };
                let value = Bill {
                    device: (&e.device).into(),
                    pool: schema::Pool { name: &e.pool_id.name },
                    start: arithmetic(instant_to_epoch_milli(start))?,
                    end: arithmetic(instant_to_epoch_milli(*end))?,
                    duration: arithmetic(instant_duration_millis(start, *end))?,
                };
                bills.push((e, value));
            }
            for (e, bill) in bills {
                let text = json(bill)?;
                let target = self.output.target(FileType::Bill, &e.pool_id, &e.device, None);
                let path = self.output.files.create_file(&target).await?;
                self.output.write(path, text).await?;
            }
            Ok(())
        })
    }
}
pub struct StdoutReporter {
    pub io: Arc<dyn crate::runtime::VendorRuntime>,
    pub timer: Arc<dyn Timer>,
    pub sink: Arc<dyn ConsoleSink>,
}
impl StdoutReporter {
    pub fn new(io: Arc<dyn crate::runtime::VendorRuntime>, timer: Arc<dyn Timer>, sink: Arc<dyn ConsoleSink>) -> Self {
        Self { io, timer, sink }
    }
}
impl Reporter for StdoutReporter {
    fn generate<'a>(&'a self, report: &'a ExecutionReport) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            let pools = &report.summary().pools;
            if pools.is_empty() {
                return Ok(());
            }
            let mut text = String::from("Marathon run finished:\n");
            for p in pools {
                text.push_str("Device pool ");
                text.push_str(&p.pool_id.name.clone());
                text.push_str(":\n");
                text.push_str(&format!(
                    "\t{} passed, {} failed, {} ignored tests\n",
                    p.passed.len(),
                    p.failed.len(),
                    p.ignored.len()
                ));
                if !p.failed.is_empty() {
                    text.push_str("\tFailed tests:\n");
                    let mut names = p.failed.clone();
                    names.sort();
                    names.dedup();
                    for name in names {
                        text.push_str("\t\t");
                        text.push_str(&name);
                        text.push('\n');
                    }
                }
                text.push_str(&format!(
                    "\tFlakiness overhead: {}\n",
                    format_stdout_duration(p.raw_duration_millis.wrapping_sub(p.duration_millis))
                ));
                text.push_str(&format!(
                    "\tRaw: {} passed, {} failed, {} ignored, {} incomplete tests\n",
                    p.raw_passed.len(),
                    p.raw_failed.len(),
                    p.raw_ignored.len(),
                    p.raw_incomplete.len()
                ));
                for (names, title, verb) in [
                    (&p.raw_failed, "Failed", "failed"),
                    (&p.raw_incomplete, "Incomplete", "incomplete"),
                ] {
                    if !names.is_empty() {
                        text.push_str(&format!("\t{title} tests:\n"));
                        let mut counts = BTreeMap::new();
                        for name in names {
                            *counts.entry(name).or_insert(0usize) += 1;
                        }
                        for (name, count) in counts {
                            text.push_str("\t\t");
                            text.push_str(&name.clone());
                            text.push_str(&format!(" {verb} {count} time(s)\n"));
                        }
                    }
                }
            }
            text.push_str(&format!(
                "Total time: {}\n\n",
                format_stdout_duration(
                    self.timer
                        .elapsed_time_millis()
                        .map_err(clock_error)
                        .map_err(operation_error)?
                )
            ));
            let sink = self.sink.clone();
            on_io(
                self.io.as_ref(),
                Box::pin(async move { sink.write(vec![OutputRecord::Raw(text)]).await }),
            )
            .await
            .map_err(operation_error)
        })
    }
}

/// The factory sorts these with 3.9c's Timeline/Allure/Html registrations.
pub fn reporter_registrations(
    device: DeviceInfoJsonReporter,
    bill: BillingReporter,
    junit: super::JUnitReporter,
    raw: RawJsonReporter,
    test: TestJsonReporter,
    stdout: StdoutReporter,
) -> Vec<ReporterRegistration> {
    vec![
        ReporterRegistration {
            slot: ReporterSlot::DeviceInfo,
            reporter: Arc::new(device),
        },
        ReporterRegistration {
            slot: ReporterSlot::Billing,
            reporter: Arc::new(bill),
        },
        ReporterRegistration {
            slot: ReporterSlot::JUnit,
            reporter: Arc::new(junit),
        },
        ReporterRegistration {
            slot: ReporterSlot::Raw,
            reporter: Arc::new(raw),
        },
        ReporterRegistration {
            slot: ReporterSlot::TestJson,
            reporter: Arc::new(test),
        },
        ReporterRegistration {
            slot: ReporterSlot::Stdout,
            reporter: Arc::new(stdout),
        },
    ]
}
