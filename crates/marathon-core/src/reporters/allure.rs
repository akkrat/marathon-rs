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
    file_manager::{absolute_file_path, relative_file},
    jstring::lines,
    report_text::{XmlTextError, allure_history_id, form_encode, replace, xml_text},
};
use marathon_config::{Configuration, VendorConfiguration};
use std::path::PathBuf;
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
    output: PathBuf,
    process_dir: PathBuf,
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
        output: PathBuf,
        process_dir: PathBuf,
        hooks: Arc<dyn ReplayHooks>,
        attachments: Arc<dyn AttachmentFiles>,
        environment_errors: EnvironmentErrors,
        stdout: Arc<dyn ConsoleSink>,
    ) -> Self {
        let output = absolute_file_path(&output.to_string_lossy(), &process_dir.to_string_lossy());
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
                let native = &output;
                std::fs::create_dir_all(native)
                    .map_err(|e| allure_error("Could not create Allure results directory", nio_error(e, &output)))?;
                let mut name = uuid;
                name.push_str("-result.json");
                write_result(&child_file(&output, &name), &model)
            })
            .await?;
        }
        let this = self.clone();
        let saved = blocking(self.io.as_ref(), move || {
            let params = this.configuration.to_map(&this.process_dir);
            mkdir(&this.output)?;
            // Xalan fails mid-transform after StreamResult opened the file, leaving it empty.
            let (bytes, invalid) = match environment_xml(&params) {
                Ok(text) => (text.as_bytes().to_vec(), None),
                Err(e) => (Vec::new(), Some(xml_failure(e))),
            };
            let error = match write(&child(&this.output, "environment.xml"), &bytes) {
                Err(error) => {
                    let mut failure = failure(
                        "javax.xml.transform.TransformerException",
                        &error.failure().to_java_string().to_owned(),
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
/// TransformerException(SAXException) as thrown by the identity transform.
fn xml_failure(error: XmlTextError) -> Failure {
    let sax = error.sax_message();
    let cause = failure("org.xml.sax.SAXException", &sax);
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
fn write_result(path: &std::path::Path, model: &impl serde::Serialize) -> VendorResult<()> {
    json(model)
        .and_then(|text| write(path, text.as_bytes()))
        .map_err(|e| allure_error("Could not write Allure test result", e))
}
fn allure_error(message: &str, error: VendorError) -> VendorError {
    let mut f = failure("io.qameta.allure.AllureResultsWriteException", message);
    f.cause = Some(Box::new(error.failure().clone()));
    VendorError::Other(f)
}
fn value<'a>(test: &'a Test, name: &str) -> Option<&'a str> {
    let property = test.meta_properties.iter().find(|p| p.name == name)?;
    match property
        .values
        .entries()
        .iter()
        .find(|(k, _)| k == &String::from("value"))?
        .1
    {
        MetaValue::String(ref value) => Some(value),
        _ => None,
    }
}
#[derive(serde::Serialize)]
pub struct Label {
    name: String,
    value: String,
}
#[derive(serde::Serialize)]
pub struct Link {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}
#[derive(serde::Serialize)]
pub struct AllureAttachment {
    name: String,
    source: String,
    #[serde(rename = "type")]
    mime: &'static str,
}
#[derive(serde::Serialize)]
pub struct StatusDetails {
    known: bool,
    muted: bool,
    flaky: bool,
    message: String,
    trace: String,
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AllureResult {
    uuid: String,
    history_id: String,
    full_name: String,
    labels: Vec<Label>,
    links: Vec<Link>,
    name: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_details: Option<StatusDetails>,
    stage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    steps: [(); 0],
    attachments: Vec<AllureAttachment>,
    parameters: [(); 0],
    start: i64,
    stop: i64,
}
fn label(name: &str, value: &str) -> Label {
    Label {
        name: name.into(),
        value: value.into(),
    }
}
fn issue(value: &str) -> Link {
    Link {
        name: Some("Issue".into()),
        url: Some(value.into()),
    }
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
    uuid: &str,
    device: &DeviceInfo,
    r: &TestResult,
    retained: &[Attachment],
    configuration: &Configuration,
    process_dir: &std::path::Path,
) -> AllureResult {
    let test = &r.test;
    let suite = test.to_class_name('.');
    let mut links = Vec::new();
    let attachments = retained
        .iter()
        .map(|a| {
            if a.kind == AttachmentType::Profiling {
                let link = if let VendorConfiguration::Android(c) = &configuration.vendor_configuration {
                    let mut path = String::from("/");
                    path.push_str(&relative_file(&a.file, &configuration.output_dir, process_dir));
                    Link {
                        name: Some("Tracing".into()),
                        url: Some(replace(
                            &c.profiling_configuration.url_template,
                            "{}",
                            &form_encode(&path),
                        )),
                    }
                } else {
                    Link { name: None, url: None }
                };
                links.push(link);
            }
            let (default, mime) = attachment_type(a.kind);
            AllureAttachment {
                name: a.name.clone().unwrap_or_else(|| default.into()),
                source: absolute_file_path(&a.file.to_string_lossy(), &process_dir.to_string_lossy())
                    .to_string_lossy()
                    .into_owned(),
                mime,
            }
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
    AllureResult {
        uuid: uuid.into(),
        history_id: allure_history_id(&test.clazz, &test.method),
        full_name: test.to_safe_test_name(),
        labels,
        links,
        name: test.method.clone(),
        status,
        status_details: r.stacktrace.as_ref().map(|trace| StatusDetails {
            known: false,
            muted: false,
            flaky: false,
            message: lines(trace)[0].clone(),
            trace: trace.clone(),
        }),
        stage: "scheduled",
        description: description.map(str::to_owned),
        steps: [],
        attachments,
        parameters: [],
        start: r.start_time,
        stop: r.end_time,
    }
}

/// Keys and values are text nodes; the first one Xalan rejects aborts the document.
pub fn environment_xml(params: &[(String, String)]) -> Result<String, XmlTextError> {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?><environment>");
    for (k, v) in params {
        out.push_str("<parameter><key>");
        out.push_str(&xml_text(k)?);
        out.push_str("</key><value>");
        out.push_str(&xml_text(v)?);
        out.push_str("</value></parameter>");
    }
    out.push_str("</environment>");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_failure_is_wrapped_as_allure_results_write_exception() {
        struct Unserializable;
        impl serde::Serialize for Unserializable {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("serialization rejected"))
            }
        }
        let Err(error) = write_result(std::path::Path::new("\0"), &Unserializable) else {
            panic!("serialization must fail before opening a file");
        };
        let failure = error.failure();
        assert_eq!(failure.class, "io.qameta.allure.AllureResultsWriteException");
        assert_eq!(failure.message.as_deref(), Some("Could not write Allure test result"));
        let Some(cause) = failure.cause.as_deref() else {
            panic!("the serialization error must remain the cause");
        };
        assert_eq!(cause.class, "java.lang.IllegalArgumentException");
        assert_eq!(cause.message.as_deref(), Some("serialization rejected"));
    }
}
