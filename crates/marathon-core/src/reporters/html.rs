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
    file_manager::{escape, relative_file},
    formatting::{DatePattern, format_date},
    jstring::lines,
    report_text::{escape_xml11, replace},
};
use marathon_error::FailureKind;
use std::path::PathBuf;
use std::sync::Arc;

fn safe_serial(device: &DeviceInfo) -> String {
    escape(&device.serial_number)
}
fn id(test: &TestResult) -> String {
    test.test.to_test_name('.', '.')
}
pub fn filename(test: &TestResult) -> String {
    let mut name = jcompat::jstring::take_utf16(&escape(&id(test)), 128);
    name.push_str(".html");
    name
}
fn status(status: TestStatus) -> &'static str {
    match status {
        TestStatus::Passed => "passed",
        TestStatus::Ignored | TestStatus::AssumptionFailure => "ignored",
        _ => "failed",
    }
}
fn duration(r: &TestResult) -> i64 {
    r.end_time.wrapping_sub(r.start_time)
}
#[derive(serde::Serialize)]
pub struct HtmlShortTest {
    id: String,
    package_name: String,
    filename: String,
    class_name: String,
    name: String,
    duration_millis: i64,
    status: &'static str,
    #[serde(rename = "deviceId")]
    device_id: String,
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HtmlDevice {
    api_level: String,
    is_table: bool,
    serial: String,
    model_name: String,
}
#[derive(serde::Serialize)]
pub struct HtmlPoolSummary {
    id: String,
    tests: Vec<HtmlShortTest>,
    passed_count: i32,
    failed_count: i32,
    ignored_count: i32,
    duration_millis: i64,
    devices: Vec<HtmlDevice>,
}
#[derive(serde::Serialize)]
pub struct HtmlIndex {
    title: String,
    total_failed: i64,
    total_flaky: i64,
    total_ignored: i64,
    total_passed: i64,
    total_duration_millis: i64,
    average_duration_millis: i64,
    max_duration_millis: i64,
    min_duration_millis: i64,
    pools: Vec<HtmlPoolSummary>,
}
pub fn short_test(r: &TestResult) -> HtmlShortTest {
    HtmlShortTest {
        id: id(r),
        package_name: r.test.pkg.clone(),
        filename: filename(r),
        class_name: r.test.clazz.clone(),
        name: r.test.method.clone(),
        duration_millis: duration(r),
        status: status(r.status),
        device_id: safe_serial(&r.device),
    }
}
pub fn pool_summary(p: &PoolSummary) -> HtmlPoolSummary {
    HtmlPoolSummary {
        id: p.pool_id.name.clone(),
        tests: p.tests.iter().map(|r| short_test(r)).collect(),
        passed_count: p.passed.len() as i32,
        failed_count: p.failed.len() as i32,
        ignored_count: p.ignored.len() as i32,
        duration_millis: p.duration_millis,
        devices: p
            .devices
            .iter()
            .map(|d| HtmlDevice {
                api_level: d.operating_system.version.clone(),
                is_table: false,
                serial: safe_serial(d),
                model_name: d.model.clone(),
            })
            .collect(),
    }
}
pub fn index(summary: &Summary, title: &str) -> VendorResult<HtmlIndex> {
    let totals: Vec<i64> = summary
        .pools
        .iter()
        .map(|p| p.tests.iter().map(|r| duration(r) as f64).sum::<f64>() as i64)
        .collect();
    let sum_count =
        |f: fn(&PoolSummary) -> i32| summary.pools.iter().fold(0i32, |sum, p| sum.wrapping_add(f(p))) as i64;
    let average = totals.iter().map(|t| *t as f64).sum::<f64>() / totals.len() as f64;
    let average = jcompat::jnum::kotlin_round_to_long(average)
        .ok_or_else(|| VendorError::Other(failure(FailureKind::IllegalArgument, "Cannot round NaN value.")))?;
    Ok(HtmlIndex {
        title: title.into(),
        total_failed: sum_count(|p| p.failed.len() as i32),
        total_flaky: sum_count(|p| p.flaky),
        total_ignored: sum_count(|p| p.ignored.len() as i32),
        total_passed: sum_count(|p| p.passed.len() as i32),
        total_duration_millis: summary
            .pools
            .iter()
            .flat_map(|p| &p.tests)
            .map(|r| duration(r) as f64)
            .sum::<f64>() as i64,
        average_duration_millis: average,
        max_duration_millis: totals.iter().max().copied().unwrap_or(0),
        min_duration_millis: totals.iter().min().copied().unwrap_or(0),
        pools: summary.pools.iter().map(pool_summary).collect(),
    })
}
pub fn logcat_html(log: &str) -> String {
    if log.is_empty() {
        return String::default();
    }
    let mut out = String::from("<div class=\"content\"><div class=\"card log\">");
    for line in lines(log) {
        // Kotlin Char.isLetter tests UTF-16 chars, so supplementary letters do not match.
        let first = line.chars().find(|c| jcompat::jstring::is_letter(*c));
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
fn url(path: &std::path::Path, root: &std::path::Path) -> String {
    let mut out = String::from("../../../../");
    out.push_str(&replace(&relative_file(path, root, root), "#", "%23"));
    out
}
#[derive(serde::Serialize)]
struct HtmlFullTest {
    pool_id: String,
    package_name: String,
    class_name: String,
    name: String,
    id: String,
    filename: String,
    duration_millis: i64,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    stacktrace: Option<String>,
    #[serde(rename = "deviceId")]
    device_id: String,
    diagnostic_video: bool,
    diagnostic_screenshots: bool,
    screenshot: String,
    videos: Vec<String>,
    log_file: String,
}
#[derive(serde::Serialize)]
struct HtmlTestLogDetails {
    pool_id: String,
    test_id: String,
    display_name: String,
    device_id: String,
    log_path: String,
}
fn full_test(
    r: &TestResult,
    pool: &DevicePoolId,
    screenshot: String,
    videos: Vec<String>,
    log: String,
) -> HtmlFullTest {
    HtmlFullTest {
        pool_id: pool.name.clone(),
        package_name: r.test.pkg.clone(),
        class_name: r.test.clazz.clone(),
        name: r.test.method.clone(),
        id: id(r),
        filename: filename(r),
        duration_millis: duration(r),
        status: status(r.status),
        stacktrace: r.stacktrace.clone(),
        device_id: safe_serial(&r.device),
        diagnostic_video: r.device.device_features.contains(&DeviceFeature::Video),
        diagnostic_screenshots: r.device.device_features.contains(&DeviceFeature::Screenshot),
        screenshot,
        videos,
        log_file: log,
    }
}
fn page(path: &std::path::Path, html: &std::path::Path, data: String, log: String, date: &str) -> VendorResult<()> {
    let text = path.to_string_lossy();
    let parent = PathBuf::from(&text[..text.rfind('/').unwrap_or(0)]);
    let mut prefix = relative_file(html, &parent, html);
    if !prefix.is_empty() {
        prefix.push('/')
    }
    let mut text = String::from(include_str!("../../../../fixtures/reporters/html/index.html"));
    for (key, value) in [
        ("${relative_path}", prefix.as_str()),
        ("${data_json}", &data),
        ("${log}", &log),
        ("${date}", date),
    ] {
        text = replace(&text, key, value)
    }
    write(path, text.as_bytes())
}
fn assignment(name: &str, value: &(impl serde::Serialize + ?Sized)) -> VendorResult<String> {
    let mut out = String::from(name);
    out.push_str(" = ");
    out.push_str(&html_json(value)?);
    Ok(out)
}
#[derive(Clone)]
pub struct HtmlSummaryReporter {
    io: Arc<dyn VendorRuntime>,
    files: Arc<dyn OutputFiles>,
    clock: Arc<dyn WallClock>,
    title: String,
    root: PathBuf,
}
impl HtmlSummaryReporter {
    pub fn new(
        io: Arc<dyn VendorRuntime>,
        files: Arc<dyn OutputFiles>,
        clock: Arc<dyn WallClock>,
        title: String,
        process_dir: PathBuf,
    ) -> Self {
        let root = jcompat::file_manager::absolute_file_path(
            &files.output_dir().to_string_lossy(),
            &process_dir.to_string_lossy(),
        );
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
                    &child_file(&pools, &name),
                    &h,
                    assignment("window.pool", &pool_summary(&p))?,
                    String::default(),
                    &d,
                )?;
                p.tests
                    .iter()
                    .map(|r| {
                        let dir = child_file(&child_file(&pools, &p.pool_id.name), &safe_serial(&r.device));
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
                            .map(|a| url(&a.file, &root))
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
                        &child_file(&dir, &filename),
                        &h,
                        assignment("window.test", &data)?,
                        logcat_html(&r.stacktrace.clone().unwrap_or_default()),
                        &d,
                    )?;
                    let logs = child(&dir, "logs");
                    mkdir(&logs)?;
                    let mut path = String::from("../");
                    path.push_str(&log);
                    let data = HtmlTestLogDetails {
                        pool_id: p.name.clone(),
                        test_id: id(&r),
                        display_name: r.test.method.clone(),
                        device_id: safe_serial(&r.device),
                        log_path: path,
                    };
                    page(
                        &child_file(&logs, &filename),
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
