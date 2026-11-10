//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/profiling/ProfilingRunListener.kt
use marathon_logging::Level;

use super::*;
use crate::listeners::{AndroidTestRunListener, to_test};
use jcompat::StringSubstitutor;
use marathon_adb::testrunner::TestEvent;
use marathon_core::{executor::shared::requested, runtime::*, vendor::VendorContext};
use marathon_dex::apk_parser::InstrumentationInfo;
use marathon_error::FailureKind;
pub struct ProfilingRunListener {
    pub artifacts: ArtifactContext,
    config: Mutex<(Option<String>, Option<String>)>,
    job: Mutex<Option<Arc<dyn VendorTask>>>,
    attachments: Attachments,
}
impl ProfilingRunListener {
    pub fn new(artifacts: ArtifactContext) -> Self {
        Self {
            artifacts,
            config: Mutex::new((None, None)),
            job: Mutex::new(None),
            attachments: Attachments::default(),
        }
    }
    fn log(&self, level: Level, message: &str) {
        self.artifacts
            .device
            .log_named("ProfilingRunListener", level, message, None);
    }
    async fn stop(&self, context: VendorContext, cancel: bool) -> VendorResult<()> {
        self.log(Level::DEBUG, "Stopping perfetto");
        let start = self.artifacts.now("android.ProfilingRunListener.stop")?;
        let result = self
            .artifacts
            .device
            .safe_execute_shell_command("killall perfetto", "", context)
            .await;
        let end = self.artifacts.now("android.ProfilingRunListener.stop")?;
        self.log(
            Level::DEBUG,
            &format!("Stopped perfetto: {}ms", end.wrapping_sub(start)),
        );
        let start = self.artifacts.now("android.ProfilingRunListener.join")?;
        let job = lock(&self.job).take();
        if let Some(job) = job {
            if cancel || result.is_err() {
                job.cancel(requested("profiling stopped"));
            }
            let _ = job.join().await;
        }
        let end = self.artifacts.now("android.ProfilingRunListener.join")?;
        self.log(
            Level::DEBUG,
            &format!("Joining perfetto: {}ms", end.wrapping_sub(start)),
        );
        result.map(|_| ())
    }
}
impl AttachmentProvider for ProfilingRunListener {
    fn register_listener(&self, l: Arc<dyn AttachmentListener>) -> VendorResult<()> {
        self.attachments.register(l)
    }
}
impl AndroidTestRunListener for ProfilingRunListener {
    fn before_test_run_in<'a>(
        &'a self,
        _info: Option<&'a InstrumentationInfo>,
        _context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let path = self
                .artifacts
                .device
                .runtime()
                .configuration
                .profiling_configuration
                .pbtxt
                .clone();
            let config = blocking(self.artifacts.device.runtime().dependencies.io.as_ref(), move || {
                path.map(|p| {
                    use std::io::Read;
                    let open_error = |e: std::io::Error| {
                        let reason = e.to_string();
                        let mut message = p.to_string_lossy().into_owned();
                        message.push_str(" (");
                        message.push_str(reason.split(" (os error ").next().unwrap_or(&reason));
                        message.push(')');
                        other(FailureKind::FileNotFound, message)
                    };
                    let mut file = std::fs::File::open(&p).map_err(open_error)?;
                    if file.metadata().map_err(io)?.is_dir() {
                        return Err(open_error(std::io::Error::from(std::io::ErrorKind::IsADirectory)));
                    }
                    let mut bytes = Vec::new();
                    file.read_to_end(&mut bytes).map_err(io)?;
                    Ok(String::from_utf8_lossy(&bytes).into_owned())
                })
                .transpose()
            })
            .await?;
            lock(&self.config).0 = config;
            Ok(())
        })
    }
    fn cleanup_in(&self, context: VendorContext) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if lock(&self.job).is_some() {
                let result = self.stop(context, true).await;
                let remaining = lock(&self.job).take();
                if let Some(job) = remaining {
                    job.cancel(requested("profiling cleanup"));
                    let _ = job.join().await;
                }
                result?;
            }
            Ok(())
        })
    }
    fn on_event_in<'a>(&'a self, event: &'a TestEvent, context: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            match event {
                TestEvent::TestRunStartedEvent { .. } => {
                    let test = self
                        .artifacts
                        .batch
                        .tests
                        .first()
                        .ok_or_else(|| other(FailureKind::NoSuchElement, "List is empty."))?;
                    let bundle = self.artifacts.device.runtime().identifier.android_bundle(test)?;
                    let info = blocking(self.artifacts.device.runtime().dependencies.io.as_ref(), move || {
                        bundle.instrumentation_info()
                    })
                    .await?;
                    let package = info.application_package.to_owned();
                    let result = self
                        .artifacts
                        .device
                        .critical_execute_shell_command(&format!("pidof {package}"), "", context)
                        .await?;
                    let pid = jcompat::jnum::parse_int(&trim(&result.output.to_owned()));
                    self.log(
                        Level::DEBUG,
                        &format!(
                            "Target pid: {}",
                            pid.map(|i| i.to_string()).unwrap_or_else(|| "null".into())
                        ),
                    );
                    let config = lock(&self.config).0.clone();
                    if let Some(config) = config {
                        let rendered = StringSubstitutor::new(|key| match key {
                            "TRACING_TARGET_PID" => pid.map(|i| i.to_string()),
                            "TRACING_TARGET_PACKAGE" => Some(package.clone()),
                            _ => None,
                        })
                        .replace(&config)
                        .map_err(|e| other(FailureKind::Arithmetic, e.message.as_str().to_owned()))?;
                        self.log(Level::DEBUG, &format!("Rendered config:\n{rendered}"));
                        lock(&self.config).1 = Some(rendered);
                    }
                }
                TestEvent::TestStarted { id } => {
                    let remote = self
                        .artifacts
                        .device
                        .file_manager()
                        .remote_profiling_for_test(&to_test(id), &self.artifacts.batch.id)?;
                    let config = lock(&self.config).1.clone().unwrap_or_else(|| "null".into());
                    let device = self.artifacts.device.clone();
                    *lock(&self.job) = Some(self.artifacts.device.runtime().dependencies.io.spawn_cancellable(
                        Box::new(move |signal| {
                            Box::pin(async move {
                                let result = device
                                    .execute_shell_command(
                                        &format!("echo '{config}' | perfetto --txt -c - -o {}", remote.to_owned()),
                                        "",
                                        VendorContext {
                                            cancellation: Arc::new(ChildSignal(signal, context.cancellation.clone())),
                                            ..context
                                        },
                                    )
                                    .await;
                                match &result {
                                    Ok(r) => device.log_named(
                                        "ProfilingRunListener",
                                        Level::DEBUG,
                                        &format!(
                                            "perfetto process finished: {}",
                                            r.as_ref()
                                                .map(|r| format!(
                                                    "output={} exitCode={}",
                                                    r.output.to_owned(),
                                                    r.exit_code
                                                ))
                                                .unwrap_or_else(|| "null".into())
                                        ),
                                        None,
                                    ),
                                    Err(e) => {
                                        let (level, text) = if matches!(e, VendorError::Cancelled(_)) {
                                            (Level::WARN, "perfetto start was interrupted")
                                        } else {
                                            (Level::ERROR, "Something went wrong while recording perfetto trace")
                                        };
                                        device.log_named("ProfilingRunListener", level, text, Some(e.failure()));
                                    }
                                }
                                result.map(|_| ())
                            })
                        }),
                    ));
                }
                TestEvent::TestEnded { id, .. } => {
                    let result = async {
                        self.stop(context.clone(), false).await?;
                        let test = to_test(id);
                        let remote = self
                            .artifacts
                            .device
                            .file_manager()
                            .remote_profiling_for_test(&test, &self.artifacts.batch.id)?;
                        let file = self.artifacts.file(FileType::Tracing, Some(&test), None, None).await?;
                        let start = self.artifacts.now("android.ProfilingRunListener.pullTracing")?;
                        self.artifacts
                            .device
                            .safe_pull_file(&remote.to_owned(), &file.to_string_lossy(), context.clone())
                            .await?;
                        let end = self.artifacts.now("android.ProfilingRunListener.pullTracing")?;
                        self.log(
                            Level::TRACE,
                            &format!(
                                "Pulling finished in {}ms {} ",
                                end.wrapping_sub(start),
                                remote.to_owned()
                            ),
                        );
                        self.attachments
                            .emit(&test, file, AttachmentType::Profiling, "profiling")?;
                        self.artifacts
                            .device
                            .safe_execute_shell_command(&format!("rm -f {}", remote.to_owned()), "", context)
                            .await?;
                        Ok(())
                    }
                    .await;
                    match result {
                        Err(VendorError::Transfer(_)) => self.log(Level::WARN, "Can't pull tracing file"),
                        r => r?,
                    }
                    self.log(Level::DEBUG, "Finished processing");
                }
                // P4: stop and join on run failure, without pulling or removing the trace.
                TestEvent::TestRunFailed { .. } if lock(&self.job).is_some() => {
                    self.stop(context, true).await?;
                }
                _ => {}
            }
            Ok(())
        })
    }
}
