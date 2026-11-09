//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AndroidDeviceTestRunner.kt
//! and adam/AdamAndroidDevice.kt:461-489 (the execute exception boundary).
use crate::{
    arguments::ArgumentsFactory,
    device::{AndroidBatchExecutor, AndroidDevice},
    listeners::*,
    models::*,
    support::*,
};
use jcompat::{logging::Level, writers::KotlinValue};
use marathon_adb::{
    instrument_request::{InstrumentOptions, TestRunnerRequest},
    testrunner::{MetricMap, TestEvent, TestIdentifier},
};
use marathon_config::{Configuration, vendor::PathRoot};
use marathon_core::{error::*, executor::shared::requested, model::*, runtime::*, vendor::*};
use marathon_dex::apk_parser::InstrumentationInfo;
use marathon_error::{AdbKind, FailureKind};
use std::sync::{Arc, Mutex};

pub const ERROR_STUCK: &str = "Test got stuck. You can increase the timeout in settings if it's too strict";
/// Device-scoped producer, independent of the execute job, as in adam. The owner
/// explicitly cancels/joins it on every exit (approved P2), including a failing after hook.
pub struct InstrumentationChannel {
    receiver: tokio::sync::mpsc::Receiver<Vec<TestEvent>>,
    task: Arc<dyn VendorTask>,
    device: AndroidDevice,
    /// D25: why the producer stopped, set before the channel closes. Kotlin discards it (N38).
    failure: Arc<Mutex<Option<VendorError>>>,
}
impl InstrumentationChannel {
    pub fn start(device: &AndroidDevice, request: TestRunnerRequest) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let owned = device.clone();
        let failure = Arc::new(Mutex::new(None));
        let producer_failure = failure.clone();
        let task = device
            .runtime()
            .dependencies
            .io
            .spawn_cancellable(Box::new(move |signal| {
                Box::pin(async move {
                    let mut stream = marathon_adb::client::shell::compat::FallibleCompatShellStream::start(
                        owned.client(),
                        request,
                        Some(owned.adb_serial()),
                    );
                    let result = async {
                        loop {
                            let chunk = cancellable(signal.clone(), async {
                                stream.next().await.transpose().map_err(adb_error)
                            })
                            .await;
                            if let Some(request) = stream.request_mut() {
                                for warning in request.take_warnings() {
                                    owned.log(Level::Warn, &warning.to_string(), None);
                                }
                            }
                            match chunk? {
                                Some(events) => {
                                    cancellable(signal.clone(), async {
                                        sender.send(events).await.map_err(|_| {
                                            VendorError::Cancelled(requested("instrumentation channel closed"))
                                        })
                                    })
                                    .await?
                                }
                                None => break,
                            }
                        }
                        // D25: a dropped connection ends the shell stream as normally as `am` does.
                        match stream.request().filter(|r| r.ended_without_result()) {
                            Some(request) => Err(stream_ended_early(request.exit_code())),
                            None => Ok(()),
                        }
                    }
                    .await;
                    stream.cancel();
                    if let Err(error) = &result
                        && !matches!(error, VendorError::Cancelled(_))
                    {
                        *lock(&producer_failure) = Some(clone_error(error));
                    }
                    // Consumed produce failures do not invoke Kotlin's exception handler.
                    result
                })
            }));
        Self {
            receiver,
            task,
            device: device.clone(),
            failure,
        }
    }
    pub async fn receive(&mut self, context: &VendorContext) -> VendorResult<Option<Vec<TestEvent>>> {
        // A closed channel is the end of the output; `take_failure` then tells whether it ended well.
        cancellable(context.cancellation.clone(), async { Ok(self.receiver.recv().await) }).await
    }
    /// D25: the failure that ended the producer, or the stream's end before a result, once the channel has closed. A
    /// client closed by provider shutdown is the end of the run, not a failure of this batch, and stays quiet (N19).
    pub fn take_failure(&self) -> Option<VendorError> {
        let failure = lock(&self.failure).take()?;
        (!self.device.client().socket_factory().is_closed()).then_some(failure)
    }
    pub async fn cancel_and_join(&mut self) {
        self.receiver.close();
        self.task.cancel(requested("instrumentation channel closed"));
        let _ = self.task.join().await;
    }
}
pub struct AndroidDeviceTestRunner<'a> {
    pub device: &'a AndroidDevice,
}
impl AndroidDeviceTestRunner<'_> {
    pub fn prepare_request(
        &self,
        configuration: &Configuration,
        info: &InstrumentationInfo,
        batch: &TestBatch,
    ) -> VendorResult<TestRunnerRequest> {
        let c = android_configuration(configuration)?;
        let tests: Vec<String> = batch
            .tests
            .iter()
            .map(|t| {
                let mut name = t.to_class_name('.');
                if t.method != "null" {
                    name.push('#');
                    name.push_str(&t.method.clone());
                }
                let mut escaped = String::default();
                for c in name.chars() {
                    let u = c as u32;
                    if u == 32 {
                        escaped.push('\\');
                    }
                    escaped.push(c);
                }
                escaped
            })
            .collect();
        self.device.log_named(
            "AndroidDeviceTestRunner",
            Level::Debug,
            &format!(
                "tests = {}",
                KotlinValue::List(tests.iter().cloned().map(KotlinValue::Text).collect())
                    .render()
                    .to_owned()
            ),
            None,
        );
        let mut request = TestRunnerRequest::new(
            info.instrumentation_package.clone(),
            InstrumentOptions {
                clazz: tests,
                coverage_file: configuration.is_code_coverage_enabled.then(|| {
                    format!(
                        "/data/data/{}/coverage/coverage-{}.ec",
                        info.application_package.to_owned(),
                        batch.id.to_owned()
                    )
                    .as_str()
                    .into()
                }),
                overrides: ArgumentsFactory { device: self.device }.generate(
                    configuration.is_code_coverage_enabled,
                    &c.instrumentation_args,
                    &c.test_access_configuration,
                ),
                ..Default::default()
            },
            self.device.supported_features(),
        );
        request.runner_class = info.test_runner_class.clone();
        request.no_window_animations = c.disable_window_animation;
        Ok(request)
    }
    async fn notify_ignored(
        &self,
        tests: &[Test],
        listener: &dyn AndroidTestRunListener,
        context: &VendorContext,
    ) -> VendorResult<()> {
        for test in tests {
            let id = TestIdentifier {
                class_name: test.to_class_name('.'),
                test_name: test.method.clone(),
            };
            listener
                .on_event_in(&TestEvent::TestStarted { id: id.clone() }, context.clone())
                .await?;
            listener
                .on_event_in(&TestEvent::TestIgnored { id: id.clone() }, context.clone())
                .await?;
            listener
                .on_event_in(
                    &TestEvent::TestEnded {
                        id,
                        metrics: MetricMap::new(),
                    },
                    context.clone(),
                )
                .await?;
        }
        Ok(())
    }
    async fn clear_data(
        &self,
        c: &Configuration,
        info: &InstrumentationInfo,
        context: VendorContext,
    ) -> VendorResult<()> {
        let a = android_configuration(c)?;
        for (clear, package) in [
            (a.application_pm_clear, &info.application_package),
            (a.test_application_pm_clear, &info.instrumentation_package),
        ] {
            if clear {
                let result = self.device.clear_package(&package.to_owned(), context.clone()).await?;
                self.device.log_named(
                    "AndroidDeviceTestRunner",
                    Level::Debug,
                    &format!(
                        "Package {} cleared: {}",
                        package.to_owned(),
                        trim(&result.output.to_owned())
                    ),
                    None,
                );
            }
        }
        if self
            .device
            .runtime()
            .file_sync
            .pull
            .iter()
            .any(|p| p.path_root == PathRoot::ExternalStorage)
        {
            let permission = if self.device.version().is_greater_or_equal_than(30) {
                Some("MANAGE_EXTERNAL_STORAGE")
            } else if self.device.api_level() == 29 {
                Some("LEGACY_STORAGE")
            } else {
                None
            };
            if let Some(permission) = permission {
                let result = self
                    .device
                    .critical_execute_shell_command(
                        &format!(
                            "appops set --uid {} {permission} allow",
                            info.application_package.to_owned()
                        ),
                        "",
                        context.clone(),
                    )
                    .await?;
                self.device.log_named(
                    "AndroidDeviceTestRunner",
                    Level::Debug,
                    &format!(
                        "File pull requested. Granted {permission} to {}: {}",
                        info.application_package.to_owned(),
                        trim(&result.output.to_owned())
                    ),
                    None,
                );
            }
        }
        if c.is_code_coverage_enabled {
            let package = info.application_package.to_owned();
            self.device
                .safe_execute_shell_command(
                    &format!("run-as {package} mkdir /data/data/{package}/coverage"),
                    "",
                    context,
                )
                .await?;
        }
        Ok(())
    }
    /// D25: the run failed, with the batch's unfinished tests on the usual retry path. The device is lost only if the
    /// same failure in a synchronous request would have made it so, so a parser exception cannot retire a healthy device.
    async fn on_producer_failure(
        &self,
        failure: VendorError,
        listener: &dyn AndroidTestRunListener,
        context: &VendorContext,
    ) -> VendorResult<()> {
        let mut message = String::from("Instrumentation failed: ");
        message.push_str(&failure.failure().to_java_string());
        self.device.log_named(
            "AndroidDeviceTestRunner",
            Level::Error,
            &message.to_owned(),
            Some(failure.failure()),
        );
        // Before the event: a test in progress has no result of its own, so this text is the only reason it can give.
        listener.on_stream_failed_in(&message, context.clone()).await?;
        listener
            .on_event_in(&TestEvent::TestRunFailed { error: message }, context.clone())
            .await?;
        if means_device_loss(&failure) {
            return Err(failure);
        }
        Ok(())
    }
    pub async fn execute(
        &self,
        configuration: Arc<Configuration>,
        raw: BatchRef,
        listener: &dyn AndroidTestRunListener,
        context: VendorContext,
    ) -> VendorResult<()> {
        let ignored: Vec<_> = raw.tests.iter().filter(|t| is_ignored(t)).cloned().collect();
        let batch = TestBatch {
            // Kotlin list subtraction uses Test equality, which excludes metadata.
            tests: raw.tests.iter().filter(|t| !ignored.contains(t)).cloned().collect(),
            id: raw.id.clone(),
        };
        if batch.tests.is_empty() {
            listener.before_test_run_in(None, context.clone()).await?;
            self.notify_ignored(&ignored, listener, &context).await?;
            listener
                .on_event_in(
                    &TestEvent::TestRunEnded {
                        elapsed_time_millis: 0,
                        metrics: MetricMap::new(),
                    },
                    context.clone(),
                )
                .await?;
            return listener.after_test_run_in(context).await;
        }
        // D28: each instrumentation runs only its own tests, in first-seen order; Kotlin gives each of them the whole
        // batch and repeats the ignored callbacks and afterTestRun (N35). testBatchTimeout and testOutputTimeout apply to
        // each instrumentation as they do to one, and each clears its own packages' data first. One listener collects
        // the whole batch, so it sees one beforeTestRun, one afterTestRun and, in between, a run per instrumentation.
        let mut partitions: Vec<(InstrumentationInfo, Vec<Test>)> = vec![];
        for test in &batch.tests {
            let bundle = self.device.runtime().identifier.android_bundle(test)?;
            let info = blocking(self.device.runtime().dependencies.io.as_ref(), move || {
                bundle.instrumentation_info()
            })
            .await?;
            match partitions.iter_mut().find(|(known, _)| *known == info) {
                Some((_, tests)) => tests.push(test.clone()),
                None => partitions.push((info, vec![test.clone()])),
            }
        }
        let count = partitions.len();
        // An instrumentation can time out before it gets as far as beforeTestRun, which the next one then owes.
        let mut announced = false;
        for (index, (info, tests)) in partitions.into_iter().enumerate() {
            let first = index == 0;
            let batch = TestBatch {
                tests,
                id: batch.id.clone(),
            };
            let request = self.prepare_request(&configuration, &info, &batch)?;
            if count > 1 {
                listener
                    .on_instrumentation_tests_in(&batch.tests, context.clone())
                    .await?;
            }
            let mut channel = None;
            let result = timed(&context, configuration.test_batch_timeout_millis, |signal| {
                let channel = &mut channel;
                let announced = &mut announced;
                let info = &info;
                let ignored = &ignored;
                let configuration = &configuration;
                let child = VendorContext {
                    cancellation: signal,
                    ..context.clone()
                };
                Box::pin(async move {
                    if first {
                        self.notify_ignored(ignored, listener, &child).await?;
                    }
                    self.clear_data(configuration, info, child.clone()).await?;
                    if *announced {
                        listener.before_next_instrumentation_in(info, child.clone()).await?;
                    } else {
                        listener.before_test_run_in(Some(info), child.clone()).await?;
                        *announced = true;
                    }
                    self.device.log_named(
                        "AndroidDeviceTestRunner",
                        Level::Debug,
                        &format!("Running {}", request.command().to_owned()),
                        None,
                    );
                    *channel = Some(InstrumentationChannel::start(self.device, request));
                    if let Some(channel) = channel.as_mut() {
                        loop {
                            let update = timed(&child, configuration.test_output_timeout_millis, |signal| {
                                let child = VendorContext {
                                    cancellation: signal,
                                    ..child.clone()
                                };
                                {
                                    let receiver = &mut *channel;
                                    Box::pin(async move { receiver.receive(&child).await })
                                }
                            })
                            .await?;
                            match update {
                                None => {
                                    listener
                                        .on_event_in(
                                            &TestEvent::TestRunFailed {
                                                error: ERROR_STUCK.into(),
                                            },
                                            child.clone(),
                                        )
                                        .await?;
                                    return Ok(());
                                }
                                Some(None) => break,
                                Some(Some(events)) => {
                                    for event in events {
                                        listener.on_event_in(&event, child.clone()).await?;
                                    }
                                }
                            }
                        }
                    }
                    self.device
                        .log_named("AndroidDeviceTestRunner", Level::Debug, "Execution finished", None);
                    Ok(())
                })
            })
            .await;
            let result = match result {
                Ok(None) => {
                    listener
                        .on_event_in(
                            &TestEvent::TestRunFailed {
                                error: ERROR_STUCK.into(),
                            },
                            context.clone(),
                        )
                        .await
                }
                Ok(Some(())) => match channel.as_ref().and_then(InstrumentationChannel::take_failure) {
                    Some(failure) => self.on_producer_failure(failure, listener, &context).await,
                    None => Ok(()),
                },
                Err(error) if is_io_exception(&error) => {
                    let mut message = String::from("adb error while running tests ");
                    message.push_str(
                        &KotlinValue::List(
                            batch
                                .tests
                                .iter()
                                .map(|t| KotlinValue::Text(t.to_test_name('.', '#')))
                                .collect(),
                        )
                        .render(),
                    );
                    self.device.log_named(
                        "AndroidDeviceTestRunner",
                        Level::Error,
                        &message.to_owned(),
                        Some(error.failure()),
                    );
                    listener
                        .on_event_in(&TestEvent::TestRunFailed { error: message }, context.clone())
                        .await
                }
                Err(e) => Err(e),
            };
            // A run failure leaves the later instrumentations to run. An error, a lost device or a cancellation does
            // not, and their tests are then the uncompleted ones of the batch's single result.
            if result.is_ok() && index + 1 < count {
                if let Some(channel) = &mut channel {
                    channel.cancel_and_join().await;
                }
                continue;
            }
            let after = listener.after_test_run_in(context.clone()).await;
            if let Some(channel) = &mut channel {
                channel.cancel_and_join().await;
            }
            after?;
            result?;
            break;
        }
        Ok(())
    }
}
fn android_configuration(
    configuration: &Configuration,
) -> VendorResult<&marathon_config::vendor::AndroidConfiguration> {
    match &configuration.vendor_configuration {
        marathon_config::VendorConfiguration::Android(configuration) => Ok(configuration),
        _ => Err(other(FailureKind::ClassCast, "Android configuration required")),
    }
}
/// Not a device loss by itself: a device that really went is reported by its server's device list, as ever.
fn stream_ended_early(exit_code: Option<i32>) -> VendorError {
    let message = match exit_code {
        Some(code) => format!("the instrumentation stream ended before a result, with exit code {code}"),
        None => "the instrumentation stream ended before a result".to_owned(),
    };
    other(FailureKind::Eof, message.as_str().to_owned())
}
fn clone_error(error: &VendorError) -> VendorError {
    VendorError::Other(error.failure().clone())
}
fn is_io_exception(error: &VendorError) -> bool {
    [
        FailureKind::Io,
        FailureKind::FileNotFound,
        FailureKind::Eof,
        FailureKind::InterruptedIo,
        FailureKind::Socket,
        FailureKind::SocketTimeout,
        FailureKind::NoSuchFile,
        FailureKind::AccessDenied,
        FailureKind::FileAlreadyExists,
        FailureKind::FileSystem,
        FailureKind::Adb(AdbKind::ConnectTimeout),
        FailureKind::Connect,
        FailureKind::Adb(AdbKind::ConnectRefused),
        FailureKind::Adb(AdbKind::ClosedChannel),
    ]
    .iter()
    .any(|s| error.failure().kind == (*s))
        && !matches!(error, VendorError::Cancelled(_))
}
#[derive(Default)]
pub struct AndroidRunnerExecutor {
    pub slots: crate::listener_factory::ListenerSlots,
}
impl AndroidBatchExecutor for AndroidRunnerExecutor {
    fn execute<'a>(
        &'a self,
        device: &'a AndroidDevice,
        configuration: Arc<Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<dyn Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let listener =
                crate::listener_factory::create(device, &configuration, pool, batch.clone(), result, &self.slots)
                    .map_err(execution_error)?;
            let owned = device.clone();
            let (mut sender, receiver) = tokio::sync::oneshot::channel();
            // The task retains the cleanup owner; closing the waiter cancels the body.
            let task = device.runtime().dependencies.io.spawn(Box::pin(async move {
                let dropped = marathon_core::executor::Signal::new();
                let context = VendorContext {
                    cancellation: Arc::new(crate::artifact_listeners::ChildSignal(
                        dropped.clone(),
                        context.cancellation,
                    )),
                    ..context
                };
                let runner = AndroidDeviceTestRunner { device: &owned };
                let run = runner.execute(configuration, batch, &listener, context.clone());
                tokio::pin!(run);
                let outcome = tokio::select! {
                    outcome = &mut run => outcome,
                    _ = sender.closed() => { dropped.cancel(requested("execute waiter dropped")); run.await }
                };
                let cleanup = listener
                    .cleanup_in(VendorContext {
                        cancellation: marathon_core::executor::Signal::new(),
                        ..context
                    })
                    .await;
                if let Err(error) = &cleanup {
                    owned.log_named(
                        "AndroidDeviceTestRunner",
                        Level::Error,
                        "Artifact listener cleanup failed",
                        Some(error.failure()),
                    );
                }
                let _ = sender.send(outcome.and(cleanup));
                Ok(())
            }));
            let outcome = receiver
                .await
                .map_err(|_| other(FailureKind::IllegalState, "batch task ended without outcome"))?;
            task.join().await?;
            outcome.map_err(execution_error)
        })
    }
}

