//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AmInstrumentTestParser.kt
use crate::{
    device::AndroidDevice, installer::AndroidAppInstaller, logcat_listener::LogcatAccumulatingListener, models::*,
    provider::AdamDeviceProvider, runner::InstrumentationChannel, services::AndroidRuntime, support::*,
    test_annotation_parser::TestAnnotationParser,
};
use jcompat::{logging::Level, writers::JavaString};
use marathon_adb::{
    instrument_request::{InstrumentOptions, TestRunnerRequest},
    testrunner::{MetricMap, TestEvent},
};
use marathon_config::vendor::AndroidTestParserConfiguration;
use marathon_config::vendor::CONFIGURATION_EXCEPTION_CLASS;
use marathon_core::{error::*, executor::bridge::call_vendor, model::Test, runtime::*, vendor::*};
use std::sync::Arc;

pub const TEST_ANNOTATION_PRODUCER: &str = "com.malinskiy.adam.junit4.android.listener.TestAnnotationProducer";
#[derive(Clone)]
pub struct AmInstrumentTestParser {
    pub runtime: Arc<AndroidRuntime>,
    pub provider: Arc<AdamDeviceProvider>,
}
struct AttemptError {
    error: VendorError,
    logcat: JavaString,
}
impl From<VendorError> for AttemptError {
    fn from(error: VendorError) -> Self {
        Self {
            error,
            logcat: JavaString::default(),
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
            message.push_str(&format!(
                "\n- testPackage: {}",
                info.instrumentation_package.to_string_lossy()
            ));
            if !info
                .application_package
                .0
                .iter()
                .all(|&u| jcompat::jstring::is_whitespace(u))
            {
                message.push_str(&format!(
                    "\n  targetPackage: {}",
                    info.application_package.to_string_lossy()
                ));
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
            for attempt in 0..3 {
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
                        if error.failure().class
                            == "com.malinskiy.marathon.android.adam.TestAnnotationProducerNotFoundException".into()
                        {
                            fallback = true;
                            let b = bundle.clone();
                            let info =
                                blocking(self.runtime.dependencies.io.as_ref(), move || b.instrumentation_info())
                                    .await?;
                            self.log(Level::Warn, &format!("Previous parsing attempt failed for {}\n                         file: {}\n                       due to test parser misconfiguration: test annotation producer was not found. See https://docs.marathonlabs.io/runner/android/configure#test-parser\n                       Next parsing attempt will remove overridden test run listener.\n                       Device log:\n                       {}", info.instrumentation_package.to_string_lossy(), JavaString(bundle.files.test_application.0.clone()).to_string_lossy(), logcat.to_string_lossy()), None);
                        } else {
                            self.log(Level::Debug, "Remote parsing failed. Retrying", Some(error.failure()));
                        }
                        if attempt == 2 {
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
                        !(fallback && key == "listener" && *value == TEST_ANNOTATION_PRODUCER.into())
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
                            "com.malinskiy.marathon.exceptions.TestParsingException",
                            format!("Unable to parse test list using {}", device.serial_number().to_string_lossy()).as_str(),
                        ))
                    })?;
                    let Some(events) = events else {
                        break;
                    };
                    for event in events {
                        match event {
                            TestEvent::TestEnded { id, metrics } => {
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
                                let text = error.to_string_lossy();
                                if (matches!(event, TestEvent::TestRunFailing { .. })
                                    && text.contains(&format!("Could not find extra class {TEST_ANNOTATION_PRODUCER}")))
                                    || (matches!(event, TestEvent::TestRunFailed { .. }) && text.contains("Process crashed"))
                                {
                                    return Err(AttemptError {
                                        error: VendorError::Other(Failure {
                                            class: "com.malinskiy.marathon.android.adam.TestAnnotationProducerNotFoundException".into(),
                                            message: None,
                                            cause: None,
                                            stacktrace: None,
                                        }),
                                        logcat: logs,
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            if !observed {
                self.log(
                    Level::Warn,
                    &format!("Bundle {} did not report any test annotations. If you need test annotations retrieval, remote test parser requires additional setup see https://docs.marathonlabs.io/runner/android/configure#test-parser", bundle.id().to_string_lossy()),
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
