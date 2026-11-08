//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/listener/TestResultsListener.kt
use super::AccumulatingTestResultListener;
use super::AppleTestRunListener;
use crate::cmd::remote::ssh::sshj::config::SshLogging;
use crate::logparser::parser::DeviceFailureReason;
use crate::{
    bin::{
        kt,
        xcrun::xcresulttool::{ResultBundleFormat, Xcresulttool},
    },
    model::XcodeVersion,
    xcresult::{ActionTestPlanRunSummaries, ActionsInvocationRecord},
};
use jcompat::logging::{Level, Logger};

use marathon_core::{
    artifacts::AttachmentCollector,
    error::VendorResult,
    execution::result::TestRunResultsAccumulator,
    listener::{AttachmentListener, AttachmentProvider},
    logging::{LogSender, WallSource},
    model::{Attachment, TestBatch, TestBatchResults, TestResult, TestStatus},
    runtime::Deferred,
    time::Timer,
    vendor::{DeviceRef, VendorContext},
};
use marathon_core::{model::Test, runtime::VendorFuture};
use std::sync::Arc;

pub struct TestResultsListenerConfig {
    pub batch: Arc<TestBatch>,
    pub device: DeviceRef,
    pub deferred: Arc<dyn Deferred<TestBatchResults>>,
    pub timer: Arc<dyn Timer>,
    pub wall: WallSource,
    pub logs: LogSender,
    pub remote_xcresult: String,
    pub xcresulttool: Arc<Xcresulttool>,
    pub xcode_version: Option<XcodeVersion>,
    pub context: VendorContext,
    pub logging: SshLogging,
    pub attachment_providers: Vec<Arc<dyn AttachmentProvider>>,
}
pub struct TestResultsListener {
    pub accumulator: AccumulatingTestResultListener,
    config: TestResultsListenerConfig,
    attachments: Arc<AttachmentCollector>,
    logger: Logger,
}
impl TestResultsListener {
    pub fn new(config: TestResultsListenerConfig) -> VendorResult<Self> {
        let result = TestRunResultsAccumulator::new(config.timer.clone(), config.wall.clone()).map_err(|e| {
            marathon_core::error::VendorError::Other(crate::cmd::failure("ReplayError", &e.message))
        })?;
        let accumulator =
            AccumulatingTestResultListener::new(config.batch.tests.len() as i32, result, config.logs.clone());
        let attachments = AttachmentCollector::new(&config.attachment_providers)?;
        let logger = config
            .logging
            .logger("com.malinskiy.marathon.apple.listener.TestResultsListener");
        Ok(Self {
            accumulator,
            config,
            attachments,
            logger,
        })
    }
    fn warn(&self, message: &str) {
        self.config
            .logging
            .log(&self.logger, Level::Warn, || message.into(), None);
    }
    async fn enhance_results(&self) -> VendorResult<()> {
        let started = (self.config.wall)();
        self.enhance().await?;
        let ended = (self.config.wall)();
        self.config.logging.log(
            &self.logger,
            Level::Debug,
            || {
                format!(
                    "Enhancing report using xcresult took {} ms",
                    ended.wrapping_sub(started)
                )
            },
            None,
        );
        Ok(())
    }
    async fn enhance(&self) -> VendorResult<()> {
        let c = &self.config;
        let Some(record) = c
            .xcresulttool
            .get::<ActionsInvocationRecord>(
                &c.remote_xcresult,
                c.xcode_version,
                ResultBundleFormat::Json,
                None,
                None,
                &c.context,
            )
            .await?
        else {
            return Ok(());
        };
        if record.issues.test_failure_summaries.is_empty() {
            return Ok(());
        }
        let mut plan_summaries = Vec::new();
        for action in record.actions {
            let action = nonnull(action)?;
            if let Some(reference) = action.action_result.tests_ref {
                let id = reference.id.to_owned();
                if let Some(summaries) = c
                    .xcresulttool
                    .get::<ActionTestPlanRunSummaries>(
                        &c.remote_xcresult,
                        c.xcode_version,
                        ResultBundleFormat::Json,
                        Some(&id),
                        None,
                        &c.context,
                    )
                    .await?
                {
                    plan_summaries.extend(summaries.summaries);
                }
            }
        }
        let mut targets = Vec::new();
        for summary in plan_summaries {
            let summary = nonnull(summary)?;
            for testable in summary.testable_summaries {
                let testable = nonnull(testable)?;
                if let Some(name) = testable.target_name
                    && !targets.contains(&name)
                {
                    targets.push(name);
                }
            }
        }
        let pkg = match targets.len() {
            1 => targets.first(),
            0 => {
                self.warn("No testable targets found in xcresult");
                None
            }
            _ => {
                self.warn("Multiple testing targets found in xcresult");
                None
            }
        };
        let completed = self.accumulator.update(|a| Ok(a.completed_tests()))?;
        let mut traces: Vec<(Test, String)> = vec![];
        for summary in record.issues.test_failure_summaries {
            if completed.is_empty() {
                self.warn("No matching test cases found in xcresult");
                continue;
            }
            let summary = nonnull(summary)?;
            let name = summary.test_case_name.to_owned();
            let name = name.strip_suffix("()").unwrap_or(&name);
            let matches: Vec<_> = completed
                .iter()
                .filter(|t| pkg.is_none_or(|pkg| t.pkg == *pkg) && t.to_simple_safe_test_name('.') == name)
                .collect();
            match matches.as_slice() {
                [test] => {
                    let index = traces.iter().position(|(t, _)| t == *test).unwrap_or_else(|| {
                        traces.push(((*test).clone(), String::new()));
                        traces.len() - 1
                    });
                    let trace = &mut traces[index].1;
                    trace.push_str(&summary.message.to_owned());
                    trace.push('\n');
                    if let Some(location) = summary.document_location_in_creating_workspace {
                        let url = location.url.to_owned();
                        if !kt::is_blank(&url) {
                            trace.push_str(&url);
                            trace.push('\n');
                        }
                    }
                }
                [] => self.warn("No matching test cases found in xcresult"),
                _ => {
                    self.warn(concat!(
                        "Multiple matching test cases found in xcresult. ",
                        "Impossible to possible to add stacktrace to report"
                    ));
                }
            }
        }
        for (test, trace) in traces {
            self.accumulator
                .update(|a| a.test_failed(&test, &kt::trim(&trace).into(), None, None))?;
        }
        Ok(())
    }
    fn results(&self) -> VendorResult<TestBatchResults> {
        let c = &self.config;
        self.accumulator.update(|a| {
            let mut finished = vec![];
            let mut failed = vec![];
            let mut uncompleted = vec![];
            for (test, value) in a.temporal_test_results.entries() {
                // A6: the Kotlin lookup compares the batch class with the parsed package.
                let batch_test = c
                    .batch
                    .tests
                    .iter()
                    .find(|t| t.pkg == test.pkg && t.clazz == test.pkg && t.method == test.method);
                let result = TestResult {
                    test: batch_test.unwrap_or(test).clone(),
                    device: c.device.0.info(),
                    test_batch_id: c.batch.id.clone(),
                    status: value.status,
                    start_time: value.start_time,
                    end_time: value.end_time,
                    stacktrace: value.stack_trace.clone(),
                    attachments: self.attachments.get(test).unwrap_or_default(),
                };
                let successful = a
                    .temporal_test_results
                    .get(&result.test)
                    .is_some_and(|r| r.is_successful());
                if successful {
                    finished.push(result.clone());
                }
                if result.status == TestStatus::Incomplete {
                    uncompleted.push(result);
                } else if !successful {
                    failed.push(result);
                }
            }
            let missing: Vec<_> = c
                .batch
                .tests
                .iter()
                .filter(|t| a.temporal_test_results.get(t).is_none())
                .cloned()
                .collect();
            uncompleted.extend(a.create_uncompleted_test_results(&missing, &c.batch, &c.device.0.info())?);
            Ok(TestBatchResults {
                device: c.device.clone(),
                finished,
                failed,
                uncompleted,
            })
        })
    }
}
impl AttachmentListener for TestResultsListener {
    fn on_attachment(&self, test: &Test, attachment: Attachment) -> VendorResult<()> {
        self.attachments.on_attachment(test, attachment)
    }
}
impl AppleTestRunListener for TestResultsListener {
    fn test_run_started<'a>(&'a self) -> VendorFuture<'a, ()> {
        self.accumulator.test_run_started()
    }
    fn test_started<'a>(&'a self, test: &'a Test) -> VendorFuture<'a, ()> {
        self.accumulator.test_started(test)
    }
    fn test_failed<'a>(
        &'a self,
        test: &'a Test,
        start_time: i64,
        end_time: i64,
        trace: Option<&'a String>,
    ) -> VendorFuture<'a, ()> {
        self.accumulator.test_failed(test, start_time, end_time, trace)
    }
    fn test_passed<'a>(&'a self, test: &'a Test, start_time: i64, end_time: i64) -> VendorFuture<'a, ()> {
        self.accumulator.test_passed(test, start_time, end_time)
    }
    fn test_ignored<'a>(&'a self, test: &'a Test, start_time: i64, end_time: i64) -> VendorFuture<'a, ()> {
        self.accumulator.test_ignored(test, start_time, end_time)
    }
    fn test_run_ended<'a>(&'a self) -> VendorFuture<'a, ()> {
        self.accumulator.test_run_ended()
    }
    fn test_run_failed<'a>(&'a self, error_message: &'a String, reason: DeviceFailureReason) -> VendorFuture<'a, ()> {
        self.accumulator.test_run_failed(error_message, reason)
    }
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.enhance_results().await?;
            let results = self.results()?;
            for result in &results.uncompleted {
                self.warn(&format!(
                    "uncompleted = {}, {}",
                    result.test.to_test_name('.', '#').to_owned(),
                    self.config.device.0.info().serial_number.to_owned()
                ));
            }
            self.config.deferred.complete(Ok(results));
            Ok(())
        })
    }
}

fn nonnull<T>(value: Option<T>) -> VendorResult<T> {
    value.ok_or_else(|| {
        marathon_core::error::VendorError::Other(crate::cmd::failure("java.lang.NullPointerException", ""))
    })
}