#[cfg(test)]
mod kind_tests {
    use super::*;
    #[test]
    fn runner_io_retries_keep_the_explicit_set_and_exclude_cancellation() {
        for kind in [
            FailureKind::Io,
            FailureKind::FileNotFound,
            FailureKind::Eof,
            FailureKind::InterruptedIo,
            FailureKind::Socket,
            FailureKind::SocketTimeout,
            FailureKind::NoSuchFile,
            FailureKind::AccessDenied,
            FailureKind::FileAlreadyExists,
            FailureKind::FileSystem,
            FailureKind::Adb(AdbKind::ConnectTimeout),
            FailureKind::Connect,
            FailureKind::Adb(AdbKind::ConnectRefused),
            FailureKind::Adb(AdbKind::ClosedChannel),
        ] {
            assert!(is_io_exception(&VendorError::Other(failure(kind.clone(), "test"))));
            assert!(!is_io_exception(&VendorError::Cancelled(Cancellation::Requested(
                failure(kind, "test")
            ))));
        }
        for kind in [
            FailureKind::Zip,
            FailureKind::UncheckedIo,
            FailureKind::Timeout,
            FailureKind::Runtime,
        ] {
            assert!(!is_io_exception(&VendorError::Other(failure(kind, "test"))));
        }
    }
}
