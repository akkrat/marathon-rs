//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/html/HtmlSummaryReporter.kt
//! and report/html-report/src/main/kotlin/com/malinskiy/marathon/report/
//! {HtmlDevice,HtmlFullTest,HtmlIndex,HtmlPoolSummary,HtmlShortTest,HtmlTestLogDetails,Status}.kt.

use super::*;
use crate::{
    analytics::{ReporterRegistration, ReporterSlot},
    model::*,
    output::*,
    report::{ExecutionReport, PoolSummary, Reporter, Summary},
    runtime::*,
    time::WallClock,
};
use jcompat::{
    file_manager::{escape_utf16, relative_file},
    formatting::{DatePattern, format_date},
    jstring::lines,
    report_text::{escape_xml11, replace},
};
use std::sync::Arc;

fn safe_serial(device: &DeviceInfo) -> String {
    String::from_utf16_lossy(&escape_utf16(&device.serial_number.encode_utf16().collect::<Vec<_>>()))
}
fn id(test: &TestResult) -> String {
    test.test.to_test_name('.', '.')
}
pub fn filename(test: &TestResult) -> String {
    let mut units = escape_utf16(&id(test).encode_utf16().collect::<Vec<_>>());
    units.truncate(128);
    let mut name = String::from_utf16_lossy(&units);
    name.push_str(".html");
    name
}
fn status(status: TestStatus) -> WriterValue {
    match status {
        TestStatus::Passed => "passed",
        TestStatus::Ignored | TestStatus::AssumptionFailure => "ignored",
        _ => "failed",
    }
    .into()
}
fn duration(r: &TestResult) -> i64 {
    r.end_time.wrapping_sub(r.start_time)
}
pub fn short_test(r: &TestResult) -> WriterValue {
    object([
        ("id", string(&id(r))),
        ("package_name", string(&r.test.pkg)),
        ("filename", string(&filename(r))),
        ("class_name", string(&r.test.clazz)),
        ("name", string(&r.test.method)),
        ("duration_millis", duration(r).into()),
        ("status", status(r.status)),
        ("deviceId", string(&safe_serial(&r.device))),
    ])
}
pub fn pool_summary(p: &PoolSummary) -> WriterValue {
    object([
        ("id", string(&p.pool_id.name)),
        (
            "tests",
            WriterValue::Array(p.tests.iter().map(|r| short_test(r)).collect()),
        ),
        ("passed_count", (p.passed.len() as i32 as i64).into()),
        ("failed_count", (p.failed.len() as i32 as i64).into()),
        ("ignored_count", (p.ignored.len() as i32 as i64).into()),
        ("duration_millis", p.duration_millis.into()),
        (
            "devices",
            WriterValue::Array(
                p.devices
                    .iter()
                    .map(|d| {
                        object([
                            ("apiLevel", string(&d.operating_system.version)),
                            ("isTable", WriterValue::Bool(false)),
                            ("serial", string(&safe_serial(d))),
                            ("modelName", string(&d.model)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}
pub fn index(summary: &Summary, title: &String) -> VendorResult<WriterValue> {
    let totals: Vec<i64> = summary
        .pools
        .iter()
        .map(|p| p.tests.iter().map(|r| duration(r) as f64).sum::<f64>() as i64)
        .collect();
    let sum_count =
        |f: fn(&PoolSummary) -> i32| summary.pools.iter().fold(0i32, |sum, p| sum.wrapping_add(f(p))) as i64;
    let average = totals.iter().map(|t| *t as f64).sum::<f64>() / totals.len() as f64;
    let average = jcompat::jnum::kotlin_round_to_long(average)
        .ok_or_else(|| VendorError::Other(failure("java.lang.IllegalArgumentException", "Cannot round NaN value.")))?;
    Ok(object([
        ("title", string(title)),
        ("total_failed", sum_count(|p| p.failed.len() as i32).into()),
        ("total_flaky", sum_count(|p| p.flaky).into()),
        ("total_ignored", sum_count(|p| p.ignored.len() as i32).into()),
        ("total_passed", sum_count(|p| p.passed.len() as i32).into()),
        (
            "total_duration_millis",
            (summary
                .pools
                .iter()
                .flat_map(|p| &p.tests)
                .map(|r| duration(r) as f64)
                .sum::<f64>() as i64)
                .into(),
        ),
        ("average_duration_millis", average.into()),
        ("max_duration_millis", totals.iter().max().copied().unwrap_or(0).into()),
        ("min_duration_millis", totals.iter().min().copied().unwrap_or(0).into()),
        (
            "pools",
            WriterValue::Array(summary.pools.iter().map(pool_summary).collect()),
        ),
    ]))
}
pub fn logcat_html(log: &String) -> String {
    if log.is_empty() {
        return String::default();
    }
    let mut out = String::from("<div class=\"content\"><div class=\"card log\">");
    for line in lines(log) {
        // Kotlin Char.isLetter tests UTF-16 chars, so supplementary letters do not match.
        let first = line
            .encode_utf16()
            .collect::<Vec<_>>()
            .iter()
            .copied()
            .find(|u| jcompat::jstring::is_letter(*u))
            .and_then(|u| char::from_u32(u as u32));
        let css = match first {
            Some('V') => "verbose",
            Some('D') => "debug",
            Some('I') => "info",
            Some('W') => "warning",
            Some('E') => "error",
            Some('A') => "assert",
            _ => "default",
        };
        out.push_str("<div class=\"log__");
        out.push_str(css);
        out.push_str("\">");
        out.push_str(&escape_xml11(&line));
        out.push_str("</div>\n");
    }
    out.push_str("</div></div>\n");
    out
}
fn url(path: &JavaPath, root: &JavaPath) -> String {
    let mut out = String::from("../../../../");
    out.push_str(&replace(&relative_file(path, root, root), "#", "%23"));
    out
}
fn full_test(
    r: &TestResult,
    pool: &DevicePoolId,
    screenshot: String,
    videos: Vec<WriterValue>,
    log: String,
) -> WriterValue {
    object([
        ("pool_id", string(&pool.name)),
        ("package_name", string(&r.test.pkg)),
        ("class_name", string(&r.test.clazz)),
        ("name", string(&r.test.method)),
        ("id", string(&id(r))),
        ("filename", string(&filename(r))),
        ("duration_millis", duration(r).into()),
        ("status", status(r.status)),
        ("stacktrace", r.stacktrace.as_ref().map_or(WriterValue::Null, string)),
        ("deviceId", string(&safe_serial(&r.device))),
        (
            "diagnostic_video",
            WriterValue::Bool(r.device.device_features.contains(&DeviceFeature::Video)),
        ),
        (
            "diagnostic_screenshots",
            WriterValue::Bool(r.device.device_features.contains(&DeviceFeature::Screenshot)),
        ),
        ("screenshot", string(&screenshot)),
        ("videos", WriterValue::Array(videos)),
        ("log_file", string(&log)),
    ])
}
fn page(path: &JavaPath, html: &JavaPath, data: String, log: String, date: &String) -> VendorResult<()> {
    let parent = JavaPath(path.0[..path.0.iter().rposition(|u| *u == 47).unwrap_or(0)].to_vec());
    let mut prefix = relative_file(html, &parent, html);
    if !prefix.is_empty() {
        prefix.push('/')
    }
    let mut text = String::from(include_str!("../../../../fixtures/reporters/html/index.html"));
    for (key, value) in [
        ("${relative_path}", &prefix),
        ("${data_json}", &data),
        ("${log}", &log),
        ("${date}", date),
    ] {
        text = replace(&text, key, value)
    }
    write(path, text.as_bytes())
}
fn assignment(name: &str, value: &WriterValue) -> VendorResult<String> {
    let mut out = String::from(name);
    out.push_str(" = ");
    out.push_str(&gson(value)?);
    Ok(out)
}
#[derive(Clone)]
pub struct HtmlSummaryReporter {
    io: Arc<dyn VendorRuntime>,
    files: Arc<dyn OutputFiles>,
    clock: Arc<dyn WallClock>,
    title: String,
    root: JavaPath,
}
impl HtmlSummaryReporter {
    pub fn new(
        io: Arc<dyn VendorRuntime>,
        files: Arc<dyn OutputFiles>,
        clock: Arc<dyn WallClock>,
        title: String,
        process_dir: JavaPath,
    ) -> Self {
        let root = jcompat::file_manager::absolute_file_path_utf16(&files.output_dir().0, &process_dir.0);
        Self {
            root,
            io,
            files,
            clock,
            title,
        }
    }
    pub fn registration(self) -> ReporterRegistration {
        ReporterRegistration {
            slot: ReporterSlot::Html,
            reporter: Arc::new(self),
        }
    }
    async fn run(self, report: ExecutionReport) -> VendorResult<()> {
        let title = self.title.clone();
        let (summary, index) = blocking(self.io.as_ref(), move || {
            let summary = report.summary().clone();
            let data = if summary.pools.is_empty() {
                None
            } else {
                Some(assignment("window.mainData", &index(&summary, &title)?)?)
            };
            Ok((summary, data))
        })
        .await?;
        let Some(index) = index else { return Ok(()) };
        let date = format_date(
            self.clock
                .millis("core.HtmlSummaryReporter.date")
                .map_err(clock_error)?,
            DatePattern::Html,
        );
        let html = self
            .files
            .create_folder(FolderType::Html, None, None)
            .await
            .map_err(vendor_error)?;
        let (h, d) = (html.clone(), date.clone());
        blocking(self.io.as_ref(), move || {
            write(
                &child(&h, "app.min.js"),
                include_bytes!("../../../../fixtures/reporters/html/app.min.js"),
            )?;
            write(
                &child(&h, "app.min.css"),
                include_bytes!("../../../../fixtures/reporters/html/app.min.css"),
            )?;
            page(&child(&h, "index.html"), &h, index, String::default(), &d)?;
            mkdir(&child(&h, "pools"))
        })
        .await?;
        for pool in summary.pools {
            let (h, d, p) = (html.clone(), date.clone(), pool.clone());
            let dirs = blocking(self.io.as_ref(), move || {
                let pools = child(&h, "pools");
                let mut name = p.pool_id.name.clone();
                name.push_str(".html");
                page(
                    &child_file(&pools, &name.encode_utf16().collect::<Vec<_>>()),
                    &h,
                    assignment("window.pool", &pool_summary(&p))?,
                    String::default(),
                    &d,
                )?;
                p.tests
                    .iter()
                    .map(|r| {
                        let dir = child_file(
                            &child_file(&pools, &p.pool_id.name.encode_utf16().collect::<Vec<_>>()),
                            &safe_serial(&r.device).encode_utf16().collect::<Vec<_>>(),
                        );
                        mkdir(&dir)?;
                        Ok(dir)
                    })
                    .collect::<VendorResult<Vec<_>>>()
            })
            .await?;
            let mut full = Vec::new();
            for (r, dir) in pool.tests.iter().zip(dirs) {
                let target = |kind| FileTarget {
                    kind,
                    pool: pool.pool_id.clone(),
                    device: r.device.clone(),
                    test: Some(r.test.clone()),
                    test_batch_id: Some(r.test_batch_id.clone()),
                    id: None,
                    chunk: None,
                };
                let shot = self
                    .files
                    .create_file(&target(FileType::Screenshot))
                    .await
                    .map_err(vendor_error)?;
                let (root, r1) = (self.root.clone(), r.clone());
                let (screenshot, videos) = blocking(self.io.as_ref(), move || {
                    let screenshot = if r1.device.device_features.contains(&DeviceFeature::Screenshot) && exists(&shot)
                    {
                        url(&shot, &root)
                    } else {
                        String::default()
                    };
                    let videos = if r1.device.device_features.contains(&DeviceFeature::Video) {
                        r1.attachments
                            .iter()
                            .filter(|a| a.kind == AttachmentType::Video && exists(&a.file))
                            .map(|a| string(&url(&a.file, &root)))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    Ok((screenshot, videos))
                })
                .await?;
                let log = self
                    .files
                    .create_file(&target(FileType::Log))
                    .await
                    .map_err(vendor_error)?;
                let (root, r, p) = (self.root.clone(), r.clone(), pool.pool_id.clone());
                full.push(
                    blocking(self.io.as_ref(), move || {
                        let log = if exists(&log) {
                            url(&log, &root)
                        } else {
                            String::default()
                        };
                        let data = full_test(&r, &p, screenshot, videos, log.clone());
                        Ok((r, dir, data, log))
                    })
                    .await?,
                );
            }
            for (r, dir, data, log) in full {
                let (h, d, p) = (html.clone(), date.clone(), pool.pool_id.clone());
                blocking(self.io.as_ref(), move || {
                    let filename = filename(&r);
                    page(
                        &child_file(&dir, &filename.encode_utf16().collect::<Vec<_>>()),
                        &h,
                        assignment("window.test", &data)?,
                        logcat_html(&r.stacktrace.clone().unwrap_or_default()),
                        &d,
                    )?;
                    let logs = child(&dir, "logs");
                    mkdir(&logs)?;
                    let mut path = String::from("../");
                    path.push_str(&log);
                    let data = object([
                        ("pool_id", string(&p.name)),
                        ("test_id", string(&id(&r))),
                        ("display_name", string(&r.test.method)),
                        ("device_id", string(&safe_serial(&r.device))),
                        ("log_path", string(&path)),
                    ]);
                    page(
                        &child_file(&logs, &filename.encode_utf16().collect::<Vec<_>>()),
                        &h,
                        assignment("window.logs", &data)?,
                        String::default(),
                        &d,
                    )
                })
                .await?;
            }
        }
        Ok(())
    }
}
impl Reporter for HtmlSummaryReporter {
    fn generate<'a>(&'a self, report: &'a ExecutionReport) -> ServiceFuture<'a, ()> {
        let this = self.clone();
        let report = report.clone();
        Box::pin(async move {
            on_io(self.io.as_ref(), Box::pin(this.run(report)))
                .await
                .map_err(operation_error)
        })
    }
}
