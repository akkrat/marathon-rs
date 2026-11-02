//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/AppleDeviceTestRunner.kt
use crate::{
    apple_device::AppleDevice,
    cmd::{failure, remote::ssh::sshj::config::SshLogging},
    listener::AppleTestRunListener,
    logparser::parser::{DeviceFailureException, DeviceFailureReason},
    model::AppleTestBundle,
    test::{TestEvent, TestRequest},
    xcodebuild_session::{CLOSE_TIMEOUT, XcodebuildSession, device_lost},
};
use jcompat::{
    jtime::Duration,
    logging::{Level, Logger},
    throwable::is_error_class,
    writers::{JavaString, KotlinValue},
};
use marathon_config::Configuration;
use marathon_core::{
    error::{Failure, VendorError, VendorResult},
    model::TestBatch,
    runtime::VendorRuntime,
    vendor::VendorContext,
};
use std::sync::Arc;

/// Suppressed throwables remain Apple-local. Flatten only at the core/log boundary.
#[derive(Clone, Debug)]
pub struct RunError {
    pub error: VendorError,
    pub reason: Option<DeviceFailureReason>,
    pub suppressed: Box<[Failure]>,
}
impl From<VendorError> for RunError {
    fn from(error: VendorError) -> Self {
        Self {
            error,
            reason: None,
            suppressed: Box::default(),
        }
    }
}
impl RunError {
    pub fn into_vendor(mut self) -> VendorError {
        let detail = match &mut self.error {
            VendorError::Cancelled(reason) => match reason {
                marathon_core::error::Cancellation::Requested(f)
                | marathon_core::error::Cancellation::Parent(f)
                | marathon_core::error::Cancellation::Timeout { detail: f, .. } => f,
            },
            VendorError::DeviceLost(f) => match f.cause {
                Some(ref mut cause) => cause.as_mut(),
                None => f,
            },
            VendorError::Other(f)
            | VendorError::BatchExecution(f)
            | VendorError::DeviceSetup(f)
            | VendorError::IncompatibleDevice(f)
            | VendorError::TestParsing(f)
            | VendorError::NoDevices(f)
            | VendorError::Transfer(f)
            | VendorError::LeavePool { detail: f, .. } => f,
        };
        flatten_suppressed(detail, &self.suppressed);
        self.error
    }
}
fn flatten_suppressed(detail: &mut Failure, suppressed: &[Failure]) {
    if suppressed.is_empty() {
        return;
    }
    let text = detail.stacktrace.get_or_insert_with(JavaString::default);
    for failure in suppressed {
        text.push_str("\tSuppressed: ");
        let trace = failure.stack_trace();
        // JVM frames are omitted by PLAN §3.10; retain nested causes with their indentation.
        let mut lines = trace
            .to_string_lossy()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .into_iter();
        if let Some(line) = lines.next() {
            text.push_str(&line);
            text.push_str("\n");
        }
        for line in lines {
            text.push_str("\t");
            text.push_str(&line);
            text.push_str("\n");
        }
    }
}
pub struct AppleDeviceTestRunner {
    pub device: Arc<dyn AppleDevice>,
    pub runtime: Arc<dyn VendorRuntime>,
    pub logging: SshLogging,
    pub session_close_timeout: Duration,
    logger: Logger,
}
impl AppleDeviceTestRunner {
    pub fn new(device: Arc<dyn AppleDevice>, runtime: Arc<dyn VendorRuntime>, logging: SshLogging) -> Self {
        let logger = logging.logger("com.malinskiy.marathon.apple.AppleDeviceTestRunner");
        Self {
            device,
            runtime,
            logging,
            session_close_timeout: CLOSE_TIMEOUT,
            logger,
        }
    }
    fn log(&self, level: Level, message: impl Into<String>, cause: Option<Failure>) {
        if self.logging.enabled(&self.logger, level) {
            (self.logging.sink)(&self.logger, level, message.into(), cause);
        }
    }
    pub async fn execute(
        &self,
        configuration: &Configuration,
        bundle: &AppleTestBundle,
        batch: &TestBatch,
        listener: &dyn AppleTestRunListener,
        context: &VendorContext,
    ) -> Result<(), RunError> {
        let fm = self.device.remote_file_manager();
        let remote = fm.remote_xctestrun_file();
        self.log(Level::Debug, format!("Remote xctestrun = {remote}"), None);
        let tests = batch
            .tests
            .iter()
            .map(|t| t.kotlin_value(&*context.hooks))
            .collect::<VendorResult<Vec<_>>>()?;
        self.log(
            Level::Debug,
            format!("Tests = {}", KotlinValue::List(tests).render().to_string_lossy()),
            None,
        );
        let mut request = TestRequest::new(
            fm.parent_of(&remote).to_owned(),
            remote,
            configuration.is_code_coverage_enabled,
        );
        request.tests = Some(batch.tests.clone());
        request.xcresult = Some(fm.remote_xcresult_file(batch).to_string_lossy());
        request.test_target_name = Some(bundle.test_bundle_id().into());
        self.execute_request(request, &batch.tests, listener, context).await
    }
    /// Also permits unit-level lifecycle probes without constructing on-disk bundles.
    pub async fn execute_request(
        &self,
        request: TestRequest,
        tests: &[marathon_core::model::Test],
        listener: &dyn AppleTestRunListener,
        context: &VendorContext,
    ) -> Result<(), RunError> {
        let session = XcodebuildSession::new(self.runtime.clone(), self.logging.clone());
        let mut channel = None;
        let mut result: Result<(), RunError> = async {
            listener.before_test_run().await?;
            channel = Some(
                self.device
                    .execute_test_request(request, session.clone(), context)
                    .await?,
            );
            if let Some(channel) = &channel {
                while let Some(events) = channel.receive(context).await? {
                    process_events(&events, listener).await?;
                }
            }
            self.log(Level::Debug, "Execution finished", None);
            Ok(())
        }
        .await;
        let mut run_failure = result.as_ref().err().cloned();
        if let Err(error) = &result
            && matches!(error.error, VendorError::Cancelled(_))
        {
            let names = KotlinValue::List(
                tests
                    .iter()
                    .map(|t| KotlinValue::Text(t.to_test_name('.', '#')))
                    .collect(),
            )
            .render();
            let message = JavaString::from(format!(
                "Tests {} got stuck. You can increase the timeout in settings if it's too strict",
                names.to_string_lossy()
            ));
            self.log(
                Level::Error,
                message.to_string_lossy(),
                Some(error.error.failure().clone()),
            );
            if let Err(error) = listener.test_run_failed(&message, DeviceFailureReason::Unknown).await {
                result = Err(error.into());
                run_failure = None;
            }
        }
        // Explicit finally: cancellation never drops either artifact collection or stop.
        if let Err(after) = listener.after_test_run().await {
            if is_error_class(&after.failure().class.to_string_lossy()) {
                // Kotlin catches Exception here; an Error replaces propagation without changing runFailure.
                result = Err(after.into());
            } else if let Some(error) = &mut run_failure {
                error.suppressed = error
                    .suppressed
                    .iter()
                    .cloned()
                    .chain(std::iter::once(after.failure().clone()))
                    .collect();
                if let Err(result) = &mut result {
                    result.suppressed = error.suppressed.clone();
                }
            } else {
                let error = RunError::from(after);
                run_failure = Some(error.clone());
                result = Err(error);
            }
        }
        if let Some(channel) = channel {
            channel.cancel();
        }
        let started = context.hooks.nanos("apple.AppleDeviceTestRunner.closeSession");
        if let Some(close) = session.stop(self.session_close_timeout, context).await {
            let serial = self.device.info().serial_number.to_string_lossy();
            let mut detail = failure(
                "java.lang.IllegalStateException",
                format!("xcodebuild session on {serial} has not been closed"),
            );
            detail.cause = Some(Box::new(close));
            if run_failure
                .as_ref()
                .is_some_and(|e| matches!(e.error, VendorError::Cancelled(_)))
            {
                self.log(
                    Level::Warn,
                    "Cancelled run left its xcodebuild session unclosed",
                    Some(detail),
                );
            } else {
                let suppressed = run_failure
                    .map(|e| vec![e.into_vendor().failure().clone()])
                    .unwrap_or_default();
                return Err(RunError {
                    error: device_lost(detail),
                    reason: None,
                    suppressed: suppressed.into(),
                });
            }
        } else if self.logging.enabled(&self.logger, Level::Debug) {
            let clock_error = |e: marathon_core::time::ReplayError| {
                RunError::from(VendorError::Other(failure("ReplayError", e.message.to_string_lossy())))
            };
            let elapsed = context
                .hooks
                .nanos("apple.AppleDeviceTestRunner.closeSession")
                .map_err(clock_error)?
                .wrapping_sub(started.map_err(clock_error)?)
                / 1_000_000;
            self.log(
                Level::Debug,
                format!(
                    "xcodebuild session on {} closed in {elapsed} ms",
                    self.device.info().serial_number.to_string_lossy()
                ),
                None,
            );
        }
        result
    }
}
pub async fn process_events(events: &[TestEvent], listener: &dyn AppleTestRunListener) -> Result<(), RunError> {
    for event in events {
        match event {
            TestEvent::TestRunStartedEvent => listener.test_run_started().await?,
            TestEvent::TestStarted { id } => listener.test_started(id).await?,
            TestEvent::TestPassed {
                id,
                start_time,
                end_time,
            } => listener.test_passed(id, *start_time, *end_time).await?,
            TestEvent::TestFailed {
                id,
                start_time,
                end_time,
                trace,
            } => listener.test_failed(id, *start_time, *end_time, trace.as_ref()).await?,
            TestEvent::TestIgnored {
                id,
                start_time,
                end_time,
            } => listener.test_ignored(id, *start_time, *end_time).await?,
            TestEvent::TestRunEnded => listener.test_run_ended().await?,
            TestEvent::TestRunFailed { message, reason } => {
                listener.test_run_failed(message, *reason).await?;
                return Err(RunError {
                    error: VendorError::Other(
                        DeviceFailureException::new(*reason, Some(message.to_string_lossy()), None).to_failure(),
                    ),
                    reason: Some(*reason),
                    suppressed: Box::default(),
                });
            }
        }
    }
    Ok(())
}
