//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/allure/AllureReporter.kt.
//! Projections follow allure-java-commons/model 2.29.0 and environment-writer 1.0.0.

use super::*;
use crate::{
    analytics::{ReporterRegistration, ReporterSlot},
    listener::AttachmentFiles,
    logging::OutputRecord,
    model::*,
    output::ConsoleSink,
    report::{ExecutionReport, Reporter},
    runtime::*,
    time::ReplayHooks,
};
use jcompat::{
    file_manager::{absolute_file_path_utf16, relative_file},
    jstring::lines,
    report_text::{XmlTextError, allure_history_id, form_encode, jackson_json, replace, xml_text},
};
use marathon_config::{Configuration, VendorConfiguration};
use std::sync::Arc;

/// environment-writer catches TransformerException and prints its stack trace.
/// The console adapter renders Failure::stack_trace (D12) on stderr, off core.
pub type EnvironmentErrors = Arc<dyn Fn(&Failure) + Send + Sync>;
/// environment-writer's System.out.println after a successful transform.
pub const ENVIRONMENT_SAVED: &str = "Allure environment data saved.\n";
#[derive(Clone)]
pub struct AllureReporter {
    io: Arc<dyn VendorRuntime>,
    configuration: Arc<Configuration>,
    output: JavaPath,
    process_dir: JavaPath,
    hooks: Arc<dyn ReplayHooks>,
    attachments: Arc<dyn AttachmentFiles>,
    environment_errors: EnvironmentErrors,
    stdout: Arc<dyn ConsoleSink>,
}
impl AllureReporter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        io: Arc<dyn VendorRuntime>,
        configuration: Arc<Configuration>,
        output: JavaPath,
        process_dir: JavaPath,
        hooks: Arc<dyn ReplayHooks>,
        attachments: Arc<dyn AttachmentFiles>,
        environment_errors: EnvironmentErrors,
        stdout: Arc<dyn ConsoleSink>,
    ) -> Self {
        let output = absolute_file_path_utf16(&output.0, &process_dir.0);
        Self {
            io,
            configuration,
            output,
            process_dir,
            hooks,
            attachments,
            environment_errors,
            stdout,
        }
    }
    pub fn registration(self) -> ReporterRegistration {
        ReporterRegistration {
            slot: ReporterSlot::Allure,
            reporter: Arc::new(self),
        }
    }
    async fn run(self, report: ExecutionReport) -> VendorResult<()> {
        for event in report.test_events() {
            let uuid = self
                .hooks
                .allocate_id("core.AllureReporter.uuid")
                .map_err(clock_error)?;
            let mut retained = Vec::new();
            for attachment in &event.test_result.attachments {
                if !self.attachments.is_empty(attachment).await? {
                    retained.push(attachment.clone())
                }
            }
            let (r, device, cfg, cwd, output) = (
                event.test_result.clone(),
                event.device.clone(),
                self.configuration.clone(),
                self.process_dir.clone(),
                self.output.clone(),
            );
            blocking(self.io.as_ref(), move || {
                let model = result_model(&uuid, &device, &r, &retained, &cfg, &cwd);
                let native = output.to_path_buf().map_err(io_error)?;
                std::fs::create_dir_all(native)
                    .map_err(|e| allure_error("Could not create Allure results directory", nio_error(e, &output)))?;
                let mut name = uuid;
                name.push_str("-result.json");
                write(&child_file(&output, &name.0), &jackson_json(&model).to_utf8_bytes())
                    .map_err(|e| allure_error("Could not write Allure test result", e))
            })
            .await?;
        }
        let this = self.clone();
        let saved = blocking(self.io.as_ref(), move || {
            let params = this.configuration.to_map(&this.process_dir);
            mkdir(&this.output)?;
            // Xalan fails mid-transform after StreamResult opened the file, leaving it empty.
            let (bytes, invalid) = match environment_xml(&params) {
                Ok(text) => (text.to_utf8_bytes(), None),
                Err(e) => (Vec::new(), Some(xml_failure(e))),
            };
            let error = match write(&child(&this.output, "environment.xml"), &bytes) {
                Err(error) => {
                    let mut failure = failure(
                        "javax.xml.transform.TransformerException",
                        &error.failure().to_java_string().to_string_lossy(),
                    );
                    failure.cause = Some(Box::new(error.failure().clone()));
                    Some(failure)
                }
                Ok(()) => invalid,
            };
            Ok(match error {
                Some(failure) => {
                    (this.environment_errors)(&failure);
                    false
                }
                None => true,
            })
        })
        .await?;
        if saved {
            // PrintStream swallows write errors, so a failing console never fails the reporter.
            let _ = self
                .stdout
                .write(vec![OutputRecord::Raw(ENVIRONMENT_SAVED.into())])
                .await;
        }
        Ok(())
    }
}
/// TransformerException(SAXException[(IOException)]) as thrown by the identity transform.
fn xml_failure(error: XmlTextError) -> Failure {
    let sax = error.sax_message();
    let mut cause = failure("org.xml.sax.SAXException", &sax);
    if let Some(io) = error.io_message() {
        cause.cause = Some(Box::new(failure("java.io.IOException", &io)));
    }
    let mut top = failure(
        "javax.xml.transform.TransformerException",
        &format!("org.xml.sax.SAXException: {sax}"),
    );
    top.cause = Some(Box::new(cause));
    top
}
impl Reporter for AllureReporter {
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
fn allure_error(message: &str, error: VendorError) -> VendorError {
    let mut f = failure("io.qameta.allure.AllureResultsWriteException", message);
    f.cause = Some(Box::new(error.failure().clone()));
    VendorError::Other(f)
}
fn value<'a>(test: &'a Test, name: &str) -> Option<&'a JavaString> {
    let property = test.meta_properties.iter().find(|p| p.name == JavaString::from(name))?;
    match property
        .values
        .entries()
        .iter()
        .find(|(k, _)| k == &JavaString::from("value"))?
        .1
    {
        MetaValue::String(ref value) => Some(value),
        _ => None,
    }
}
fn label(name: &str, value: &JavaString) -> WriterValue {
    object([("name", name.into()), ("value", string(value))])
}
fn issue(value: &JavaString) -> WriterValue {
    object([("name", "Issue".into()), ("url", string(value))])
}
fn attachment_type(kind: AttachmentType) -> (&'static str, &'static str) {
    match kind {
        AttachmentType::ScreenshotGif => ("Screenshot_gif", "image/gif"),
        AttachmentType::ScreenshotJpeg => ("Screenshot_jpeg", "image/jpeg"),
        AttachmentType::ScreenshotPng => ("Screenshot_png", "image/png"),
        AttachmentType::ScreenshotWebp => ("Screenshot_webp", "image/webp"),
        AttachmentType::Video => ("Video", "video/mp4"),
        AttachmentType::Log => ("Log", "text/plain"),
        AttachmentType::Profiling => ("Profiling", "application/octet-stream"),
    }
}
/// `retained` is already filtered through the shared attachment emptiness service.
pub fn result_model(
    uuid: &JavaString,
    device: &DeviceInfo,
    r: &TestResult,
    retained: &[Attachment],
    configuration: &Configuration,
    process_dir: &JavaPath,
) -> WriterValue {
    let test = &r.test;
    let suite = test.to_class_name('.');
    let mut links = Vec::new();
    let attachments = retained
        .iter()
        .map(|a| {
            if a.kind == AttachmentType::Profiling {
                let link = if let VendorConfiguration::Android(c) = &configuration.vendor_configuration {
                    let mut path = JavaString::from("/");
                    path.append(relative_file(&a.file, &configuration.output_dir, process_dir));
                    object([
                        ("name", "Tracing".into()),
                        (
                            "url",
                            string(&replace(
                                &c.profiling_configuration.url_template,
                                "{}",
                                &form_encode(&path),
                            )),
                        ),
                    ])
                } else {
                    WriterValue::Object(Vec::new())
                };
                links.push(link);
            }
            let (default, mime) = attachment_type(a.kind);
            object([
                ("name", a.name.as_ref().map_or_else(|| default.into(), string)),
                (
                    "source",
                    WriterValue::String(JavaString(absolute_file_path_utf16(&a.file.0, &process_dir.0).0)),
                ),
                ("type", mime.into()),
            ])
        })
        .collect();
    let mut labels = vec![
        label("host", &device.serial_number),
        label("package", &test.pkg),
        label("testClass", &suite),
        label("testMethod", &test.method),
        label("suite", &suite),
    ];
    let mut description = None;
    for prefix in ["io.qameta.allure.", "io.qameta.allure.kotlin."] {
        if let Some(v) = value(test, &(prefix.to_owned() + "Description")) {
            description = Some(v)
        }
        for name in ["Issue", "TmsLink"] {
            if let Some(v) = value(test, &(prefix.to_owned() + name)) {
                links.push(issue(v))
            }
        }
        // Neither DEX nor instrumentation metadata contains runtime enum instances.
        for (name, key) in [
            ("Epic", "epic"),
            ("Feature", "feature"),
            ("Story", "story"),
            ("Owner", "owner"),
        ] {
            if let Some(v) = value(test, &(prefix.to_owned() + name)) {
                labels.push(label(key, v))
            }
        }
    }
    let status = match r.status {
        TestStatus::Failure => "failed",
        TestStatus::Passed => "passed",
        TestStatus::Incomplete => "broken",
        TestStatus::Ignored | TestStatus::AssumptionFailure => "skipped",
    };
    let mut fields = vec![
        ("uuid".into(), string(uuid)),
        (
            "historyId".into(),
            WriterValue::from(allure_history_id(&test.clazz, &test.method).as_str()),
        ),
        ("fullName".into(), string(&test.to_safe_test_name())),
        ("labels".into(), WriterValue::Array(labels)),
        ("links".into(), WriterValue::Array(links)),
        ("name".into(), string(&test.method)),
        ("status".into(), status.into()),
    ];
    if let Some(trace) = &r.stacktrace {
        fields.push((
            "statusDetails".into(),
            object([
                ("known", WriterValue::Bool(false)),
                ("muted", WriterValue::Bool(false)),
                ("flaky", WriterValue::Bool(false)),
                ("message", string(&lines(trace)[0])),
                ("trace", string(trace)),
            ]),
        ))
    }
    fields.push(("stage".into(), "scheduled".into()));
    if let Some(v) = description {
        fields.push(("description".into(), string(v)))
    }
    fields.extend([
        ("steps".into(), WriterValue::Array(Vec::new())),
        ("attachments".into(), WriterValue::Array(attachments)),
        ("parameters".into(), WriterValue::Array(Vec::new())),
        ("start".into(), r.start_time.into()),
        ("stop".into(), r.end_time.into()),
    ]);
    WriterValue::Object(fields)
}
/// Keys and values are text nodes; the first one Xalan rejects aborts the document.
pub fn environment_xml(params: &[(JavaString, JavaString)]) -> Result<JavaString, XmlTextError> {
    let mut out = JavaString::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?><environment>");
    for (k, v) in params {
        out.push_str("<parameter><key>");
        out.append(xml_text(k)?);
        out.push_str("</key><value>");
        out.append(xml_text(v)?);
        out.push_str("</value></parameter>");
    }
    out.push_str("</environment>");
    Ok(out)
}
