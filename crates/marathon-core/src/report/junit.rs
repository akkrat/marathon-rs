//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/junit/JUnitReporter.kt,
//! model/{JUnitReport,TestSuite,TestCase,Failure,Skipped,Rerun}.kt and
//! serialize/{JUnitReportSerializer,XmlExtensions}.kt (reachable single-suite surface).

use super::*;
use crate::{artifact_io::*, runtime::VendorRuntime};
use jcompat::{
    file_manager::absolute_file_path,
    formatting::{DatePattern, format_date},
    jnum,
};
use std::io::Write;
use std::path::PathBuf;

pub struct JUnitReporter {
    pub io: Arc<dyn VendorRuntime>,
    pub output_dir: PathBuf,
    pub cwd: PathBuf,
}
impl JUnitReporter {
    pub fn new(io: Arc<dyn VendorRuntime>, output_dir: PathBuf, cwd: PathBuf) -> Self {
        Self { io, output_dir, cwd }
    }
}
impl Reporter for JUnitReporter {
    fn generate<'a>(&'a self, report: &'a ExecutionReport) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            // Build every suite before the first filesystem side effect.
            let reports: Vec<_> = report
                .summary()
                .pools
                .iter()
                .filter(|p| !p.tests.is_empty())
                .map(|p| (p.pool_id.name.clone(), junit_xml(p)))
                .collect();
            for (pool, text) in reports {
                let output = absolute_file_path(&self.output_dir.to_string_lossy(), &self.cwd.to_string_lossy());
                blocking(self.io.as_ref(), move || {
                    // Paths.get joins raw pool segments, unlike FileManager's escape.
                    let directory = PathBuf::from(format!("{}/tests/{pool}", output.to_string_lossy()));
                    let _ = std::fs::create_dir_all(&directory);
                    let path = PathBuf::from(format!("{}/marathon_junit_report.xml", directory.to_string_lossy()));
                    let native = &path;
                    // createNewFile precedes the serializer's truncating FileOutputStream.
                    match std::fs::OpenOptions::new().write(true).create_new(true).open(native) {
                        Ok(_) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(e) => {
                            let text = e.to_string();
                            let reason = text.split(" (os error ").next().unwrap_or(&text);
                            return Err(crate::error::VendorError::Other(crate::executor::shared::failure(
                                "java.io.IOException",
                                reason,
                            )));
                        }
                    }
                    let mut file = std::fs::File::create(native).map_err(|e| file_open_error(e, &path))?;
                    file.write_all(text.as_bytes()).map_err(io_error)
                })
                .await
                .map_err(operation_error)?;
            }
            Ok(())
        })
    }
}
const SCHEMA: &str = "https://raw.githubusercontent.com/jenkinsci/xunit-plugin/ae25da5089d4f94ac6c4669bf736e4d416cc4665/src/main/resources/org/jenkinsci/plugins/xunit/types/model/xsd/junit-10.xsd";
fn attr(out: &mut String, key: &str, value: &str) {
    out.push(' ');
    out.push_str(key);
    out.push_str("=\"");
    for ch in value.chars() {
        let unit = ch as u32;
        match unit {
            34 => out.push_str("&quot;"),
            38 => out.push_str("&amp;"),
            60 => out.push_str("&lt;"),
            62 => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out.push('"');
}
fn cdata(out: &mut String, text: &str) {
    out.push_str("<![CDATA[");
    out.push_str(&text.replace("]]>", "]]]]><![CDATA[>"));
    out.push_str("]]>");
}
fn seconds(millis: i64) -> String {
    jnum::double_to_string(millis as f64 / 1000.0)
}
/// Exact StAX bytes for the only model JUnitReporter constructs (approved P2).
pub fn junit_xml(pool: &PoolSummary) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?><testsuite");
    attr(&mut out, "xsi:noNamespaceSchemaLocation", SCHEMA);
    attr(&mut out, "xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance");
    attr(&mut out, "name", &pool.pool_id.name);
    attr(&mut out, "tests", &pool.tests.len().to_string());
    attr(&mut out, "failures", &pool.failed.len().to_string());
    attr(&mut out, "errors", "0");
    attr(
        &mut out,
        "time",
        &seconds(
            pool.tests
                .iter()
                .fold(0i64, |sum, r| sum.wrapping_add(r.end_time.wrapping_sub(r.start_time))),
        ),
    );
    attr(&mut out, "skipped", &pool.ignored.len().to_string());
    if let Some(end) = pool.tests.iter().map(|r| r.end_time).max() {
        attr(&mut out, "timestamp", &format_date(end, DatePattern::Junit));
    }
    out.push('>');
    for r in &pool.tests {
        out.push_str("<testcase");
        attr(&mut out, "name", &r.test.method);
        attr(&mut out, "time", &seconds(r.end_time.wrapping_sub(r.start_time)));
        attr(&mut out, "classname", &r.test.to_class_name('.'));
        out.push('>');
        let failure = matches!(r.status, TestStatus::Failure | TestStatus::Incomplete);
        if r.is_ignored() || failure {
            let tag = if r.is_ignored() { "skipped" } else { "failure" };
            out.push('<');
            out.push_str(tag);
            out.push('>');
            if let Some(trace) = r.stacktrace.as_ref().filter(|t| !t.is_empty()) {
                cdata(&mut out, trace);
            }
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
        if !r.is_ignored()
            && let Some((_, retries)) = pool.retries.iter().find(|(key, _)| key == r)
        {
            for retry in retries
                .iter()
                .filter(|e| e.test_result.status != TestStatus::Incomplete && (!failure || !e.test_result.is_success()))
            {
                let tag = if failure { "rerunFailure" } else { "flakyFailure" };
                out.push('<');
                out.push_str(tag);
                attr(&mut out, "type", "rerun exception");
                out.push('>');
                if let Some(trace) = &retry.test_result.stacktrace {
                    out.push_str("<stackTrace>");
                    cdata(&mut out, trace);
                    out.push_str("</stackTrace>");
                }
                out.push_str("</");
                out.push_str(tag);
                out.push('>');
            }
        }
        out.push_str("</testcase>");
    }
    out.push_str("</testsuite>");
    out
}
