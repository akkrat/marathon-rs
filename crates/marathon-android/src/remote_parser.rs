//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AmInstrumentTestParser.kt
use crate::{
    device::AndroidDevice, installer::AndroidAppInstaller, logcat_listener::LogcatAccumulatingListener, models::*,
    provider::AdamDeviceProvider, runner::InstrumentationChannel, services::AndroidRuntime, support::*,
    test_annotation_parser::TestAnnotationParser,
};
use jcompat::logging::Level;
use marathon_adb::{
    instrument_request::{InstrumentOptions, TestRunnerRequest},
    testrunner::{MetricMap, TestEvent},
};
use marathon_config::vendor::AndroidTestParserConfiguration;
use marathon_config::vendor::CONFIGURATION_EXCEPTION_CLASS;
use marathon_core::{error::*, executor::bridge::call_vendor, model::Test, runtime::*, vendor::*};
use marathon_error::{AndroidKind, FailureKind};
use std::sync::Arc;

pub const TEST_ANNOTATION_PRODUCER: &str = "com.malinskiy.adam.junit4.android.listener.TestAnnotationProducer";
#[derive(Clone)]
pub struct AmInstrumentTestParser {
    pub runtime: Arc<AndroidRuntime>,
    pub provider: Arc<AdamDeviceProvider>,
}
struct AttemptError {
    error: VendorError,
    logcat: String,
}
impl From<VendorError> for AttemptError {
    fn from(error: VendorError) -> Self {
        Self {
            error,
            logcat: String::default(),
        }
    }
}
impl AmInstrumentTestParser {
    fn log(&self, level: Level, message: &str, error: Option<&Failure>) {
        let logger = {
            let mut registry = lock(&self.runtime.dependencies.logging);
            let name = "com.malinskiy.marathon.android.adam.AmInstrumentTestParser";
            if registry.is_registered(name) {
                registry.slf4j_logger(name)
            } else {
                registry.logger(name, None)
            }
        };
        self.runtime.emit(&logger, level, message, error);
    }
    async fn extract_inner(&self, reference: DeviceRef, context: VendorContext) -> VendorResult<Vec<Test>> {
        let mut message = String::from("Parsing bundle(s):");
        for bundle in &self.runtime.bundles {
            let b = bundle.clone();
            let info = blocking(self.runtime.dependencies.io.as_ref(), move || b.instrumentation_info()).await?;
            message.push_str(&format!("\n- testPackage: {}", info.instrumentation_package.to_owned()));
            if !info.application_package.chars().all(jcompat::jstring::is_whitespace) {
                message.push_str(&format!("\n  targetPackage: {}", info.application_package.to_owned()));
            }
        }
        self.log(Level::Debug, &message, None);
        let device = self.provider.android_device(&reference).ok_or_else(|| {
            other(
                CONFIGURATION_EXCEPTION_CLASS,
                "Unexpected device type for remote test parsing",
            )
        })?;
        let mut result = vec![];
        let mut fallback = false;
        for bundle in &self.runtime.bundles {
            let mut failed_attempts = 0;
            loop {
                match self
                    .parse_attempt(&device, bundle.clone(), fallback, context.clone())
                    .await
                {
                    Ok(tests) => {
                        for test in tests {
                            if !result.contains(&test) {
                                result.push(test);
                            }
                        }
                        break;
                    }
                    Err(AttemptError {
                        error: e @ VendorError::Cancelled(_),
                        ..
                    }) => return Err(e),
                    Err(AttemptError { error, logcat }) => {
                        if error.failure().kind == FailureKind::Android(AndroidKind::TestAnnotationProducerNotFound) {
                            fallback = true;
                            let b = bundle.clone();
                            let info =
                                blocking(self.runtime.dependencies.io.as_ref(), move || b.instrumentation_info())
                                    .await?;
                            self.log(Level::Warn, &format!("Previous parsing attempt failed for {}\n                         file: {}\n                       due to test parser misconfiguration: test annotation producer was not found. See https://docs.marathonlabs.io/runner/android/configure#test-parser\n                       Next parsing attempt will remove overridden test run listener.\n                       Device log:\n                       {}", info.instrumentation_package.to_owned(), bundle.files.test_application.to_string_lossy().into_owned(), logcat.to_owned()), None);
                        } else {
                            self.log(Level::Debug, "Remote parsing failed. Retrying", Some(error.failure()));
                            // D22, D41: the retries have no delay, so an outage or a brief `offline` would use them all
                            // up within its first moments, on a server or device that is about to return.
                            if !server_is_not_in_question(&error) && device.wait_out_passing_loss(&context).await? {
                                continue;
                            }
                        }
                        failed_attempts += 1;
                        if failed_attempts == 3 {
                            return Err(error);
                        }
                        context.clock.delay(0, context.cancellation.clone()).await?;
                    }
                }
            }
        }
        Ok(result)
    }
    pub async fn parse_tests(
        &self,
        device: &AndroidDevice,
        bundle: Arc<AndroidTestBundle>,
        fallback: bool,
        context: VendorContext,
    ) -> VendorResult<Vec<Test>> {
        self.parse_attempt(device, bundle, fallback, context)
            .await
            .map_err(|error| error.error)
    }
    async fn parse_attempt(
        &self,
        device: &AndroidDevice,
        bundle: Arc<AndroidTestBundle>,
        fallback: bool,
        context: VendorContext,
    ) -> Result<Vec<Test>, AttemptError> {
        AndroidAppInstaller::new(self.runtime.clone())
            .prepare_partial_installation(device, bundle.clone(), context.clone())
            .await?;
        let listener = LogcatAccumulatingListener::new(Arc::new(device.clone()));
        listener.setup();
        let mut channel = None;
        let result: Result<Vec<Test>, AttemptError> = async {
            let b = bundle.clone();
            let info = blocking(self.runtime.dependencies.io.as_ref(), move || b.instrumentation_info()).await?;
            let overrides: MetricMap = match &self.runtime.configuration.test_parser_configuration {
                AndroidTestParserConfiguration::Remote { instrumentation_args } => instrumentation_args
                    .iter()
                    .filter(|(key, value)| {
                        !(fallback && key == "listener" && *value == TEST_ANNOTATION_PRODUCER)
                    })
                    .map(|(key, value)| (key.as_str().into(), value.clone()))
                    .collect(),
                _ => MetricMap::new(),
            };
            let mut request = TestRunnerRequest::new(
                info.instrumentation_package.clone(),
                InstrumentOptions {
                    log: Some(true),
                    overrides,
                    ..Default::default()
                },
                device.supported_features(),
            );
            request.runner_class = info.test_runner_class;
            listener.start();
            channel = Some(InstrumentationChannel::start(device, request));
            let mut tests = vec![];
            let mut observed = false;
            let mut session = SessionEnd::default();
            if let Some(channel) = channel.as_mut() {
                loop {
                    let events = timed(
                        &context,
                        self.runtime.dependencies.configuration.test_output_timeout_millis,
                        |signal| {
                            let child = VendorContext {
                                cancellation: signal,
                                ..context.clone()
                            };
                            let receiver = &mut *channel;
                            Box::pin(async move { receiver.receive(&child).await })
                        },
                    )
                    .await?
                    .ok_or_else(|| {
                        VendorError::TestParsing(failure(
                            FailureKind::TestParsing,
                            format!("Unable to parse test list using {}", device.serial_number().to_owned()).as_str(),
                        ))
                    })?;
                    let Some(events) = events else {
                        // D25: otherwise the attempt would "succeed" with whatever tests had been listed so far (N38).
                        // That includes a stream that merely stopped: a dropped connection looks like any other end.
                        if let Some(failure) = channel.take_failure() {
                            let mut error = failure_of_parse(device, &failure);
                            error.cause = Some(Box::new(failure.failure().clone()));
                            return Err(VendorError::TestParsing(error).into());
                        }
                        if let Some(reason) = session.short_list() {
                            return Err(VendorError::TestParsing(failure_of_session(device, &reason)).into());
                        }
                        break;
                    };
                    for event in events {
                        match event {
                            TestEvent::TestRunStartedEvent { test_count } => session.announced = Some(test_count),
                            TestEvent::TestEnded { id, metrics } => {
                                session.listed += 1;
                                let annotations = TestAnnotationParser::new().extract_annotations(&metrics);
                                observed |= !annotations.is_empty();
                                let test = TestIdentifier {
                                    class_name: id.class_name,
                                    test_name: id.test_name,
                                }
                                .to_test(annotations);
                                if !tests.contains(&test) {
                                    tests.push(test.clone());
                                }
                                self.runtime.identifier.put(&test, &bundle);
                            }
                            TestEvent::TestRunFailing { ref error, .. }
                            | TestEvent::TestRunFailed { ref error } => {
                                let logs = listener.stop();
                                let text = error.to_owned();
                                if (matches!(event, TestEvent::TestRunFailing { .. })
                                    && text.contains(&format!("Could not find extra class {TEST_ANNOTATION_PRODUCER}")))
                                    || (matches!(event, TestEvent::TestRunFailed { .. }) && text.contains("Process crashed"))
                                {
                                    return Err(AttemptError {
                                        error: VendorError::Other(Failure {
                                            kind: FailureKind::Android(AndroidKind::TestAnnotationProducerNotFound),
                                            message: None,
                                            cause: None,
                                            stacktrace: None,
                                        }),
                                        logcat: logs,
                                    });
                                }
                                match event {
                                    TestEvent::TestRunFailing { ref error, .. } => session.failing = Some(error.clone()),
                                    _ => session.failed = Some(error.clone()),
                                }
                            }
                            // The transformer follows a failed run with this only when the session reached its code.
                            TestEvent::TestRunEnded { .. } => session.code_failure = session.failed.take(),
                            _ => {}
                        }
                    }
                }
            }
            if !observed {
                self.log(
                    Level::Warn,
                    &format!("Bundle {} did not report any test annotations. If you need test annotations retrieval, remote test parser requires additional setup see https://docs.marathonlabs.io/runner/android/configure#test-parser", bundle.id().to_owned()),
                    None,
                );
            }
            Ok(tests)
        }
        .await;
        // P2: Kotlin only unregisters after success and never cancels this channel.
        // Keep the same listener Arc and finish even on timeout/cancel/fallback.
        listener.finish();
        listener.stop();
        if let Some(channel) = &mut channel {
            channel.cancel_and_join().await;
        }
        result
    }
}
/// D42: how a session ended, as far as it tells whether the list is whole. Kotlin keeps whatever was listed, whatever
/// the end, unless the text says `Process crashed`.
#[derive(Default)]
struct SessionEnd {
    /// `numtests`, which a session states before its first test.
    announced: Option<i32>,
    /// Tests reported as ended, counting one reported twice, as `numtests` does.
    listed: i32,
    failing: Option<String>,
    failed: Option<String>,
    /// The text of an `INSTRUMENTATION_CODE` other than -1, which is success.
    code_failure: Option<String>,
}
impl SessionEnd {
    /// Why the list is short, if it is. A session can end with a failing code on a whole list, which a retry would only
    /// repeat: adam's `log_6` lists 9 of 9 and ends with code 0 and no message, and a listener can fail after the last
    /// test. So a list that reached the count it announced stands. One with no count to reach proves nothing and doesn't.
    ///
    /// A run failure with no such code, past D25's check for a cut stream, and after a count, comes from a last line with
    /// no line break: the transformer can't read that code, whatever it says, and calls the run incomplete. `am` ends its
    /// lines, so a list this short counts as cut. With no count it is `am` refusing to start or output that never was the
    /// protocol, which stay as they are.
    fn short_list(&self) -> Option<String> {
        let failure = self
            .code_failure
            .as_ref()
            .or(self.failed.as_ref().filter(|_| self.announced.is_some()))?;
        if self.announced.is_some_and(|count| count > 0 && self.listed >= count) {
            return None;
        }
        let mut reason = failure.clone();
        if let Some(failing) = &self.failing {
            reason.push_str(": ");
            reason.push_str(&failing.clone());
        }
        reason.push_str(&format!(", tests listed: {}", self.listed));
        if let Some(count) = self.announced {
            reason.push_str(&format!(" of {count}"));
        }
        Some(reason)
    }
}
/// The parse failures with no cause, which do not suggest an outage. Output gone silent: a lost server ends the stream at
/// once, and the transcripts of a silent device stay as they are. A session cut short (D42): it reached its result code
/// over that same server.
fn server_is_not_in_question(error: &VendorError) -> bool {
    matches!(error, VendorError::TestParsing(failure) if failure.cause.is_none())
}
fn failure_of_session(device: &AndroidDevice, reason: &str) -> Failure {
    let mut message = String::from("Unable to parse test list using ");
    message.push_str(&device.serial_number());
    message.push_str(": ");
    message.push_str(reason);
    self::failure(FailureKind::TestParsing, message)
}
fn failure_of_parse(device: &AndroidDevice, failure: &VendorError) -> Failure {
    let mut message = String::from("Unable to parse test list using ");
    message.push_str(&device.serial_number());
    message.push_str(": ");
    message.push_str(&failure.failure().to_java_string());
    self::failure(FailureKind::TestParsing, message)
}
impl RemoteTestParser for AmInstrumentTestParser {
    fn extract(&self, reference: DeviceRef, context: VendorContext) -> VendorFuture<'_, Vec<Test>> {
        Box::pin(async move {
            let parser = self.clone();
            call_vendor(
                self.runtime.dependencies.io.as_ref(),
                context.cancellation.clone(),
                move |signal| {
                    Box::pin(async move {
                        parser
                            .extract_inner(
                                reference,
                                VendorContext {
                                    cancellation: signal,
                                    ..context
                                },
                            )
                            .await
                    })
                },
            )
            .await
        })
    }
}
